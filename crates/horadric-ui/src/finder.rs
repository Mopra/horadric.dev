//! The universal search: the field at the bottom of the usage window and
//! the list of conversations it finds, dropped beside it as one types.
//!
//! The usage window never takes the focus by itself, so a click on the
//! field brings it forward on purpose, and it lets go again on Escape, on a
//! pick, or when the focus goes elsewhere. The list never takes the focus
//! or the mouse, as the address field's suggestions do not: typing goes on
//! while it is open, the arrow keys move its choice, and a click on a row
//! tells the usage window. A pick carries the conversation on where it ran,
//! the way the quest log does, so no terminal has to be opened to find it.
//!
//! Searching reads transcripts and can take a while the first time, so it
//! runs on a thread of its own. One thread at most: a query typed while one
//! runs waits for it and replaces any other waiting, so only the newest is
//! looked for next.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use horadric_hooks::search::Found;
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MonitorFromRect, ValidateRect, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::Ime::{
    ImmGetContext, ImmReleaseContext, ImmSetCandidateWindow, ImmSetCompositionWindow,
    CANDIDATEFORM, CFS_EXCLUDE, CFS_POINT, COMPOSITIONFORM,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE,
    VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCaretBlinkTime, GetForegroundWindow,
    GetWindowLongPtrW, IsWindow, KillTimer, LoadCursorW, PostMessageW, RegisterClassW,
    SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, CREATESTRUCTW,
    GWLP_USERDATA, IDC_ARROW, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOWNOACTIVATE,
    WM_APP, WM_CHAR, WM_ERASEBKGND, WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WM_KILLFOCUS,
    WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY,
    WM_PAINT, WM_SIZE, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::app::{self, Input};
use crate::field::{self, Field};
use crate::layout::{self, FinderLayout, Rect};
use crate::render::{self, FieldLook, FinderScene, FoundLook, Target};
use crate::window::Shared;
use crate::{backdrop, clipboard, history};

pub(crate) const CLASS: PCWSTR = w!("HoradricFinder");

/// Posted to the usage window when a search's answer is in.
pub const WM_FINDER_FOUND: u32 = WM_APP + 70;
/// Posted to the usage window when a row of the list is clicked, its place
/// among everything found in `wparam`.
pub const WM_FINDER_PICK: u32 = WM_APP + 71;

/// Timers on the usage window: the caret's blink, and the pause after a
/// key before the search runs, so a word typed fast is one search.
const BLINK: usize = 0x5EA1;
const DEBOUNCE: usize = 0x5EA2;
const DEBOUNCE_MS: u32 = 120;

/// What the field says while empty.
pub const PLACEHOLDER: &str = "Search conversations";
const SEARCHING: &str = "Searching\u{2026}";
const NOTHING: &str = "No conversations found";

/// The most conversations asked for, and the most rows shown at once.
const LIMIT: usize = 60;
const SHOWN: usize = 10;
/// How wide the list is, in DIPs: wide enough for a title and its words.
const WIDTH: f32 = 560.0;
/// The gap between the field and the list, in DIPs.
const GAP: f32 = 4.0;
/// The longest query, in characters.
const MAX_QUERY: usize = 200;

const WM_MOUSELEAVE: u32 = 0x02A3;

/// Where the field is, which the usage window knows: its well on screen
/// in pixels for the list to hang from, its text in the window in DIPs,
/// and the window's scale.
#[derive(Debug, Clone, Copy)]
pub struct Anchor {
    pub field: RECT,
    pub inner: Rect,
    pub scale: f32,
}

/// What the usage window draws of the field this frame.
pub struct Look {
    pub text: windows::Win32::Graphics::DirectWrite::IDWriteTextLayout,
    pub placeholder: bool,
    pub scroll: f32,
    pub selection: Vec<Rect>,
    pub caret: Option<Rect>,
    pub focused: bool,
}

impl Look {
    pub fn field(&self, rect: Rect) -> FieldLook<'_> {
        FieldLook {
            rect,
            text: &self.text,
            placeholder: self.placeholder.then_some(PLACEHOLDER),
            multiline: false,
            scroll: (self.scroll, 0.0),
            selection: self.selection.clone(),
            caret: self.caret,
            focused: self.focused,
        }
    }
}

/// The search field's state, kept by the usage window.
pub struct Search {
    shared: Rc<Shared>,
    host: Cell<HWND>,
    field: RefCell<Field>,
    /// The field has the keyboard.
    typing: Cell<bool>,
    lit: Cell<bool>,
    scroll: Cell<f32>,
    /// The first half of a character typed as two UTF-16 units.
    high: Cell<Option<u16>>,
    /// The window that had the focus before the field took it, which
    /// Escape gives it back to.
    before: Cell<HWND>,
    /// Counts the searches asked for, so an answer to an older one is
    /// dropped.
    seq: Cell<u64>,
    /// The query last searched for, so moving the caret searches nothing.
    asked: RefCell<Option<String>>,
    searching: Cell<bool>,
    seeker: Seeker,
    found: RefCell<Vec<Found>>,
    list: RefCell<Option<Box<List>>>,
    /// The caret on screen in client pixels, for the input method.
    ime_at: Cell<Option<[i32; 4]>>,
}

impl Search {
    pub fn new(shared: Rc<Shared>) -> Self {
        Search {
            shared,
            host: Cell::new(HWND::default()),
            field: RefCell::new(Field::new("", false, MAX_QUERY)),
            typing: Cell::new(false),
            lit: Cell::new(false),
            scroll: Cell::new(0.0),
            high: Cell::new(None),
            before: Cell::new(HWND::default()),
            seq: Cell::new(0),
            asked: RefCell::new(None),
            searching: Cell::new(false),
            seeker: Seeker::new(horadric_hooks::search::search),
            found: RefCell::new(Vec::new()),
            list: RefCell::new(None),
            ime_at: Cell::new(None),
        }
    }

    /// The window the field is in, set once it exists.
    pub fn set_host(&self, hwnd: HWND) {
        self.host.set(hwnd);
    }

    pub fn typing(&self) -> bool {
        self.typing.get()
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.host.get()), None, false);
        }
    }

    /// Takes the keyboard for the field, the usage window brought forward
    /// to get it, and opens the list for what is typed. `at`, a point in
    /// the window in DIPs, puts the caret there.
    pub fn begin(&self, anchor: &Anchor, at: Option<(f32, bool)>) {
        let hwnd = self.host.get();
        if !self.typing.replace(true) {
            unsafe {
                let fg = GetForegroundWindow();
                self.before
                    .set(if fg == hwnd { HWND::default() } else { fg });
                let _ = SetForegroundWindow(hwnd);
                let _ = SetFocus(Some(hwnd));
                SetTimer(Some(hwnd), BLINK, GetCaretBlinkTime(), None);
            }
            *self.list.borrow_mut() = List::open(self.shared.clone(), hwnd, anchor.field);
            self.show_found();
            // Asked again each time, since conversations went on meanwhile.
            self.asked.replace(None);
            self.run();
        }
        if let Some((x, extend)) = at {
            let offset = self.offset_at(anchor, x);
            self.edit(|f| f.set_caret(offset, extend));
        }
        self.lit.set(true);
        self.invalidate();
    }

    /// Lets go of the keyboard and closes the list. `clear` empties the
    /// field, and `restore` gives the focus back to where it was before.
    /// A click elsewhere keeps the text, so looking something up does not
    /// cost the query.
    pub fn leave(&self, clear: bool, restore: bool) {
        let hwnd = self.host.get();
        if clear {
            *self.field.borrow_mut() = Field::new("", false, MAX_QUERY);
            self.scroll.set(0.0);
        }
        if !self.typing.replace(false) {
            return self.invalidate();
        }
        // An answer still on its way is for a list that is gone.
        self.seq.set(self.seq.get() + 1);
        self.searching.set(false);
        unsafe {
            let _ = KillTimer(Some(hwnd), BLINK);
            let _ = KillTimer(Some(hwnd), DEBOUNCE);
        }
        self.list.borrow_mut().take();
        self.found.borrow_mut().clear();
        let before = self.before.replace(HWND::default());
        if restore && !before.is_invalid() && unsafe { IsWindow(Some(before)) }.as_bool() {
            unsafe {
                let _ = SetForegroundWindow(before);
            }
        }
        self.invalidate();
    }

    /// The list hangs from the field again after the window moved.
    pub fn reanchor(&self, anchor: &Anchor) {
        if let Some(l) = self.list.borrow().as_ref() {
            l.anchor(anchor.field);
        }
    }

    /// The field's text laid out, and where its caret and selection are.
    pub fn look(&self, anchor: &Anchor) -> Option<Look> {
        let f = self.field.borrow();
        let inner = anchor.inner;
        let text = render::field_layout(&self.shared.gpu, &f.text, &inner, false).ok()?;
        let caret = render::caret_at(&text, field::utf16_at(&f.text, f.caret));
        let (w, _) = render::text_size(&text);
        let scroll = field::follow(self.scroll.get(), caret.x, 2.0, inner.w, w + 2.0);
        self.scroll.set(scroll);
        let (a, b) = f.selection();
        let (a, b) = (field::utf16_at(&f.text, a), field::utf16_at(&f.text, b));
        let selection = render::range_rects(&text, a, b - a);
        let typing = self.typing.get();
        let s = anchor.scale;
        let px = |v: f32| (v * s).round() as i32;
        let x = inner.x + caret.x - scroll;
        self.ime_at
            .set(Some([px(x), px(inner.y), px(x + 1.0), px(inner.bottom())]));
        Some(Look {
            text,
            placeholder: f.text.is_empty(),
            scroll,
            selection: if typing { selection } else { Vec::new() },
            caret: (typing && self.lit.get()).then_some(caret),
            focused: typing,
        })
    }

    /// The place in the text nearest `x`, a point in the window in DIPs.
    fn offset_at(&self, anchor: &Anchor, x: f32) -> usize {
        let f = self.field.borrow();
        let inner = anchor.inner;
        match render::field_layout(&self.shared.gpu, &f.text, &inner, false) {
            Ok(l) => {
                let at = render::offset_at(&l, x - inner.x + self.scroll.get(), inner.h / 2.0);
                field::byte_at(&f.text, at)
            }
            Err(_) => f.text.len(),
        }
    }

    /// Changes the field, and searches again shortly when the text changed.
    fn edit(&self, change: impl FnOnce(&mut Field)) {
        let before = self.field.borrow().text.clone();
        change(&mut self.field.borrow_mut());
        self.lit.set(true);
        let hwnd = self.host.get();
        unsafe {
            // The blink starts over, so the caret shows while keys go in.
            SetTimer(Some(hwnd), BLINK, GetCaretBlinkTime(), None);
            if self.field.borrow().text != before {
                SetTimer(Some(hwnd), DEBOUNCE, DEBOUNCE_MS, None);
            }
        }
        self.invalidate();
    }

    /// Asks for what the field says now, unless that was asked last.
    fn run(&self) {
        let query = self.field.borrow().text.trim().to_string();
        if self.asked.borrow().as_deref() == Some(query.as_str()) {
            return;
        }
        self.asked.replace(Some(query.clone()));
        let seq = self.seq.get() + 1;
        self.seq.set(seq);
        self.searching.set(true);
        let hwnd = self.host.get().0 as isize;
        self.seeker.ask(seq, &query, move || unsafe {
            let _ = PostMessageW(
                Some(HWND(hwnd as *mut c_void)),
                WM_FINDER_FOUND,
                WPARAM(0),
                LPARAM(0),
            );
        });
        self.show_found();
    }

    /// Takes an answer that came in, if it is to the newest question.
    fn take_found(&self) {
        if !self.typing.get() {
            return;
        }
        if let Some(found) = self.seeker.take(self.seq.get()) {
            self.searching.set(false);
            *self.found.borrow_mut() = found;
            self.show_found();
            if let Some(l) = self.list.borrow().as_ref() {
                l.restart();
            }
        }
    }

    /// Puts what was found in the list, or what it says while there is
    /// nothing.
    fn show_found(&self) {
        let list = self.list.borrow();
        let Some(l) = list.as_ref() else {
            return;
        };
        let held = held(&self.shared);
        let now = SystemTime::now();
        let looks = self
            .found
            .borrow()
            .iter()
            .map(|f| look(f, now, &held))
            .collect();
        let note = if self.searching.get() {
            SEARCHING
        } else {
            NOTHING
        };
        l.set(looks, note);
    }

    /// Carries on the `i`th conversation found, where it ran.
    fn pick(&self, i: usize) {
        let Some(f) = self.found.borrow().get(i).cloned() else {
            return;
        };
        // The stage comes forward with the conversation, so the focus is
        // not handed back.
        self.leave(true, false);
        app::push(Input::Resume(f.cwd, f.past));
    }

    fn step(&self, by: i32) {
        if let Some(l) = self.list.borrow().as_ref() {
            l.step(by);
        }
    }

    fn page(&self) -> i32 {
        self.list.borrow().as_ref().map_or(1, |l| l.shown() as i32)
    }

    fn key(&self, vk: u16) {
        let down = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) } < 0;
        let (ctrl, shift) = (down(VK_CONTROL), down(VK_SHIFT));
        match VIRTUAL_KEY(vk) {
            VK_ESCAPE => self.leave(true, true),
            VK_RETURN => {
                let chosen = self.list.borrow().as_ref().and_then(|l| l.chosen());
                if let Some(i) = chosen {
                    self.pick(i);
                }
            }
            VK_UP => self.step(-1),
            VK_DOWN => self.step(1),
            VK_PRIOR => self.step(-self.page()),
            VK_NEXT => self.step(self.page()),
            VK_LEFT => self.edit(|f| f.left(ctrl, shift)),
            VK_RIGHT => self.edit(|f| f.right(ctrl, shift)),
            VK_HOME => self.edit(|f| f.home(true, shift)),
            VK_END => self.edit(|f| f.end(true, shift)),
            VK_BACK => self.edit(|f| f.backspace(ctrl)),
            VK_DELETE if shift => self.edit(|f| clipboard::set_text(&f.cut())),
            VK_DELETE => self.edit(|f| f.delete(ctrl)),
            _ if !ctrl => {}
            k => match char::from_u32(k.0 as u32) {
                Some('A') => self.edit(Field::select_all),
                Some('C') => {
                    let f = self.field.borrow();
                    if !f.selected().is_empty() {
                        clipboard::set_text(f.selected());
                    }
                }
                Some('X') => self.edit(|f| {
                    if f.caret != f.anchor {
                        clipboard::set_text(&f.cut());
                    }
                }),
                Some('V') => {
                    if let Some(s) = clipboard::get_text() {
                        self.edit(|f| f.insert(&s));
                    }
                }
                Some('Z') => self.edit(|f| {
                    f.undo();
                }),
                Some('Y') => self.edit(|f| {
                    f.redo();
                }),
                _ => {}
            },
        }
    }

    /// A character typed. The keys that edit came as key downs, so every
    /// control character here is dropped.
    fn typed(&self, unit: u16) {
        let s = match unit {
            0xD800..=0xDBFF => {
                self.high.set(Some(unit));
                return;
            }
            0xDC00..=0xDFFF => match self.high.take() {
                Some(h) => String::from_utf16_lossy(&[h, unit]),
                None => return,
            },
            u if u < 0x20 || u == 0x7F => return,
            u => String::from_utf16_lossy(&[u]),
        };
        self.edit(|f| f.insert(&s));
    }

    /// Puts the input method's composition at the caret.
    fn place_ime(&self) {
        let Some([left, top, right, bottom]) = self.ime_at.get() else {
            return;
        };
        let hwnd = self.host.get();
        let composition = COMPOSITIONFORM {
            dwStyle: CFS_POINT,
            ptCurrentPos: POINT { x: left, y: top },
            rcArea: RECT::default(),
        };
        let candidates = CANDIDATEFORM {
            dwIndex: 0,
            dwStyle: CFS_EXCLUDE,
            ptCurrentPos: POINT { x: left, y: bottom },
            rcArea: RECT {
                left,
                top,
                right,
                bottom,
            },
        };
        unsafe {
            let imc = ImmGetContext(hwnd);
            if imc.is_invalid() {
                return;
            }
            let _ = ImmSetCompositionWindow(imc, &composition);
            let _ = ImmSetCandidateWindow(imc, &candidates);
            let _ = ImmReleaseContext(hwnd, imc);
        }
    }

    /// The usage window's messages the search answers: the keys and the
    /// focus while it types, its timers, and the list's and the thread's
    /// news.
    pub fn handle(&self, msg: u32, wparam: WPARAM) -> Option<LRESULT> {
        match msg {
            WM_FINDER_FOUND => {
                self.take_found();
                Some(LRESULT(0))
            }
            WM_FINDER_PICK => {
                self.pick(wparam.0);
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == BLINK => {
                self.lit.set(!self.lit.get());
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == DEBOUNCE => {
                unsafe {
                    let _ = KillTimer(Some(self.host.get()), DEBOUNCE);
                }
                if self.typing.get() {
                    self.run();
                }
                Some(LRESULT(0))
            }
            _ if !self.typing.get() => None,
            WM_KEYDOWN => {
                self.key(wparam.0 as u16);
                Some(LRESULT(0))
            }
            WM_CHAR => {
                self.typed(wparam.0 as u16);
                Some(LRESULT(0))
            }
            WM_KILLFOCUS => {
                self.leave(false, false);
                None
            }
            WM_IME_STARTCOMPOSITION => {
                self.place_ime();
                None
            }
            _ => None,
        }
    }
}

/// The conversations tiles hold, running or paused, which a pick shows
/// rather than starts a second time.
fn held(shared: &Shared) -> Vec<String> {
    shared
        .registry
        .lock()
        .map(|r| {
            r.all()
                .filter_map(|s| s.claude_session_id.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// How the list shows a conversation found.
fn look(f: &Found, now: SystemTime, held: &[String]) -> FoundLook {
    let ago = now.duration_since(f.past.modified).unwrap_or_default();
    let snippet = if f.snippet.trim().is_empty() {
        &f.past.asked
    } else {
        &f.snippet
    };
    let title = one_line(&f.past.title.text);
    FoundLook {
        title: if title.is_empty() {
            "Untitled conversation".to_string()
        } else {
            title
        },
        meta: meta(&f.project, &history::ago_text(ago)),
        snippet: one_line(snippet),
        held: held.contains(&f.past.id),
    }
}

/// The project and the age, as the right of a row says them.
fn meta(project: &str, ago: &str) -> String {
    match project.trim() {
        "" => ago.to_string(),
        p => format!("{p} \u{00B7} {ago}"),
    }
}

/// Text on one line: its line breaks and runs of spaces made one space.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The choice moved `by` rows among `n`, from none to the first going down
/// and the last going up, and stopping at either end.
fn step(chosen: Option<usize>, by: i32, n: usize) -> Option<usize> {
    if n == 0 {
        return None;
    }
    let last = n as i64 - 1;
    let next = match chosen {
        None if by > 0 => 0,
        None => last,
        Some(i) => (i as i64 + by as i64).clamp(0, last),
    };
    Some(next as usize)
}

/// The first row shown so that `chosen` is among the `shown` rows.
fn follow(top: usize, chosen: usize, shown: usize) -> usize {
    if chosen < top {
        chosen
    } else if chosen >= top + shown {
        chosen + 1 - shown.max(1)
    } else {
        top
    }
}

/// The first row shown after scrolling `by` rows, kept within the list.
fn scrolled(top: usize, by: i32, shown: usize, n: usize) -> usize {
    let most = n.saturating_sub(shown) as i64;
    (top as i64 + by as i64).clamp(0, most.max(0)) as usize
}

/// Where a list `size` big goes for a field at `field` on a screen whose
/// work area is `work`, all in pixels: under the field when it fits, over
/// it when only that fits, else on the side with more room, and from the
/// field's left edge, moved in to stay on the screen.
fn place(field: RECT, (w, h): (i32, i32), work: RECT, gap: i32) -> (i32, i32) {
    let below = field.bottom + gap;
    let above = field.top - gap - h;
    let y = if below + h <= work.bottom {
        below
    } else if above >= work.top {
        above
    } else if work.bottom - field.bottom >= field.top - work.top {
        below.min(work.bottom - h)
    } else {
        above.max(work.top)
    };
    let x = field.left.min(work.right - w).max(work.left);
    (x, y)
}

/// The most room the list has, in pixels, on the roomier side of the field.
fn room(field: RECT, work: RECT, gap: i32) -> i32 {
    (work.bottom - field.bottom).max(field.top - work.top) - gap
}

/// The coalescing search thread. Holds at most one query waiting and one
/// answer, each tagged with the number it was asked under.
pub struct Seeker {
    job: Arc<Mutex<Job>>,
    engine: fn(&str, usize) -> Vec<Found>,
}

#[derive(Default)]
struct Job {
    want: Option<(u64, String)>,
    running: bool,
    done: Option<(u64, Vec<Found>)>,
}

fn locked(job: &Mutex<Job>) -> MutexGuard<'_, Job> {
    job.lock().unwrap_or_else(|e| e.into_inner())
}

impl Seeker {
    pub fn new(engine: fn(&str, usize) -> Vec<Found>) -> Self {
        Seeker {
            job: Arc::new(Mutex::new(Job::default())),
            engine,
        }
    }

    /// Searches for `query` as number `seq`, then calls `wake`. A thread
    /// already searching takes it up when it is done, in place of any
    /// other still waiting.
    pub fn ask(&self, seq: u64, query: &str, wake: impl Fn() + Send + 'static) {
        let mut j = locked(&self.job);
        j.want = Some((seq, query.to_string()));
        if j.running {
            return;
        }
        j.running = true;
        drop(j);
        let job = self.job.clone();
        let engine = self.engine;
        let spawned = std::thread::Builder::new()
            .name("horadric-search".into())
            .spawn(move || loop {
                let (seq, query) = {
                    let mut j = locked(&job);
                    match j.want.take() {
                        Some(w) => w,
                        None => {
                            j.running = false;
                            return;
                        }
                    }
                };
                let found = engine(&query, LIMIT);
                let mut j = locked(&job);
                // A newer question came meanwhile: only its answer is
                // wanted, and it is next.
                if j.want.is_none() {
                    j.done = Some((seq, found));
                    drop(j);
                    wake();
                }
            });
        if spawned.is_err() {
            locked(&self.job).running = false;
        }
    }

    /// The answer to question `seq`, once. An answer to an older one is
    /// dropped.
    pub fn take(&self, seq: u64) -> Option<Vec<Found>> {
        match locked(&self.job).done.take() {
            Some((s, found)) if s == seq => Some(found),
            _ => None,
        }
    }
}

/// The list of conversations found, dropped beside the field.
pub struct List {
    hwnd: HWND,
    /// The usage window, told of a click.
    owner: HWND,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    layout: RefCell<FinderLayout>,
    rows: RefCell<Vec<FoundLook>>,
    note: Cell<&'static str>,
    /// Counted among every row, not only those shown.
    chosen: Cell<Option<usize>>,
    top: Cell<usize>,
    /// How many rows fit on the screen at once.
    fits: Cell<usize>,
    field: Cell<RECT>,
}

pub fn register_class() -> Result<()> {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: GetModuleHandleW(None)?.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        Ok(())
    }
}

/// Where the list goes and how it is laid out, in pixels and DIPs.
struct Geometry {
    at: (i32, i32),
    size: (i32, i32),
    layout: FinderLayout,
    fits: usize,
}

fn work_area(r: RECT) -> RECT {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        let _ = GetMonitorInfoW(MonitorFromRect(&r, MONITOR_DEFAULTTONEAREST), &mut info);
    }
    info.rcWork
}

fn geometry(shared: &Shared, owner: HWND, field: RECT, rows: usize) -> Geometry {
    let s = unsafe { GetDpiForWindow(owner) }.max(96) as f32 / 96.0;
    let work = work_area(field);
    let gap = (GAP * s).round() as i32;
    let w = ((WIDTH * s).round() as i32).min(work.right - work.left - 16);
    let fits = layout::finder_fits(&shared.metrics, room(field, work, gap) as f32 / s).min(SHOWN);
    let layout = layout::finder(&shared.metrics, w as f32 / s, rows.min(fits));
    let h = (layout.size.1 * s).round() as i32;
    Geometry {
        at: place(field, (w, h), work, gap),
        size: (w, h),
        layout,
        fits,
    }
}

impl List {
    /// Opens it empty, saying it searches, from the field at `field` on
    /// screen.
    fn open(shared: Rc<Shared>, owner: HWND, field: RECT) -> Option<Box<Self>> {
        let g = geometry(&shared, owner, field, 0);
        let mut win = Box::new(List {
            hwnd: HWND::default(),
            owner,
            shared,
            target: RefCell::new(None),
            layout: RefCell::new(g.layout),
            rows: RefCell::new(Vec::new()),
            note: Cell::new(SEARCHING),
            chosen: Cell::new(None),
            top: Cell::new(0),
            fits: Cell::new(g.fits),
            field: Cell::new(field),
        });
        unsafe {
            // Born where it shows and at its size: a window that starts
            // elsewhere and moves has been seen not to paint.
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS,
                w!("Horadric search"),
                WS_POPUP,
                g.at.0,
                g.at.1,
                g.size.0,
                g.size.1,
                Some(owner),
                None,
                Some(GetModuleHandleW(None).ok()?.into()),
                Some(&*win as *const List as *const c_void),
            )
            .ok()?;
            win.hwnd = hwnd;
            let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            backdrop::border(hwnd, None);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        Some(win)
    }

    /// Lays out again and moves to fit the rows there are now.
    fn fit(&self) {
        let n = self.rows.borrow().len();
        let g = geometry(&self.shared, self.owner, self.field.get(), n);
        self.fits.set(g.fits);
        *self.layout.borrow_mut() = g.layout;
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                g.at.0,
                g.at.1,
                g.size.0,
                g.size.1,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
        self.invalidate();
    }

    fn anchor(&self, field: RECT) {
        if self.field.replace(field) != field {
            self.fit();
        }
    }

    /// Lists `rows`, or says `note` while there are none. The choice and
    /// the scroll stay where they were as far as the rows allow, since an
    /// answer can come in while the arrows are moving.
    fn set(&self, rows: Vec<FoundLook>, note: &'static str) {
        let n = rows.len();
        let same = *self.rows.borrow() == rows && self.note.get() == note;
        if same {
            return;
        }
        *self.rows.borrow_mut() = rows;
        self.note.set(note);
        self.chosen
            .set(self.chosen.get().map(|c| c.min(n.saturating_sub(1))));
        if n == 0 {
            self.chosen.set(None);
        }
        self.top
            .set(scrolled(self.top.get(), 0, self.shown_of(n), n));
        self.fit();
    }

    /// The first row chosen and shown, for a fresh answer.
    fn restart(&self) {
        let n = self.rows.borrow().len();
        self.chosen.set((n > 0).then_some(0));
        self.top.set(0);
        self.invalidate();
    }

    fn shown_of(&self, n: usize) -> usize {
        n.min(self.fits.get())
    }

    fn shown(&self) -> usize {
        self.shown_of(self.rows.borrow().len())
    }

    fn chosen(&self) -> Option<usize> {
        self.chosen.get()
    }

    fn step(&self, by: i32) {
        let n = self.rows.borrow().len();
        let Some(c) = step(self.chosen.get(), by, n) else {
            return;
        };
        self.chosen.set(Some(c));
        self.top.set(follow(self.top.get(), c, self.shown()));
        self.invalidate();
    }

    fn scale(&self) -> f32 {
        unsafe { GetDpiForWindow(self.hwnd) }.max(96) as f32 / 96.0
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn paint(&self) {
        let layout = self.layout.borrow();
        let s = self.scale();
        let (w, h) = (
            (layout.size.0 * s).round() as u32,
            (layout.size.1 * s).round() as u32,
        );
        let dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(96);
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            match Target::new(&self.shared.gpu, self.hwnd, w, h, dpi) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for the search: {e}");
                    return;
                }
            }
        }
        let rows = self.rows.borrow();
        let top = self.top.get();
        let shown = layout.rows.len();
        let end = (top + shown).min(rows.len());
        let scene = FinderScene {
            layout: &layout,
            rows: &rows[top.min(end)..end],
            chosen: self.chosen.get().and_then(|c| c.checked_sub(top)),
            top,
            total: rows.len(),
            note: self.note.get(),
        };
        let result = slot
            .as_ref()
            .map(|t| t.draw_finder(&self.shared.gpu, &self.shared.metrics, &scene));
        if let Some(Err(_)) = result {
            *slot = None;
        }
    }

    /// The row under a point, counted among every row.
    fn hit(&self, lparam: LPARAM) -> Option<usize> {
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32 / s;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
        let i = layout::finder_hit(&self.layout.borrow(), x, y)?;
        Some(self.top.get() + i).filter(|&i| i < self.rows.borrow().len())
    }

    fn handle(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                self.paint();
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd), None);
                }
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            // The field keeps the keyboard through a click on a row.
            WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
            WM_SIZE => {
                let w = (lparam.0 & 0xffff) as u32;
                let h = ((lparam.0 >> 16) & 0xffff) as u32;
                if let Some(t) = self.target.borrow().as_ref() {
                    let _ = t.resize(w, h);
                }
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                if let Some(i) = self.hit(lparam) {
                    if self.chosen.replace(Some(i)) != Some(i) {
                        self.invalidate();
                    }
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => Some(LRESULT(0)),
            WM_MOUSEWHEEL => {
                let notches = ((wparam.0 >> 16) & 0xffff) as i16 as i32 / 120;
                let n = self.rows.borrow().len();
                let top = scrolled(self.top.get(), -3 * notches, self.shown(), n);
                if self.top.replace(top) != top {
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                if let Some(i) = self.hit(lparam) {
                    unsafe {
                        let _ =
                            PostMessageW(Some(self.owner), WM_FINDER_PICK, WPARAM(i), LPARAM(0));
                    }
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

/// The window goes with its Box.
impl Drop for List {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const List;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The Box destroys the window as it is dropped, so it outlives it.
    let win = &*ptr;
    match win.handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    use horadric_core::Title;
    use horadric_hooks::transcript::Past;

    fn found(id: &str, title: &str, snippet: &str) -> Found {
        Found {
            past: Past {
                id: id.to_string(),
                title: Title {
                    text: title.to_string(),
                    custom: false,
                },
                asked: "what was asked last".to_string(),
                retitled: false,
                path: PathBuf::new(),
                len: 0,
                modified: SystemTime::UNIX_EPOCH,
                started: SystemTime::UNIX_EPOCH,
            },
            cwd: PathBuf::from("C:\\work\\horadric"),
            project: "horadric".to_string(),
            snippet: snippet.to_string(),
            score: 1.0,
            matched: horadric_hooks::search::Matched::Nothing,
        }
    }

    fn echo(query: &str, _limit: usize) -> Vec<Found> {
        vec![found("1", query, query)]
    }

    #[test]
    fn the_choice_steps_from_none_and_stops_at_the_ends() {
        assert_eq!(step(None, 1, 5), Some(0));
        assert_eq!(step(None, -1, 5), Some(4));
        assert_eq!(step(Some(2), 1, 5), Some(3));
        assert_eq!(step(Some(4), 1, 5), Some(4));
        assert_eq!(step(Some(1), -10, 5), Some(0));
        assert_eq!(step(Some(1), 10, 5), Some(4));
        assert_eq!(step(Some(1), 1, 0), None);
    }

    #[test]
    fn the_view_follows_the_choice_and_scrolls_within_the_list() {
        assert_eq!(follow(0, 3, 10), 0);
        assert_eq!(follow(0, 12, 10), 3);
        assert_eq!(follow(5, 2, 10), 2);
        assert_eq!(scrolled(0, 3, 10, 30), 3);
        assert_eq!(scrolled(18, 3, 10, 30), 20);
        assert_eq!(scrolled(2, -3, 10, 30), 0);
        assert_eq!(scrolled(0, 3, 10, 4), 0);
    }

    #[test]
    fn the_list_hangs_under_the_field_or_over_it_and_stays_on_screen() {
        let work = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        let field = RECT {
            left: 1600,
            top: 300,
            right: 1880,
            bottom: 330,
        };
        // Under it, moved in from the right edge.
        assert_eq!(place(field, (560, 400), work, 4), (1360, 334));
        // Over it when there is no room under it.
        let low = RECT {
            top: 900,
            bottom: 930,
            ..field
        };
        assert_eq!(place(low, (560, 400), work, 4), (1360, 496));
        // Neither fits: the roomier side, kept on the screen.
        assert_eq!(place(field, (560, 900), work, 4), (1360, 140));
        assert_eq!(room(field, work, 4), 706);
        assert_eq!(room(low, work, 4), 896);
    }

    #[test]
    fn a_row_says_its_title_project_age_and_words_on_one_line() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(3 * 3600);
        let f = found("abc", "Fix the\nresume", "the  hook\tfires");
        let l = look(&f, now, &["abc".to_string()]);
        assert_eq!(l.title, "Fix the resume");
        assert_eq!(l.meta, "horadric \u{00B7} 3 h");
        assert_eq!(l.snippet, "the hook fires");
        assert!(l.held);
        let bare = found("x", "", " ");
        let l = look(&bare, now, &[]);
        assert_eq!(l.title, "Untitled conversation");
        assert_eq!(l.snippet, "what was asked last");
        assert!(!l.held);
        assert_eq!(meta("", "now"), "now");
    }

    #[test]
    fn an_answer_is_taken_once_and_only_for_the_newest_question() {
        let seeker = Seeker::new(echo);
        let (tx, rx) = mpsc::channel();
        let wake = move || {
            let _ = tx.send(());
        };
        seeker.ask(1, "hooks", wake.clone());
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(seeker.take(2), None);
        seeker.ask(3, "tiles", wake);
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let got = seeker.take(3).unwrap();
        assert_eq!(got[0].snippet, "tiles");
        assert_eq!(seeker.take(3), None);
    }

    static SLOW_CALLS: AtomicUsize = AtomicUsize::new(0);

    fn slow(query: &str, _limit: usize) -> Vec<Found> {
        SLOW_CALLS.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        vec![found("1", query, query)]
    }

    #[test]
    fn questions_asked_while_one_runs_are_searched_once_for_the_newest() {
        let seeker = Seeker::new(slow);
        let (tx, rx) = mpsc::channel();
        for (seq, q) in [(1, "a"), (2, "ab"), (3, "abc")] {
            let tx = tx.clone();
            seeker.ask(seq, q, move || {
                let _ = tx.send(());
            });
        }
        let mut last = None;
        while last.is_none() {
            rx.recv_timeout(Duration::from_secs(5)).unwrap();
            last = seeker.take(3);
        }
        assert_eq!(last.unwrap()[0].snippet, "abc");
        assert!(SLOW_CALLS.load(Ordering::SeqCst) <= 2);
    }
}
