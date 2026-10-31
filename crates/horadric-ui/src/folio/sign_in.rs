//! Logging in to Claude and out from the Folio, no terminal: `claude auth
//! login` and `claude auth logout` run hidden, their output read into the
//! sign-in card ([`horadric_core::folio::SignIn`], where what the CLI says
//! is parsed). The CLI writes its own login files as it always does, so
//! the tray's accounts see a new login as they see one made in a console.
//!
//! One login runs at a time, for one card: a new one ends the last. Once
//! it succeeds, the session it was for, and any other whose turn failed
//! for want of it, start again on their conversations with the new login.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use horadric_core::agent::Agent;
use horadric_core::folio::{AuthStatus, Block, Heard, SignIn, SignInState};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SW_SHOWNORMAL};

use super::{cards, redraw, with, Chat, CHATS};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Posted to the router when a login or logout has said something.
pub(super) const WM_FOLIO_LOGIN: u32 = windows::Win32::UI::WindowsAndMessaging::WM_USER + 11;
/// How often the waiter looks whether the CLI has ended.
const POLL: Duration = Duration::from_millis(250);

/// What a run has said, filled by its threads.
#[derive(Debug, Clone, Default)]
struct Said {
    out: String,
    err: String,
    /// Its exit code once it ended, every word of it read by then.
    exit: Option<i32>,
    /// Who is logged in after a login that succeeded.
    status: Option<AuthStatus>,
}

struct Run {
    id: u64,
    /// The session whose card it serves.
    chat: String,
    stdin: Option<ChildStdin>,
    child: Arc<Mutex<Child>>,
    said: Arc<Mutex<Said>>,
}

impl Drop for Run {
    fn drop(&mut self) {
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
        }
    }
}

thread_local! {
    static RUN: RefCell<Option<Run>> = const { RefCell::new(None) };
    static NEXT: Cell<u64> = const { Cell::new(1) };
    /// What a session restarted after a login says first, by session.
    static GREETINGS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

/// What logouts said, by the session that asked, for the UI thread.
static LOGOUTS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

pub(super) fn post(router: isize) {
    unsafe {
        let _ = PostMessageW(
            Some(HWND(router as *mut c_void)),
            WM_FOLIO_LOGIN,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

/// `claude.exe` with `args`, on pipes, with no window, and with the
/// environment a headless session gets, so it logs in where they look.
pub(super) fn command(args: &[&str]) -> Result<Command, String> {
    if let Some(why) = crate::accounts::refused(Agent::Claude) {
        return Err(why);
    }
    let exe = horadric_pty::headless::claude_exe().ok_or("claude.exe was not found")?;
    let mut c = Command::new(exe);
    c.args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let inherited = crate::console::inherited_env(crate::console::under_claude());
    let tags = [horadric_hooks::SESSION_ENV, horadric_hooks::OWNER_ENV];
    for name in (horadric_pty::headless::ENV_REMOVE.iter())
        .chain(&inherited)
        .chain(&tags)
    {
        c.env_remove(name);
    }
    Ok(c)
}

/// Reads `from` into `said` as it comes, not by line: the CLI's prompt for
/// a code has no line end.
fn reader(
    mut from: impl Read + Send + 'static,
    said: Arc<Mutex<Said>>,
    err: bool,
    router: isize,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = from.read(&mut buf) {
            if n == 0 {
                break;
            }
            let text = String::from_utf8_lossy(&buf[..n]);
            if let Ok(mut s) = said.lock() {
                if err { &mut s.err } else { &mut s.out }.push_str(&text);
            }
            post(router);
        }
    })
}

/// The output of `claude auth status --json`, when it says.
fn status() -> Option<AuthStatus> {
    let out = command(&["auth", "status", "--json"])
        .ok()?
        .stdin(Stdio::null())
        .output()
        .ok()?;
    AuthStatus::read(&String::from_utf8_lossy(&out.stdout))
}

fn spawn(id: u64, chat: &str) -> Result<Run, String> {
    let router = super::router();
    let mut child = command(&["auth", "login"])?
        .spawn()
        .map_err(|e| format!("claude auth login did not start: {e}"))?;
    let said: Arc<Mutex<Said>> = Arc::default();
    let readers = [
        child.stdout.take().map(|o| reader(o, Arc::clone(&said), false, router)),
        child.stderr.take().map(|e| reader(e, Arc::clone(&said), true, router)),
    ];
    let stdin = child.stdin.take();
    let child = Arc::new(Mutex::new(child));
    let (waited, told) = (Arc::clone(&child), Arc::clone(&said));
    std::thread::spawn(move || {
        let code = loop {
            match waited.lock().map(|mut c| c.try_wait()) {
                Ok(Ok(Some(s))) => break s.code().unwrap_or(-1),
                Ok(Ok(None)) => {}
                _ => break -1,
            }
            std::thread::sleep(POLL);
        };
        for r in readers.into_iter().flatten() {
            let _ = r.join();
        }
        let who = (code == 0).then(status).flatten();
        if let Ok(mut s) = told.lock() {
            s.status = who;
            s.exit = Some(code);
        }
        post(router);
    });
    Ok(Run {
        id,
        chat: chat.to_string(),
        stdin,
        child,
        said,
    })
}

/// The card at block `block`, if it is one.
fn card(chat: &mut Chat, block: usize) -> Option<&mut SignIn> {
    match chat.folio.blocks.get_mut(block) {
        Some(Block::SignIn(c)) => Some(c),
        _ => None,
    }
}

/// Starts a login for session `id`'s card at `block`, or for a new card
/// (`/login` typed) when None. Any login still running ends.
pub(super) fn log_in(chat: &mut Chat, id: &str, block: Option<usize>) {
    let run = NEXT.with(|n| n.replace(n.get() + 1));
    RUN.with(|r| r.borrow_mut().take());
    match block.and_then(|b| card(chat, b)) {
        Some(c) => c.begin(run),
        None => chat.folio.sign_in_started(run),
    }
    chat.follow = true;
    match spawn(run, id) {
        Ok(r) => RUN.with(|slot| *slot.borrow_mut() = Some(r)),
        Err(why) => {
            if let Some(c) = (chat.folio.blocks.iter_mut().rev()).find_map(|b| match b {
                Block::SignIn(c) if c.run == run => Some(c),
                _ => None,
            }) {
                c.state = SignInState::Failed(why);
            }
        }
    }
    // Other cards a login ran for are told it ended.
    post(super::router());
}

/// The card's "Log in" or "Try again".
pub(super) fn log_in_at(id: &str, block: usize) {
    with(id, |c| log_in(c, id, Some(block)));
    redraw(id);
}

/// Opens the card's sign-in page in the default browser, not the browser
/// pane: the login is the human's own, with the browser that holds it.
pub(super) fn open_page(id: &str, block: usize) {
    let url = with(id, |c| card(c, block).and_then(|c| c.url.clone())).flatten();
    if let Some(url) = url.filter(|u| u.starts_with("https://")) {
        unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                &HSTRING::from(url),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
        }
    }
}

/// Sends the code typed in the card's field to its login.
pub(super) fn send_code(id: &str, block: usize) {
    with(id, |c| {
        let field = cards::code_field(block);
        let code = c.tr.state.text(&field).trim().to_string();
        let Some(card) = card(c, block).filter(|k| k.active()) else {
            return;
        };
        if code.is_empty() {
            return;
        }
        let run = card.run;
        let sent = RUN.with(|r| {
            let mut r = r.borrow_mut();
            let stdin = r.as_mut().filter(|r| r.id == run)?.stdin.as_mut()?;
            writeln!(stdin, "{code}").and_then(|_| stdin.flush()).ok()
        });
        if sent.is_some() {
            card.code_sent();
            c.tr.state.fields.remove(&field);
            c.tr.state.focus = None;
        }
    });
    redraw(id);
}

/// The card's "Cancel": its login ends.
pub(super) fn cancel(id: &str, block: usize) {
    with(id, |c| {
        let Some(card) = card(c, block).filter(|k| k.active()) else {
            return;
        };
        card.state = SignInState::Cancelled;
        let run = card.run;
        RUN.with(|r| {
            let mut r = r.borrow_mut();
            if r.as_ref().is_some_and(|r| r.id == run) {
                r.take();
            }
        });
        c.tr.state.focus = None;
    });
    redraw(id);
}

/// `/logout`: the CLI logs out, hidden, and the session says how it went.
pub(super) fn log_out(chat: &mut Chat, id: &str) {
    let mut c = match command(&["auth", "logout"]) {
        Ok(c) => c,
        Err(why) => {
            chat.folio.note(&format!("Not logged out: {why}"));
            return;
        }
    };
    chat.folio.note("Logging out of Claude\u{2026}");
    chat.follow = true;
    let (router, id) = (super::router(), id.to_string());
    std::thread::spawn(move || {
        let said = match c.stdin(Stdio::null()).output() {
            Ok(o) if o.status.success() => {
                let out = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if out.is_empty() {
                    "Logged out of Claude.".to_string()
                } else {
                    out
                }
            }
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
                format!("Not logged out: {err}")
            }
            Err(e) => format!("Not logged out: {e}"),
        };
        if let Ok(mut l) = LOGOUTS.lock() {
            l.push((id, said));
        }
        post(router);
    });
}

/// What session `id` says first once it has started again after a login.
pub(super) fn greeting(id: &str) -> Option<String> {
    GREETINGS.with(|g| g.borrow_mut().remove(id))
}

/// Brings every card up to what the logins said, on the UI thread.
pub(super) fn tend() {
    let logouts = LOGOUTS.lock().map(|mut l| std::mem::take(&mut *l));
    for (id, said) in logouts.unwrap_or_default() {
        with(&id, |c| c.folio.note(&said));
        redraw(&id);
    }
    let run = RUN.with(|r| {
        let r = r.borrow();
        let r = r.as_ref()?;
        let said = r.said.lock().ok()?.clone();
        Some((r.id, r.chat.clone(), said))
    });
    let (current, owner, said) = run.unwrap_or_default();
    let mut heard = Heard::read(&said.out, &said.err);
    // Done waits for the end, which brings who it logged in as.
    heard.succeeded &= said.exit.is_some();
    let mut changed: Vec<String> = Vec::new();
    let mut done = false;
    let mut waiting: Vec<String> = Vec::new();
    let tended = CHATS.with(|c| {
        let Ok(mut chats) = c.try_borrow_mut() else {
            return false;
        };
        for (id, chat) in chats.iter_mut() {
            let mut moved = false;
            for b in chat.folio.blocks.iter_mut() {
                let Block::SignIn(card) = b else {
                    continue;
                };
                if !card.active() {
                    continue;
                }
                if card.run != current || *id != owner {
                    card.state = SignInState::Cancelled;
                    moved = true;
                    continue;
                }
                if card.hear(&heard, said.exit) {
                    moved = true;
                    if card.state == SignInState::Done(None) {
                        let email = said.status.as_ref().and_then(|s| s.email.clone());
                        card.state = SignInState::Done(email);
                        done = true;
                    }
                }
            }
            if (chat.folio.sign_in_card()).is_some_and(|k| k.state == SignInState::Needed) {
                waiting.push(id.clone());
            }
            if moved {
                changed.push(id.clone());
            }
        }
        true
    });
    if !tended {
        // Painting holds the Folios: come back once it is done.
        post(super::router());
        return;
    }
    if said.exit.is_some() {
        RUN.with(|r| {
            let mut r = r.borrow_mut();
            if r.as_ref().is_some_and(|r| r.id == current) {
                r.take();
            }
        });
    }
    for id in &changed {
        redraw(id);
    }
    if !done {
        return;
    }
    let who = said.status.as_ref().and_then(|s| {
        let email = s.email.as_deref()?;
        Some(match &s.subscription {
            Some(plan) => format!("Logged in to Claude as {email} ({plan})."),
            None => format!("Logged in to Claude as {email}."),
        })
    });
    let who = who.unwrap_or_else(|| "Logged in to Claude.".to_string());
    super::restart(&owner, &who, true);
    for id in waiting.iter().filter(|w| **w != owner) {
        super::restart(id, &who, false);
    }
}

/// Session `id` says `text` first once it has started again.
pub(super) fn greet(id: &str, text: &str) {
    GREETINGS.with(|g| g.borrow_mut().insert(id.to_string(), text.to_string()));
}
