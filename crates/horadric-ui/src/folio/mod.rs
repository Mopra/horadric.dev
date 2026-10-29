//! The Folio: a headless Claude session's conversation drawn as a chat, not
//! a terminal. Experimental, beside the terminal sessions, opened from the
//! chat button under a project's tiles. See docs/plan/headless.md.
//!
//! A Folio is a pane on the stage like a sanctuary: its console has no
//! program, and the pane keeps its header and glass. The conversation is a
//! child window of the pane laid over the glass, with its own render
//! target, since a pane's drawing is the terminal's. The pane keeps the
//! keyboard and hands keys here; the child takes the mouse.
//!
//! The session itself (the [`Folio`] model and the [`Headless`] process)
//! lives here by project key, not in the window, so it runs on while the
//! stage shows another project. Lines come on the headless reader's thread
//! and reach the UI thread through a message only window.
//!
//! Behind the conversation runes drift up through a glow in the project's
//! colour. They warm while Claude works and cool to rest when it stops,
//! and at rest, or hidden, nothing redraws.
//!
//! Here is the session: its registry by project key, its lifecycle and
//! what reaches the CLI. [`view`] is the window, its paint loop and input,
//! [`transcript`] lays out and draws the blocks, [`composer`] the input
//! box and its buttons, and [`backdrop`] the background.

mod backdrop;
mod composer;
mod transcript;
mod view;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use horadric_core::folio::Folio;
use horadric_pty::headless::Headless;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetParent, PostMessageW, RegisterClassW, HWND_MESSAGE,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_USER, WNDCLASSW,
};

use crate::field::Field;
use crate::folio_look as look;

pub use view::{attach, char, key, place, register_class, wheel};
/// Posted to the pane a Folio is in when what its header says may have
/// changed.
pub const WM_FOLIO_REFRESH: u32 = WM_USER + 9;
/// Posted to the router when lines wait in some Folio's inbox.
const WM_FOLIO_LINES: u32 = WM_USER + 10;

const ROUTER_CLASS: PCWSTR = w!("HoradricFolioRouter");
const COMPOSER_MAX: usize = 100_000;

/// One Folio and the session behind it.
struct Chat {
    dir: PathBuf,
    folio: Folio,
    /// None when it did not start.
    headless: Option<Headless>,
    /// Lines from the reader thread, None for the stream's end.
    inbox: Arc<Mutex<VecDeque<Option<String>>>>,
    /// A wake is queued for the router, so a chatty stream posts once.
    posted: Arc<AtomicBool>,
    ended: bool,
    error: Option<String>,
    view: Option<HWND>,
    composer: Field,
    composer_scroll: f32,
    scroll: f32,
    /// Keeps the end in view as it streams, until scrolled away from.
    follow: bool,
    content_h: f32,
    view_h: f32,
    /// Blocks unfolded by a click, by index.
    open: HashSet<usize>,
    busy_was: bool,
    busy_since: Instant,
    /// What the pane's header was last told, so it is told again only
    /// when that changes.
    header: String,
}

impl Chat {
    fn new(dir: &Path) -> Chat {
        Chat {
            dir: dir.to_path_buf(),
            folio: Folio::new(),
            headless: None,
            inbox: Arc::default(),
            posted: Arc::default(),
            ended: false,
            error: None,
            view: None,
            composer: Field::new("", true, COMPOSER_MAX),
            composer_scroll: 0.0,
            scroll: 0.0,
            follow: true,
            content_h: 0.0,
            view_h: 0.0,
            open: HashSet::new(),
            busy_was: false,
            // Long ago, so a Folio opened at rest starts cold.
            busy_since: Instant::now() - look::COOL,
            header: String::new(),
        }
    }

    fn note_busy(&mut self) {
        if self.folio.busy != self.busy_was {
            self.busy_was = self.folio.busy;
            self.busy_since = Instant::now();
        }
    }

    fn heat(&self) -> f32 {
        look::heat(self.folio.busy, self.busy_since.elapsed())
    }

    fn detail(&self) -> String {
        let state = if self.error.is_some() || self.ended {
            "ended"
        } else if self.folio.busy {
            "working"
        } else {
            "experimental"
        };
        match &self.folio.model {
            Some(m) => format!("{state} \u{b7} {m}"),
            None => state.to_string(),
        }
    }

    /// Whether the header says something new since it was last told.
    fn header_moved(&mut self) -> bool {
        let now = self.detail();
        if now == self.header {
            return false;
        }
        self.header = now;
        true
    }

    /// Writes what the model queued for the CLI, such as the answer to a
    /// request it cannot serve.
    fn flush(&mut self) {
        let lines = self.folio.take_outbox();
        let Some(h) = &self.headless else {
            return;
        };
        for line in lines {
            if let Err(e) = h.send_line(&line) {
                eprintln!("horadric: folio could not answer the CLI: {e}");
            }
        }
    }
}

thread_local! {
    static CHATS: RefCell<HashMap<String, Chat>> = RefCell::new(HashMap::new());
    static ROUTER: Cell<isize> = const { Cell::new(0) };
}

fn with<T>(key: &str, f: impl FnOnce(&mut Chat) -> T) -> Option<T> {
    CHATS.with(|c| c.try_borrow_mut().ok()?.get_mut(key).map(f))
}

/// Starts the project's Folio, a headless Claude in `dir`, unless it has
/// one already.
pub fn open(key: &str, dir: &Path) {
    if CHATS.with(|c| c.borrow().contains_key(key)) {
        return;
    }
    let mut chat = Chat::new(dir);
    let router = router();
    let (inbox, posted) = (Arc::clone(&chat.inbox), Arc::clone(&chat.posted));
    let started = Headless::start(dir, None, move |line| {
        if let Ok(mut q) = inbox.lock() {
            q.push_back(line);
        }
        if !posted.swap(true, Ordering::AcqRel) {
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(router as *mut c_void)),
                    WM_FOLIO_LINES,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    });
    match started {
        Ok(h) => chat.headless = Some(h),
        Err(e) => chat.error = Some(format!("Claude did not start: {e}")),
    }
    CHATS.with(|c| c.borrow_mut().insert(key.to_string(), chat));
}

/// Ends the project's Folio: its claude.exe goes with its [`Headless`].
pub fn close(key: &str) {
    let gone = CHATS.with(|c| c.borrow_mut().remove(key));
    drop(gone);
}

/// Ends every Folio, as the app quits or reloads. A headless session has
/// no host to outlive the app in yet.
pub fn close_all() {
    let gone: Vec<Chat> = CHATS.with(|c| c.borrow_mut().drain().map(|(_, v)| v).collect());
    drop(gone);
}

/// What the pane's header says beside its name.
pub fn detail(key: &str) -> String {
    with(key, |c| c.detail()).unwrap_or_default()
}

/// The window lines are routed through to the UI thread, made on first
/// use.
fn router() -> isize {
    let have = ROUTER.with(Cell::get);
    if have != 0 {
        return have;
    }
    let hwnd = unsafe {
        let Ok(instance) = GetModuleHandleW(None) else {
            return 0;
        };
        let wc = WNDCLASSW {
            lpfnWndProc: Some(router_proc),
            hInstance: instance.into(),
            lpszClassName: ROUTER_CLASS,
            ..Default::default()
        };
        RegisterClassW(&wc);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            ROUTER_CLASS,
            PCWSTR::null(),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        )
    };
    let id = hwnd.map_or(0, |h| h.0 as isize);
    ROUTER.with(|r| r.set(id));
    id
}

unsafe extern "system" fn router_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_FOLIO_LINES {
        drain();
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Feeds every Folio the lines its reader left, and redraws the ones that
/// changed.
fn drain() {
    let mut changed: Vec<(HWND, bool)> = Vec::new();
    let busy = CHATS.with(|c| {
        let Ok(mut chats) = c.try_borrow_mut() else {
            return true;
        };
        for chat in chats.values_mut() {
            // Before the drain, so a line after it posts again.
            chat.posted.store(false, Ordering::Release);
            let lines: Vec<Option<String>> = match chat.inbox.lock() {
                Ok(mut q) => q.drain(..).collect(),
                Err(_) => continue,
            };
            if lines.is_empty() {
                continue;
            }
            let mut moved = false;
            for line in lines {
                match line {
                    Some(l) => moved |= chat.folio.feed(&l),
                    None => {
                        chat.folio.end();
                        // Before any init, nothing ran: stderr says why.
                        if chat.folio.session_id.is_none() && chat.error.is_none() {
                            chat.error = Some(start_failure(chat.headless.as_ref()));
                        }
                        chat.ended = true;
                        moved = true;
                    }
                }
            }
            chat.flush();
            chat.note_busy();
            let header = chat.header_moved();
            if let (true, Some(v)) = (moved || header, chat.view) {
                changed.push((v, header));
            }
        }
        false
    });
    if busy {
        // Painting holds the Folios. Come back once it is done.
        unsafe {
            let _ = PostMessageW(
                Some(HWND(ROUTER.with(Cell::get) as *mut c_void)),
                WM_FOLIO_LINES,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
    for (v, header) in changed {
        refresh(v, header);
    }
}

/// What the error line says when claude.exe ended before it started, with
/// the last of what it wrote to stderr.
fn start_failure(h: Option<&Headless>) -> String {
    let tail = h.map(Headless::stderr_tail).unwrap_or_default();
    let last: Vec<&str> = (tail.iter().rev())
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .take(3)
        .collect();
    if last.is_empty() {
        return "Claude ended before it started.".to_string();
    }
    let said: Vec<&str> = last.into_iter().rev().collect();
    format!("Claude ended before it started: {}", said.join(" "))
}

/// Redraws the conversation, and tells its pane when its header changed.
fn refresh(view: HWND, header: bool) {
    unsafe {
        let _ = InvalidateRect(Some(view), None, false);
        if header {
            if let Ok(pane) = GetParent(view) {
                let _ = PostMessageW(Some(pane), WM_FOLIO_REFRESH, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn redraw(key: &str) {
    if let Some((Some(v), header)) = with(key, |c| (c.view, c.header_moved())) {
        refresh(v, header);
    }
}

fn send_prompt(key: &str) {
    with(key, |c| {
        if c.folio.busy || c.ended {
            return;
        }
        let Some(text) = look::prompt_of(&c.composer.text) else {
            return;
        };
        let Some(h) = &c.headless else {
            return;
        };
        c.folio.push_prompt(&text);
        if let Err(e) = h.send_prompt(&text) {
            c.error = Some(format!("The prompt did not reach Claude: {e}"));
            // No turn started, so none will end with a result.
            c.folio.end();
        }
        c.composer = Field::new("", true, COMPOSER_MAX);
        c.composer_scroll = 0.0;
        c.follow = true;
        c.note_busy();
    });
}

fn answer(key: &str, request_id: &str, allow: bool) {
    with(key, |c| {
        let Some(line) = c.folio.answer(request_id, allow) else {
            return;
        };
        if let Some(h) = &c.headless {
            if let Err(e) = h.send_line(&line) {
                c.error = Some(format!("The answer did not reach Claude: {e}"));
            }
        }
    });
    redraw(key);
}

fn interrupt(key: &str) {
    with(key, |c| {
        if let Some(h) = &c.headless {
            let _ = h.interrupt();
        }
    });
}
