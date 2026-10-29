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

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use horadric_core::folio::{AskState, Block, Ending, Folio, StepState};
use horadric_pty::headless::Headless;
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_GRADIENT_STOP, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{
    ID2D1HwndRenderTarget, ID2D1LinearGradientBrush, ID2D1RadialGradientBrush,
    ID2D1SolidColorBrush, ID2D1StrokeStyle, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_CAP_STYLE_ROUND, D2D1_DASH_STYLE_SOLID, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE,
    D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    D2D1_LINE_JOIN_ROUND, D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES, D2D1_ROUNDED_RECT,
    D2D1_STROKE_STYLE_PROPERTIES,
};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteTextFormat, IDWriteTextLayout, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_ITALIC,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Gdi::{
    CreateRoundRectRgn, InvalidateRect, ScreenToClient, SetWindowRgn, ValidateRect,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_DELETE, VK_DOWN, VK_END, VK_HOME,
    VK_LEFT, VK_NEXT, VK_PRIOR, VK_RIGHT, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetAncestor, GetClientRect, GetCursorPos, GetParent,
    GetWindowLongPtrW, IsIconic, IsWindowVisible, KillTimer, LoadCursorW, PostMessageW,
    RegisterClassW, SetCursor, SetTimer, SetWindowLongPtrW, SetWindowPos, CREATESTRUCTW,
    CS_DBLCLKS, GA_ROOT, GWLP_USERDATA, HTCLIENT, HWND_MESSAGE, IDC_ARROW, IDC_HAND, IDC_IBEAM,
    SWP_NOACTIVATE, SWP_NOZORDER, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CAPTURECHANGED, WM_DESTROY,
    WM_ERASEBKGND, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE,
    WM_NCDESTROY, WM_PAINT, WM_SETCURSOR, WM_SIZE, WM_TIMER, WM_USER, WNDCLASSW, WS_CHILD,
    WS_CLIPSIBLINGS, WS_VISIBLE,
};
use windows_numerics::Vector2;

use crate::clipboard;
use crate::field::{self, Field};
use crate::folio_look::{self as look, RunState};
use crate::keys::Mods;
use crate::layout::Rect;
use crate::render::{self, color, Gpu};
use crate::sparks::RUNES;
use crate::theme::{self, Color};
use crate::window::Shared;

/// Posted to the pane a Folio is in when what its header says may have
/// changed.
pub const WM_FOLIO_REFRESH: u32 = WM_USER + 9;
/// Posted to the router when lines wait in some Folio's inbox.
const WM_FOLIO_LINES: u32 = WM_USER + 10;

const CLASS: PCWSTR = w!("HoradricFolio");
const ROUTER_CLASS: PCWSTR = w!("HoradricFolioRouter");
const TICK: usize = 1;
/// Between frames while the background moves. It drifts slowly, so this is
/// smooth enough and costs a third of the display's rate.
const FRAME_MS: u32 = 40;
/// The glass's corner, less the line of its rim the conversation leaves.
const CORNER: f32 = 7.0;

const FONT: PCWSTR = w!("Segoe UI Variable Text");
const MONO: PCWSTR = w!("Cascadia Mono");
const ICONS: PCWSTR = w!("Segoe Fluent Icons");

const TOP: f32 = 22.0;
const GAP: f32 = 14.0;
const LINE_H: f32 = 22.0;
const STEP_H: f32 = 20.0;
const FOOTER_H: f32 = 18.0;
const BUBBLE_X: f32 = 14.0;
const BUBBLE_Y: f32 = 9.0;
const BUBBLE_SHARE: f32 = 0.82;
const CARD_PAD: f32 = 14.0;
const BUTTON_W: f32 = 84.0;
const BUTTON_H: f32 = 30.0;
/// The most of an ask's input a card shows before it is cut off.
const ASK_MAX_H: f32 = 140.0;
const COMPOSER_PAD: f32 = 12.0;
const COMPOSER_BOTTOM: f32 = 16.0;
const COMPOSER_LINES: f32 = 8.0;
const SEND: f32 = 28.0;
/// A wheel notch, in DIPs.
const NOTCH: f32 = 54.0;
/// What a raw line shows of itself.
const RAW_MAX: usize = 400;
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

pub fn register_class() -> Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        Ok(())
    }
}

/// Lays the conversation of project `key` over the glass of the pane
/// `parent`. It is placed by [`place`] and goes with the pane.
pub fn attach(key: &str, shared: &Rc<Shared>, parent: HWND) {
    let view = Box::new(View {
        hwnd: Cell::new(HWND::default()),
        key: key.to_string(),
        shared: Rc::clone(shared),
        gfx: RefCell::new(None),
        hits: RefCell::new(Vec::new()),
        laid: RefCell::new(Vec::new()),
        composer: RefCell::new(None),
        ticking: Cell::new(false),
        last: Cell::new(None),
        clock: Cell::new(0.0),
        selecting: Cell::new(false),
    });
    let hwnd = unsafe {
        let Ok(instance) = GetModuleHandleW(None) else {
            return;
        };
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
            0,
            0,
            0,
            0,
            Some(parent),
            None,
            Some(instance.into()),
            Some(Box::into_raw(view) as *const c_void),
        )
    };
    if let Ok(hwnd) = hwnd {
        with(key, |c| c.view = Some(hwnd));
    }
}

/// Puts the conversation over `glass`, in the pane's pixels, its corners
/// rounded as the glass's are.
pub fn place(key: &str, glass: RECT) {
    let Some(Some(view)) = with(key, |c| c.view) else {
        return;
    };
    let (w, h) = (glass.right - glass.left, glass.bottom - glass.top);
    unsafe {
        let _ = SetWindowPos(
            view,
            None,
            glass.left,
            glass.top,
            w.max(0),
            h.max(0),
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        let r = (CORNER * 2.0 * dpi(view) as f32 / 96.0).round() as i32;
        // The window owns the region once it is set.
        let region = CreateRoundRectRgn(0, 0, w + 1, h + 1, r, r);
        SetWindowRgn(view, Some(region), true);
    }
}

fn dpi(hwnd: HWND) -> u32 {
    unsafe { GetDpiForWindow(hwnd) }.max(96)
}

/// A key that makes a character, from the pane. Enter sends, Shift+Enter
/// breaks the line, Esc stops Claude while it works.
pub fn char(key: &str, c: char, mods: Mods) {
    let mut send = false;
    with(key, |chat| {
        let f = &mut chat.composer;
        match c {
            '\r' if mods.shift => f.insert("\n"),
            '\r' => send = true,
            '\u{1b}' => {
                if chat.folio.busy {
                    if let Some(h) = &chat.headless {
                        let _ = h.interrupt();
                    }
                } else {
                    f.set_caret(f.caret, false);
                }
            }
            '\u{8}' => f.backspace(false),
            '\u{7f}' => f.backspace(true),
            '\u{1}' => f.select_all(),
            '\u{3}' => {
                if !f.selected().is_empty() {
                    clipboard::set_text(f.selected());
                }
            }
            '\u{18}' => {
                if !f.selected().is_empty() {
                    clipboard::set_text(&f.cut());
                }
            }
            '\u{16}' => {
                if let Some(t) = clipboard::get_text() {
                    f.insert(&t);
                }
            }
            c if (c as u32) < 0x20 => {}
            c => f.insert(c.encode_utf8(&mut [0; 4])),
        }
    });
    if send {
        send_prompt(key);
    }
    redraw(key);
}

/// A key that makes no character, from the pane. False leaves it to the
/// pane.
pub fn key(key: &str, vk: u16, mods: Mods) -> bool {
    let (word, extend) = (mods.ctrl, mods.shift);
    let page = with(key, |c| c.view_h * 0.85).unwrap_or(0.0);
    let vertical = |up: bool| -> bool {
        // The caret moves a line in the composer, or past its ends the
        // conversation scrolls.
        let moved = view_of(key).and_then(|v| v.caret_line(key, up, extend));
        if moved.is_none() {
            scroll_by(key, if up { -NOTCH } else { NOTCH });
        }
        true
    };
    let done = match vk {
        v if v == VK_LEFT.0 => with(key, |c| c.composer.left(word, extend)).is_some(),
        v if v == VK_RIGHT.0 => with(key, |c| c.composer.right(word, extend)).is_some(),
        v if v == VK_HOME.0 => with(key, |c| c.composer.home(word, extend)).is_some(),
        v if v == VK_END.0 => with(key, |c| c.composer.end(word, extend)).is_some(),
        v if v == VK_DELETE.0 => with(key, |c| c.composer.delete(word)).is_some(),
        v if v == VK_UP.0 => vertical(true),
        v if v == VK_DOWN.0 => vertical(false),
        v if v == VK_PRIOR.0 => {
            scroll_by(key, -page);
            true
        }
        v if v == VK_NEXT.0 => {
            scroll_by(key, page);
            true
        }
        _ => false,
    };
    if done {
        redraw(key);
    }
    done
}

/// The wheel, from the pane, in Windows' units of 120 a notch.
pub fn wheel(key: &str, delta: i32) {
    scroll_by(key, -delta as f32 / 120.0 * NOTCH);
    redraw(key);
}

fn scroll_by(key: &str, by: f32) {
    with(key, |c| {
        c.scroll = look::clamp_scroll(c.scroll + by, c.content_h, c.view_h);
        c.follow = look::at_bottom(c.scroll, c.content_h, c.view_h);
    });
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

/// What a click on part of the conversation does.
#[derive(Clone)]
enum Hit {
    Answer(String, bool),
    Fold(usize),
    Send,
    Stop,
    Composer,
}

/// A block's text laid out, kept while its text and width stay the same.
struct Laid {
    tag: u8,
    len: usize,
    width: f32,
    layout: IDWriteTextLayout,
    w: f32,
    h: f32,
}

struct Gfx {
    rt: ID2D1HwndRenderTarget,
    /// The window's DPI the target draws at. Moving to another monitor
    /// changes the window's, and the target is told.
    dpi: u32,
    brush: ID2D1SolidColorBrush,
    glow: ID2D1RadialGradientBrush,
    shade: ID2D1LinearGradientBrush,
    round: ID2D1StrokeStyle,
    accent: Color,
    text: IDWriteTextFormat,
    line: IDWriteTextFormat,
    italic: IDWriteTextFormat,
    small: IDWriteTextFormat,
    title: IDWriteTextFormat,
    mono: IDWriteTextFormat,
    button: IDWriteTextFormat,
    icon: IDWriteTextFormat,
    big: IDWriteTextFormat,
}

struct View {
    hwnd: Cell<HWND>,
    key: String,
    shared: Rc<Shared>,
    gfx: RefCell<Option<Gfx>>,
    hits: RefCell<Vec<(Rect, Hit)>>,
    laid: RefCell<Vec<Option<Laid>>>,
    /// The composer's text laid out and where it was drawn, in DIPs, for
    /// placing the caret by a click or an arrow.
    composer: RefCell<Option<(IDWriteTextLayout, f32, f32)>>,
    ticking: Cell<bool>,
    last: Cell<Option<Instant>>,
    /// The background's own time, in seconds. It runs only while it moves,
    /// so warming again picks up where it stopped.
    clock: Cell<f32>,
    selecting: Cell<bool>,
}

fn view_of(key: &str) -> Option<&'static View> {
    let hwnd = with(key, |c| c.view)??;
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const View;
        // The window owns its View until WM_NCDESTROY, and every caller is
        // on the UI thread, done with it before any message comes.
        (!ptr.is_null()).then(|| &*ptr)
    }
}

unsafe fn text_format(
    gpu: &Gpu,
    family: PCWSTR,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
    italic: bool,
    one_line: bool,
) -> Result<IDWriteTextFormat> {
    let style = if italic {
        DWRITE_FONT_STYLE_ITALIC
    } else {
        DWRITE_FONT_STYLE_NORMAL
    };
    let f = gpu.dw.CreateTextFormat(
        family,
        None,
        weight,
        style,
        DWRITE_FONT_STRETCH_NORMAL,
        size,
        w!("en-us"),
    )?;
    if one_line {
        f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        let trimming = DWRITE_TRIMMING {
            granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
            delimiter: 0,
            delimiterCount: 0,
        };
        let sign = gpu.dw.CreateEllipsisTrimmingSign(&f)?;
        f.SetTrimming(&trimming, &sign)?;
    }
    Ok(f)
}

fn d2d(r: &Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.x,
        top: r.y,
        right: r.right(),
        bottom: r.bottom(),
    }
}

fn rounded(r: &Rect, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT {
        rect: d2d(r),
        radiusX: radius,
        radiusY: radius,
    }
}

impl Gfx {
    unsafe fn new(gpu: &Gpu, hwnd: HWND, key: &str) -> Result<Gfx> {
        let mut r = RECT::default();
        let _ = GetClientRect(hwnd, &mut r);
        let dpi = dpi(hwnd);
        let rt = render::hwnd_target(gpu, hwnd, r.right as u32, r.bottom as u32, dpi, false)?;
        let brush = rt.CreateSolidColorBrush(&color(theme::palette().term_fg), None)?;
        let accent = theme::accent(key);
        let stops = |a: Color, b: Color| {
            [
                D2D1_GRADIENT_STOP {
                    position: 0.0,
                    color: color(a),
                },
                D2D1_GRADIENT_STOP {
                    position: 1.0,
                    color: color(b),
                },
            ]
        };
        let glow_stops = rt.CreateGradientStopCollection(
            &stops(accent, accent.with_alpha(0.0)),
            D2D1_GAMMA_2_2,
            D2D1_EXTEND_MODE_CLAMP,
        )?;
        let glow = rt.CreateRadialGradientBrush(
            &D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES::default(),
            None,
            &glow_stops,
        )?;
        let black = Color::rgb(0);
        let shade_stops = rt.CreateGradientStopCollection(
            &stops(black.with_alpha(0.5), black.with_alpha(0.0)),
            D2D1_GAMMA_2_2,
            D2D1_EXTEND_MODE_CLAMP,
        )?;
        let shade = rt.CreateLinearGradientBrush(
            &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES::default(),
            None,
            &shade_stops,
        )?;
        let round = gpu.d2d.CreateStrokeStyle(
            &D2D1_STROKE_STYLE_PROPERTIES {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                dashCap: D2D1_CAP_STYLE_ROUND,
                lineJoin: D2D1_LINE_JOIN_ROUND,
                miterLimit: 1.0,
                dashStyle: D2D1_DASH_STYLE_SOLID,
                dashOffset: 0.0,
            },
            None,
        )?;
        let normal = DWRITE_FONT_WEIGHT_NORMAL;
        let semi = DWRITE_FONT_WEIGHT_SEMI_BOLD;
        let button = text_format(gpu, FONT, 13.0, semi, false, true)?;
        button.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
        let icon = text_format(gpu, ICONS, 12.0, normal, false, true)?;
        icon.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
        let big = text_format(gpu, FONT, 20.0, semi, false, true)?;
        big.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
        Ok(Gfx {
            rt,
            dpi,
            brush,
            glow,
            shade,
            round,
            accent,
            text: text_format(gpu, FONT, 14.5, normal, false, false)?,
            line: text_format(gpu, FONT, 13.0, normal, false, true)?,
            italic: text_format(gpu, FONT, 13.0, normal, true, false)?,
            small: text_format(gpu, FONT, 12.0, normal, false, true)?,
            title: text_format(gpu, FONT, 13.5, semi, false, true)?,
            mono: text_format(gpu, MONO, 12.5, normal, false, false)?,
            button,
            icon,
            big,
        })
    }

    unsafe fn fill(&self, r: &Rect, c: Color) {
        self.brush.SetColor(&color(c));
        self.rt.FillRectangle(&d2d(r), &self.brush);
    }

    unsafe fn fill_round(&self, r: &Rect, radius: f32, c: Color) {
        self.brush.SetColor(&color(c));
        self.rt
            .FillRoundedRectangle(&rounded(r, radius), &self.brush);
    }

    unsafe fn stroke_round(&self, r: &Rect, radius: f32, c: Color, width: f32) {
        self.brush.SetColor(&color(c));
        let r = Rect::new(r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0);
        self.rt
            .DrawRoundedRectangle(&rounded(&r, radius), &self.brush, width, None);
    }

    unsafe fn text_in(&self, s: &str, f: &IDWriteTextFormat, r: &Rect, c: Color) {
        let wide: Vec<u16> = s.encode_utf16().collect();
        self.brush.SetColor(&color(c));
        self.rt.DrawText(
            &wide,
            f,
            &d2d(r),
            &self.brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            Default::default(),
        );
    }

    unsafe fn layout_at(&self, l: &IDWriteTextLayout, x: f32, y: f32, c: Color) {
        self.brush.SetColor(&color(c));
        self.rt.DrawTextLayout(
            Vector2 { X: x, Y: y },
            l,
            &self.brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
    }

    /// A rune of the rim's, `size` tall with its top at `y`, centred on `x`.
    unsafe fn rune(&self, which: usize, x: f32, y: f32, size: f32, c: Color, width: f32) {
        self.brush.SetColor(&color(c));
        for stroke in RUNES[which % RUNES.len()] {
            for pair in stroke.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                self.rt.DrawLine(
                    Vector2 {
                        X: x + a.0 * size,
                        Y: y + a.1 * size,
                    },
                    Vector2 {
                        X: x + b.0 * size,
                        Y: y + b.1 * size,
                    },
                    &self.brush,
                    width,
                    &self.round,
                );
            }
        }
    }
}

/// Everything one frame draws, in the order blocks stack.
struct Palette {
    bg: Color,
    fg: Color,
    dim: Color,
    faint: Color,
    accent: Color,
}

impl View {
    fn hwnd(&self) -> HWND {
        self.hwnd.get()
    }

    fn scale(&self) -> f32 {
        dpi(self.hwnd()) as f32 / 96.0
    }

    fn dip(&self, lparam: LPARAM) -> (f32, f32) {
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32;
        (x / s, y / s)
    }

    /// Shown, and on a stage that is not minimized: worth drawing frames
    /// for.
    fn seen(&self) -> bool {
        unsafe {
            IsWindowVisible(self.hwnd()).as_bool()
                && !IsIconic(GetAncestor(self.hwnd(), GA_ROOT)).as_bool()
        }
    }

    /// Runs the frame clock while the background moves and it is seen,
    /// and stops it otherwise.
    fn clock(&self, moving: bool) {
        let on = moving && self.seen();
        if self.ticking.replace(on) != on {
            unsafe {
                if on {
                    SetTimer(Some(self.hwnd()), TICK, FRAME_MS, None);
                } else {
                    let _ = KillTimer(Some(self.hwnd()), TICK);
                }
            }
        }
        if !on {
            self.last.set(None);
        }
    }

    /// The block's text laid out at `width`, from the cache when it has not
    /// changed.
    fn laid(
        &self,
        gfx: &Gfx,
        i: usize,
        tag: u8,
        text: &str,
        format: &IDWriteTextFormat,
        width: f32,
    ) -> Option<(IDWriteTextLayout, f32, f32)> {
        let mut laid = self.laid.borrow_mut();
        if laid.len() <= i {
            laid.resize_with(i + 1, || None);
        }
        if let Some(l) = &laid[i] {
            if l.tag == tag && l.len == text.len() && l.width == width {
                return Some((l.layout.clone(), l.w, l.h));
            }
        }
        let layout = render::wrapped(&self.shared.gpu, format, text, width.max(1.0)).ok()?;
        let (w, h) = render::text_size(&layout);
        laid[i] = Some(Laid {
            tag,
            len: text.len(),
            width,
            layout: layout.clone(),
            w,
            h,
        });
        let _ = gfx;
        Some((layout, w, h))
    }

    /// How tall block `i` stands at column width `cw`.
    fn height(&self, gfx: &Gfx, i: usize, b: &Block, open: bool, cw: f32) -> f32 {
        let laid = |tag, text: &str, f: &IDWriteTextFormat, w| {
            self.laid(gfx, i, tag, text, f, w).map_or(0.0, |l| l.2)
        };
        match b {
            Block::Prompt(t) => {
                laid(0, t, &gfx.text, cw * BUBBLE_SHARE - 2.0 * BUBBLE_X) + 2.0 * BUBBLE_Y
            }
            Block::Reply(t) if t.trim().is_empty() => 0.0,
            Block::Reply(t) => laid(1, t, &gfx.text, cw),
            Block::Thinking(t) if open => LINE_H + 4.0 + laid(2, t, &gfx.italic, cw - 22.0),
            Block::Thinking(_) => LINE_H,
            Block::Tools(steps) if open => LINE_H + steps.len() as f32 * STEP_H,
            Block::Tools(_) => LINE_H,
            Block::Ask { shown, .. } => {
                let shown = laid(3, shown, &gfx.mono, cw - 2.0 * CARD_PAD).min(ASK_MAX_H);
                CARD_PAD + 20.0 + 6.0 + shown + 12.0 + BUTTON_H + CARD_PAD
            }
            Block::Footer { .. } => FOOTER_H,
            Block::Raw(t) => laid(4, &raw_text(t), &gfx.italic, cw),
            _ => 0.0,
        }
    }

    fn paint(&self) {
        let hwnd = self.hwnd();
        let mut slot = self.gfx.borrow_mut();
        let accent = theme::accent(&self.key);
        if slot.as_ref().is_some_and(|g| g.accent != accent) {
            *slot = None;
        }
        if slot.is_none() {
            match unsafe { Gfx::new(&self.shared.gpu, hwnd, &self.key) } {
                Ok(g) => *slot = Some(g),
                Err(e) => {
                    eprintln!("horadric: folio render target: {e}");
                    return;
                }
            }
        }
        let Some(gfx) = slot.as_mut() else {
            return;
        };
        let now = dpi(hwnd);
        if gfx.dpi != now {
            unsafe { gfx.rt.SetDpi(now as f32, now as f32) };
            gfx.dpi = now;
        }
        let gfx = &*gfx;
        let ok = CHATS.with(|c| {
            let Ok(mut chats) = c.try_borrow_mut() else {
                return true;
            };
            let Some(chat) = chats.get_mut(&self.key) else {
                return true;
            };
            unsafe { self.draw(gfx, chat) }
        });
        if !ok {
            *slot = None;
        }
    }

    /// Draws one frame. False when the target was lost.
    unsafe fn draw(&self, gfx: &Gfx, chat: &mut Chat) -> bool {
        let now = Instant::now();
        let heat = chat.heat();
        let moving = chat.folio.busy || heat > 0.0;
        if moving {
            let dt = self
                .last
                .replace(Some(now))
                .map_or(0.0, |t| now.duration_since(t).as_secs_f32().min(0.1));
            self.clock.set(self.clock.get() + dt);
        }
        self.clock(moving);
        let t = self.clock.get();
        let rt = &gfx.rt;
        let size = rt.GetSize();
        let (w, h) = (size.width, size.height);
        let p = theme::palette();
        let pal = Palette {
            bg: p.term_bg,
            fg: p.term_fg,
            dim: p.term_fg.with_alpha(0.6),
            faint: p.term_fg.with_alpha(0.38),
            accent: gfx.accent,
        };
        let (cx, cw) = look::column(w);
        let mut hits: Vec<(Rect, Hit)> = Vec::new();

        rt.BeginDraw();
        rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        rt.Clear(Some(&color(pal.bg)));
        self.background(gfx, &pal, w, h, t, heat);

        // The composer first, since the conversation ends above it.
        // Its hits go last, so they win over a block's under the composer.
        let mut composer_hits: Vec<(Rect, Hit)> = Vec::new();
        let composer = self.composer(gfx, chat, &pal, cx, cw, h, &mut composer_hits);
        let area = Rect::new(0.0, 0.0, w, (composer.y - 10.0).max(0.0));

        // Measure, then follow the end or keep the scroll in bounds.
        let heights: Vec<f32> = (chat.folio.blocks.iter().enumerate())
            .map(|(i, b)| self.height(gfx, i, b, chat.open.contains(&i), cw))
            .collect();
        let tail = if chat.folio.busy { 28.0 } else { 0.0 }
            + if chat.ended || chat.error.is_some() {
                LINE_H + GAP
            } else {
                0.0
            };
        let content = TOP
            + (chat.folio.blocks.iter().zip(&heights))
                .map(|(b, &bh)| if bh > 0.0 { bh + gap_after(b) } else { 0.0 })
                .sum::<f32>()
            + tail
            + 8.0;
        chat.content_h = content;
        chat.view_h = area.h;
        chat.scroll = if chat.follow {
            (content - area.h).max(0.0)
        } else {
            look::clamp_scroll(chat.scroll, content, area.h)
        };

        rt.PushAxisAlignedClip(&d2d(&area), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        if chat.folio.blocks.is_empty() && chat.error.is_none() && !chat.ended {
            self.empty(gfx, &pal, chat, &area);
        }
        let mut y = TOP - chat.scroll;
        for (i, (b, &bh)) in chat.folio.blocks.iter().zip(&heights).enumerate() {
            if bh <= 0.0 {
                continue;
            }
            if y + bh >= 0.0 && y <= area.bottom() {
                let open = chat.open.contains(&i);
                let mut mine = Vec::new();
                self.block(gfx, &pal, i, b, open, cx, cw, y, bh, &mut mine);
                // Drawing is clipped to the area; what is hidden must not
                // take clicks either.
                hits.extend(
                    (mine.into_iter()).filter_map(|(r, hit)| Some((look::clip(&r, &area)?, hit))),
                );
            }
            y += bh + gap_after(b);
        }
        if chat.folio.busy {
            for k in 0..3 {
                let pulse = ((t * 4.0 - k as f32 * 0.7).sin() * 0.5 + 0.5).powf(1.5);
                let x = cx + 6.0 + k as f32 * 12.0;
                gfx.brush.SetColor(&color(
                    pal.accent.mix(pal.fg, 0.3).with_alpha(0.25 + 0.75 * pulse),
                ));
                rt.FillEllipse(
                    &D2D1_ELLIPSE {
                        point: Vector2 { X: x, Y: y + 10.0 },
                        radiusX: 3.2,
                        radiusY: 3.2,
                    },
                    &gfx.brush,
                );
            }
            y += 28.0;
        }
        if let Some(e) = &chat.error {
            gfx.text_in(e, &gfx.line, &Rect::new(cx, y, cw, LINE_H), theme::error());
        } else if chat.ended {
            let r = Rect::new(cx, y, cw, LINE_H);
            gfx.text_in(
                "Claude ended. Close the Folio to start again.",
                &gfx.line,
                &r,
                pal.dim,
            );
        }
        rt.PopAxisAlignedClip();

        // The glass is sunk into the plate, so its top edge is in shade.
        gfx.shade.SetStartPoint(Vector2 { X: 0.0, Y: 0.0 });
        gfx.shade.SetEndPoint(Vector2 { X: 0.0, Y: 9.0 });
        rt.FillRectangle(&d2d(&Rect::new(0.0, 0.0, w, 9.0)), &gfx.shade);

        hits.extend(composer_hits);
        *self.hits.borrow_mut() = hits;
        rt.EndDraw(None, None).is_ok()
    }

    /// The glow rising from under the composer in the project's colour,
    /// and runes drifting up through it, warmer while Claude works.
    unsafe fn background(&self, gfx: &Gfx, pal: &Palette, w: f32, h: f32, t: f32, heat: f32) {
        let breath = (t * 1.6).sin() * 0.5 + 0.5;
        gfx.glow.SetCenter(Vector2 {
            X: w / 2.0,
            Y: h + h * 0.1,
        });
        let reach = w.max(h) * (0.62 + 0.06 * heat * breath);
        gfx.glow.SetRadiusX(reach);
        gfx.glow.SetRadiusY(reach * 0.7);
        gfx.glow.SetOpacity(0.09 + heat * (0.08 + 0.06 * breath));
        gfx.rt
            .FillRectangle(&d2d(&Rect::new(0.0, 0.0, w, h)), &gfx.glow);
        let ink = pal.fg.mix(pal.accent, 0.35 + 0.5 * heat);
        for i in 0..look::EMBERS {
            let e = look::ember(i, t, w, h, heat, RUNES.len());
            if e.alpha <= 0.004 {
                continue;
            }
            gfx.rune(e.rune, e.x, e.y, e.size, ink.with_alpha(e.alpha), 1.3);
        }
    }

    /// What an empty Folio says, in the middle of the glass.
    unsafe fn empty(&self, gfx: &Gfx, pal: &Palette, chat: &Chat, area: &Rect) {
        let mid = area.h / 2.0;
        let x = area.w / 2.0;
        gfx.rune(3, x, mid - 70.0, 34.0, pal.accent.with_alpha(0.55), 2.0);
        let name = chat
            .dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let title = format!("Ask Claude about {name}");
        let r = Rect::new(area.x + 20.0, mid - 22.0, area.w - 40.0, 30.0);
        gfx.text_in(&title, &gfx.big, &r, pal.fg.with_alpha(0.85));
        let r = Rect::new(area.x + 20.0, mid + 10.0, area.w - 40.0, 20.0);
        let note = "Experimental. A headless Claude Code session, drawn by Horadric.";
        let mut centred = gfx.button.clone();
        if let Ok(f) = text_format(
            &self.shared.gpu,
            FONT,
            12.5,
            DWRITE_FONT_WEIGHT_NORMAL,
            false,
            true,
        ) {
            let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            centred = f;
        }
        gfx.text_in(note, &centred, &r, pal.faint);
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn block(
        &self,
        gfx: &Gfx,
        pal: &Palette,
        i: usize,
        b: &Block,
        open: bool,
        cx: f32,
        cw: f32,
        y: f32,
        bh: f32,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        match b {
            Block::Prompt(t) => {
                let max = cw * BUBBLE_SHARE - 2.0 * BUBBLE_X;
                let Some((l, tw, _)) = self.laid(gfx, i, 0, t, &gfx.text, max) else {
                    return;
                };
                let bw = tw.min(max) + 2.0 * BUBBLE_X;
                let r = Rect::new(cx + cw - bw, y, bw, bh);
                gfx.fill_round(&r, 14.0, pal.bg.mix(pal.accent, 0.2).mix(pal.fg, 0.04));
                gfx.stroke_round(&r, 14.0, pal.accent.with_alpha(0.28), 1.0);
                gfx.layout_at(&l, r.x + BUBBLE_X, y + BUBBLE_Y, pal.fg);
            }
            Block::Reply(t) => {
                if let Some((l, _, _)) = self.laid(gfx, i, 1, t, &gfx.text, cw) {
                    gfx.layout_at(&l, cx, y, pal.fg);
                }
            }
            Block::Thinking(t) => {
                let r = Rect::new(cx, y, cw, LINE_H);
                self.fold_line(
                    gfx,
                    pal,
                    ('\u{E82F}', pal.faint),
                    &look::thinking_line(t),
                    open,
                    &r,
                    pal.faint,
                );
                hits.push((r, Hit::Fold(i)));
                if open {
                    if let Some((l, _, _)) = self.laid(gfx, i, 2, t, &gfx.italic, cw - 22.0) {
                        gfx.fill(
                            &Rect::new(cx + 7.0, y + LINE_H + 2.0, 1.5, bh - LINE_H - 2.0),
                            pal.faint.with_alpha(0.25),
                        );
                        gfx.layout_at(&l, cx + 22.0, y + LINE_H + 4.0, pal.dim);
                    }
                }
            }
            Block::Tools(steps) => {
                let r = Rect::new(cx, y, cw, LINE_H);
                let state = look::run_state(steps);
                let mut text = look::run_summary(steps);
                // A failure marks the run's icon. The line stays quiet,
                // since a failed step is often Claude finding its way.
                let mark = match state {
                    RunState::Running => {
                        text.push_str(" \u{2026}");
                        pal.dim
                    }
                    RunState::Stopped => {
                        text.push_str(" \u{b7} stopped");
                        pal.faint
                    }
                    RunState::Done(0) => pal.dim,
                    RunState::Done(n) => {
                        text.push_str(&format!(" \u{b7} {n} failed"));
                        theme::error().with_alpha(0.9)
                    }
                };
                let icon = ('\u{E90F}', mark);
                self.fold_line(gfx, pal, icon, &text, open, &r, pal.dim);
                hits.push((r, Hit::Fold(i)));
                if open {
                    for (k, s) in steps.iter().enumerate() {
                        let sy = y + LINE_H + k as f32 * STEP_H;
                        let mark = match s.state {
                            StepState::Running => pal.faint,
                            StepState::Stopped => pal.faint.with_alpha(0.2),
                            StepState::Done => theme::done().with_alpha(0.8),
                            StepState::Failed => theme::error(),
                        };
                        gfx.fill_round(
                            &Rect::new(cx + 6.0, sy + STEP_H / 2.0 - 2.5, 5.0, 5.0),
                            2.5,
                            mark,
                        );
                        let line = if s.summary.is_empty() {
                            s.tool.clone()
                        } else {
                            format!("{}  {}", s.tool, s.summary)
                        };
                        gfx.text_in(
                            &line,
                            &gfx.small,
                            &Rect::new(cx + 22.0, sy, cw - 22.0, STEP_H),
                            pal.dim,
                        );
                    }
                }
            }
            Block::Ask {
                request_id,
                tool,
                shown,
                state,
                ..
            } => {
                let card = Rect::new(cx, y, cw, bh);
                gfx.fill_round(&card, 12.0, pal.bg.mix(pal.fg, 0.05));
                let edge = match state {
                    AskState::Waiting => theme::waiting().with_alpha(0.75),
                    _ => pal.faint.with_alpha(0.3),
                };
                gfx.stroke_round(&card, 12.0, edge, 1.2);
                let inner = cw - 2.0 * CARD_PAD;
                let title = Rect::new(cx + CARD_PAD, y + CARD_PAD, inner, 20.0);
                gfx.text_in(&look::ask_title(tool), &gfx.title, &title, pal.fg);
                let top = title.bottom() + 6.0;
                if let Some((l, _, lh)) = self.laid(gfx, i, 3, shown, &gfx.mono, inner) {
                    let shown = Rect::new(
                        cx + CARD_PAD - 2.0,
                        top - 2.0,
                        inner + 4.0,
                        lh.min(ASK_MAX_H) + 4.0,
                    );
                    gfx.fill_round(&shown, 6.0, Color::rgb(0).with_alpha(0.25));
                    gfx.rt
                        .PushAxisAlignedClip(&d2d(&shown), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                    gfx.layout_at(&l, cx + CARD_PAD, top, pal.fg.with_alpha(0.85));
                    gfx.rt.PopAxisAlignedClip();
                }
                let by = card.bottom() - CARD_PAD - BUTTON_H;
                let right = card.right() - CARD_PAD;
                match state {
                    AskState::Waiting => {
                        let allow = Rect::new(right - BUTTON_W, by, BUTTON_W, BUTTON_H);
                        let deny = Rect::new(allow.x - 8.0 - BUTTON_W, by, BUTTON_W, BUTTON_H);
                        gfx.fill_round(&allow, 8.0, pal.fg.with_alpha(0.92));
                        gfx.text_in("Allow", &gfx.button, &allow, pal.bg);
                        gfx.stroke_round(&deny, 8.0, pal.fg.with_alpha(0.35), 1.0);
                        gfx.text_in("Deny", &gfx.button, &deny, pal.fg);
                        hits.push((allow, Hit::Answer(request_id.clone(), true)));
                        hits.push((deny, Hit::Answer(request_id.clone(), false)));
                    }
                    _ => {
                        let said = look::ask_said(*state);
                        let r = Rect::new(cx + CARD_PAD, by, inner, BUTTON_H);
                        gfx.text_in(said, &gfx.line, &r, pal.dim);
                    }
                }
            }
            Block::Footer {
                duration_ms,
                cost_usd,
                ending,
                ..
            } => {
                let ink = match ending {
                    Ending::Failed => theme::error().with_alpha(0.8),
                    Ending::Done | Ending::Stopped => pal.faint,
                };
                let line = look::footer_line(*duration_ms, *cost_usd, *ending);
                gfx.text_in(&line, &gfx.small, &Rect::new(cx, y, cw, FOOTER_H), ink);
                // A short rule under the turn, in the project's colour.
                gfx.fill(
                    &Rect::new(cx, y + FOOTER_H + 8.0, 28.0, 1.0),
                    pal.accent.with_alpha(0.35),
                );
            }
            Block::Raw(t) => {
                if let Some((l, _, _)) = self.laid(gfx, i, 4, &raw_text(t), &gfx.italic, cw) {
                    gfx.layout_at(&l, cx, y, pal.faint);
                }
            }
            _ => {}
        }
    }

    /// A folded block's one line: its icon, its text, and a chevron saying
    /// a click opens it.
    #[allow(clippy::too_many_arguments)]
    unsafe fn fold_line(
        &self,
        gfx: &Gfx,
        pal: &Palette,
        (icon, mark): (char, Color),
        text: &str,
        open: bool,
        r: &Rect,
        ink: Color,
    ) {
        let i = Rect::new(r.x, r.y, 16.0, r.h);
        gfx.text_in(&icon.to_string(), &gfx.icon, &i, mark);
        let tr = Rect::new(r.x + 22.0, r.y, r.w - 44.0, r.h);
        gfx.text_in(text, &gfx.line, &tr, ink);
        let chevron = if open { '\u{E70D}' } else { '\u{E76C}' };
        let c = Rect::new(r.right() - 16.0, r.y, 16.0, r.h);
        gfx.text_in(
            &chevron.to_string(),
            &gfx.icon,
            &c,
            pal.faint.with_alpha(0.3),
        );
    }

    /// The composer at the bottom: the text being written, its caret and
    /// selection, and the button that sends it, or stops Claude while it
    /// works. Returns its box.
    #[allow(clippy::too_many_arguments)]
    unsafe fn composer(
        &self,
        gfx: &Gfx,
        chat: &mut Chat,
        pal: &Palette,
        cx: f32,
        cw: f32,
        h: f32,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> Rect {
        let gpu = &self.shared.gpu;
        let inner_w = (cw - 2.0 * COMPOSER_PAD - SEND - 8.0).max(10.0);
        let f = &chat.composer;
        let empty = f.text.is_empty();
        let shown = if empty { "M" } else { f.text.as_str() };
        let Ok(l) = render::wrapped(gpu, &gfx.text, shown, inner_w) else {
            return Rect::new(cx, h, cw, 0.0);
        };
        let (_, th) = render::text_size(&l);
        let line_h = render::wrapped(gpu, &gfx.text, "M", inner_w)
            .map(|l| render::text_size(&l).1)
            .unwrap_or(20.0);
        let view_h = th.clamp(line_h, line_h * COMPOSER_LINES);
        let bh = view_h + 2.0 * COMPOSER_PAD;
        let r = Rect::new(cx, h - COMPOSER_BOTTOM - bh, cw, bh);
        let focused = self.has_keyboard();
        gfx.fill_round(&r, 16.0, pal.bg.mix(pal.fg, 0.07).with_alpha(0.94));
        let edge = if focused {
            pal.accent.with_alpha(0.6)
        } else {
            pal.fg.with_alpha(0.14)
        };
        gfx.stroke_round(&r, 16.0, edge, 1.0);
        let (ox, oy) = (r.x + COMPOSER_PAD, r.y + COMPOSER_PAD);
        let text_box = Rect::new(ox, oy, inner_w, view_h);
        if empty {
            let hint = if chat.folio.busy {
                "Claude is working. Esc stops it."
            } else {
                "Message Claude. Enter sends, Shift+Enter breaks the line."
            };
            gfx.text_in(
                hint,
                &gfx.line,
                // Clear of the caret standing at its start.
                &Rect::new(ox + 4.0, oy, inner_w - 4.0, line_h),
                pal.faint,
            );
            chat.composer_scroll = 0.0;
        } else {
            let caret = render::caret_at(&l, field::utf16_at(&f.text, f.caret));
            chat.composer_scroll =
                field::follow(chat.composer_scroll, caret.y, caret.h, view_h, th);
        }
        let sy = oy - chat.composer_scroll;
        gfx.rt
            .PushAxisAlignedClip(&d2d(&text_box), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        if !empty {
            let (a, b) = f.selection();
            let (a16, b16) = (field::utf16_at(&f.text, a), field::utf16_at(&f.text, b));
            for s in render::range_rects(&l, a16, b16 - a16) {
                gfx.fill(
                    &Rect::new(ox + s.x, sy + s.y, s.w, s.h),
                    pal.accent.with_alpha(0.35),
                );
            }
            gfx.layout_at(&l, ox, sy, pal.fg);
        }
        if focused {
            let c = render::caret_at(
                &l,
                if empty {
                    0
                } else {
                    field::utf16_at(&f.text, f.caret)
                },
            );
            gfx.fill(
                &Rect::new(ox + c.x, sy + c.y, 1.5, c.h),
                pal.accent.mix(pal.fg, 0.4),
            );
        }
        gfx.rt.PopAxisAlignedClip();
        *self.composer.borrow_mut() = (!empty).then_some((l, ox, sy));
        hits.push((r, Hit::Composer));

        let send = Rect::new(
            r.right() - COMPOSER_PAD - SEND,
            r.bottom() - COMPOSER_PAD / 2.0 - SEND - 2.0,
            SEND,
            SEND,
        );
        let (glyph, fill, hit) = if chat.folio.busy {
            ('\u{E71A}', pal.fg.with_alpha(0.85), Hit::Stop)
        } else if look::prompt_of(&f.text).is_some() && !chat.ended {
            ('\u{E724}', pal.accent, Hit::Send)
        } else {
            ('\u{E724}', pal.fg.with_alpha(0.12), Hit::Send)
        };
        gfx.fill_round(&send, SEND / 2.0, fill);
        gfx.text_in(&glyph.to_string(), &gfx.icon, &send, pal.bg);
        hits.push((send, hit));
        r
    }

    /// Whether the keyboard is in the pane this conversation is laid in.
    fn has_keyboard(&self) -> bool {
        unsafe {
            let focus = windows::Win32::UI::Input::KeyboardAndMouse::GetFocus();
            GetParent(self.hwnd()).is_ok_and(|p| p == focus)
        }
    }

    /// Moves the composer's caret a line up or down. None when it is on
    /// the first or the last line already.
    fn caret_line(&self, key: &str, up: bool, extend: bool) -> Option<()> {
        let held = self.composer.borrow();
        let (l, _, _) = held.as_ref()?;
        with(key, |c| {
            let f = &mut c.composer;
            let caret = render::caret_at(l, field::utf16_at(&f.text, f.caret));
            let y = if up {
                caret.y - caret.h / 2.0
            } else {
                caret.y + caret.h * 1.5
            };
            let (_, th) = render::text_size(l);
            if y < 0.0 || y > th {
                return None;
            }
            let at = render::offset_at(l, caret.x, y);
            f.set_caret(field::byte_at(&f.text, at), extend);
            Some(())
        })?
    }

    /// The composer's text under a point in the view, as a byte offset.
    fn composer_at(&self, x: f32, y: f32) -> Option<usize> {
        let held = self.composer.borrow();
        let (l, ox, oy) = held.as_ref()?;
        let at = render::offset_at(l, x - ox, y - oy);
        with(&self.key, |c| field::byte_at(&c.composer.text, at))
    }

    fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        // Buttons are pushed after what they sit on, so the last wins.
        (self.hits.borrow().iter().rev())
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, h)| h.clone())
    }

    fn on_press(&self, lparam: LPARAM, double: bool) {
        let hwnd = self.hwnd();
        unsafe {
            // The pane keeps the keyboard, so typing goes on into the
            // composer wherever the click was.
            if let Ok(pane) = GetParent(hwnd) {
                let _ = SetFocus(Some(pane));
            }
        }
        let (x, y) = self.dip(lparam);
        let key = self.key.clone();
        match self.hit_at(x, y) {
            Some(Hit::Answer(id, allow)) => answer(&key, &id, allow),
            Some(Hit::Fold(i)) => {
                with(&key, |c| {
                    if !c.open.remove(&i) {
                        c.open.insert(i);
                    }
                    c.follow = false;
                });
            }
            Some(Hit::Send) => send_prompt(&key),
            Some(Hit::Stop) => interrupt(&key),
            Some(Hit::Composer) => {
                if let Some(at) = self.composer_at(x, y) {
                    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                    with(&key, |c| {
                        if double {
                            c.composer.select_word(at);
                        } else {
                            c.composer.set_caret(at, shift);
                        }
                    });
                    if !double {
                        self.selecting.set(true);
                        unsafe {
                            SetCapture(hwnd);
                        }
                    }
                }
            }
            None => {}
        }
        redraw(&key);
    }

    fn on_drag(&self, lparam: LPARAM) {
        if !self.selecting.get() {
            return;
        }
        let (x, y) = self.dip(lparam);
        if let Some(at) = self.composer_at(x, y) {
            with(&self.key, |c| c.composer.set_caret(at, true));
            redraw(&self.key);
        }
    }

    fn cursor(&self) -> bool {
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
            let _ = ScreenToClient(self.hwnd(), &mut p);
        }
        let s = self.scale();
        let id = match self.hit_at(p.x as f32 / s, p.y as f32 / s) {
            Some(Hit::Composer) => IDC_IBEAM,
            Some(_) => IDC_HAND,
            None => IDC_ARROW,
        };
        unsafe {
            if let Ok(c) = LoadCursorW(None, id) {
                SetCursor(Some(c));
            }
        }
        true
    }

    fn handle(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                self.paint();
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd()), None);
                }
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_SIZE => {
                let w = (lparam.0 & 0xffff) as u32;
                let h = ((lparam.0 >> 16) & 0xffff) as u32;
                if let Some(g) = self.gfx.borrow().as_ref() {
                    let _ = render::resize_target(&g.rt, w, h);
                }
                unsafe {
                    let _ = InvalidateRect(Some(self.hwnd()), None, false);
                }
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TICK => {
                if self.seen() {
                    unsafe {
                        let _ = InvalidateRect(Some(self.hwnd()), None, false);
                    }
                } else {
                    self.clock(false);
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                self.on_press(lparam, msg == WM_LBUTTONDBLCLK);
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                self.on_drag(lparam);
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                if self.selecting.replace(false) {
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                }
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                self.selecting.set(false);
                Some(LRESULT(0))
            }
            WM_SETCURSOR if (lparam.0 & 0xffff) as u32 == HTCLIENT => {
                self.cursor().then_some(LRESULT(1))
            }
            WM_DESTROY => {
                self.clock(false);
                let hwnd = self.hwnd();
                with(&self.key, |c| {
                    if c.view == Some(hwnd) {
                        c.view = None;
                    }
                });
                None
            }
            _ => None,
        }
    }
}

/// The room left under a block before the next.
fn gap_after(b: &Block) -> f32 {
    match b {
        Block::Footer { .. } => GAP + 16.0,
        Block::Thinking(_) | Block::Tools(_) => GAP / 2.0,
        _ => GAP,
    }
}

fn raw_text(t: &str) -> String {
    if t.chars().count() <= RAW_MAX {
        return t.to_string();
    }
    let mut s: String = t.chars().take(RAW_MAX).collect();
    s.push('\u{2026}');
    s
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        let view = cs.lpCreateParams as *const View;
        if !view.is_null() {
            (*view).hwnd.set(hwnd);
        }
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, view as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut View;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        // The window made its View its own when it was created.
        drop(Box::from_raw(ptr));
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    match (*ptr).handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
