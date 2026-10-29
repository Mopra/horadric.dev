//! The Context window: every thing a project's Claude sessions load each
//! turn, what each costs and whether it is used, with a switch per line
//! that writes the project's `.claude/settings.json`. Its other mode lists
//! every known project with what its turns cost, so the heavy one is found
//! first.
//!
//! One window, like the quest log: opening it for another project switches
//! it. What it shows is gathered by [`inventory::gather`], which reads
//! megabytes of transcripts, so that runs on a thread and the app hands the
//! window each result as it comes. A switch is a small file write, done
//! here and at once, and only the lines' `on` change after it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;

use horadric_core::context::{self, Source, Switch};
use horadric_core::spend::uptake;
use windows::core::{w, Result, BOOL, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND, DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{InvalidateRect, ScreenToClient, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_BACK, VK_DOWN,
    VK_END, VK_ESCAPE, VK_HOME, VK_NEXT, VK_PRIOR, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW,
    GetWindowRect, IsIconic, IsZoomed, LoadCursorW, LoadIconW, PostMessageW, RegisterClassW,
    SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow, CREATESTRUCTW, GWLP_USERDATA,
    HTCAPTION, HTCLIENT, IDC_ARROW, MINMAXINFO, NCCALCSIZE_PARAMS, SM_CXPADDEDBORDER, SM_CYFRAME,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_RESTORE,
    SW_SHOWNORMAL, WINDOW_EX_STYLE, WM_CAPTURECHANGED, WM_CLOSE, WM_DPICHANGED, WM_ERASEBKGND,
    WM_GETMINMAXINFO, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_NCACTIVATE, WM_NCCALCSIZE, WM_NCCREATE, WM_NCDESTROY, WM_NCHITTEST, WM_PAINT, WM_SIZE,
    WNDCLASSW, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU, WS_THICKFRAME,
};

use crate::app::{self, Input};
use crate::inventory::{self, Inventory, Line, DAYS};
use crate::layout::Rect;
use crate::questlog::{edge, ht, place_for, unix_now, utc_offset, when};
use crate::render::{ContextScene, Target};
use crate::window::Shared;

pub(crate) const CLASS: PCWSTR = w!("HoradricContext");
/// The windows crate files this under `Win32_UI_Controls`.
const WM_MOUSELEAVE: u32 = 0x02A3;

/// The header, the caption the window is dragged by, in DIPs.
pub const HEADER_H: f32 = 58.0;
/// The totals and what they were measured from, under the header.
const SUMMARY_H: f32 = 70.0;
/// The band naming the columns over the rows.
const HEAD_H: f32 = 26.0;
pub const ROW_H: f32 = 30.0;
const MARGIN: f32 = 14.0;
/// Inside the screen, between its edge and the first and last column.
const PAD: f32 = 12.0;
/// Narrower rows than this drop the source column.
const WITH_SOURCE: f32 = 560.0;
pub const MIN_SIZE: (f32, f32) = (460.0, 360.0);
const FIRST_SIZE: (f32, f32) = (820.0, 660.0);
const EDGE: f32 = 6.0;

/// Under the totals: what a switch cannot do and when it takes hold.
pub const NOTE: &str = "claude.ai connectors switch one by one (deniedMcpServers). \
New sessions pick changes up; running ones keep theirs.";

/// One column of the rows: where it starts and how wide it is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Col {
    pub x: f32,
    pub w: f32,
}

impl Col {
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
}

/// Where every part of the window is, in DIPs.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextLayout {
    pub size: (f32, f32),
    pub close: Rect,
    /// "All projects", back to the overview, beside the close key.
    pub back: Rect,
    /// "CONTEXT", and the project's name under it.
    pub label: Rect,
    pub project: Rect,
    /// The totals, what they were measured from, and the note.
    pub total: Rect,
    pub measured: Rect,
    pub note: Rect,
    /// The screen the rows are on, the band naming the columns at its
    /// top, and the rows under it, which scroll.
    pub list: Rect,
    pub head: Rect,
    pub rows: Rect,
    /// A project's columns: tokens, the switch, the name, where it comes
    /// from (none when narrow) and its use.
    pub tokens: Col,
    pub switch: Col,
    pub name: Col,
    pub source: Option<Col>,
    pub used: Col,
    /// The overview's: the project, per turn, after the switches, idle.
    pub o_name: Col,
    pub o_before: Col,
    pub o_after: Col,
    pub o_idle: Col,
}

/// What a point in the window is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextHit {
    Nothing,
    Caption,
    Close,
    Back,
    /// A row, by its place in the list.
    Row(usize),
    /// A row's switch.
    Switch(usize),
}

pub fn layout(size: (f32, f32)) -> ContextLayout {
    let (w, h) = (size.0.max(MIN_SIZE.0), size.1.max(MIN_SIZE.1));
    let close = Rect::new(w - 12.0 - 34.0, 12.0, 34.0, 28.0);
    let back_w = 112.0;
    let back = Rect::new(close.x - 10.0 - back_w, 12.0, back_w, 28.0);
    let label = Rect::new(MARGIN + 6.0, 12.0, back.x - MARGIN - 18.0, 16.0);
    let project = Rect::new(label.x, 28.0, label.w, 22.0);
    let inner_w = w - 2.0 * MARGIN;
    let text_x = MARGIN + 6.0;
    let text_w = inner_w - 12.0;
    let total = Rect::new(text_x, HEADER_H + 2.0, text_w, 24.0);
    let measured = Rect::new(text_x, total.bottom() + 2.0, text_w, 18.0);
    let note = Rect::new(text_x, measured.bottom(), text_w, 18.0);
    let top = HEADER_H + SUMMARY_H;
    let list = Rect::new(MARGIN, top, inner_w, h - MARGIN - top);
    let head = Rect::new(list.x, list.y + 4.0, list.w, HEAD_H);
    let rows = Rect::new(
        list.x,
        head.bottom(),
        list.w,
        list.bottom() - head.bottom() - 4.0,
    );

    let tokens = Col {
        x: rows.x + PAD,
        w: 52.0,
    };
    let switch = Col {
        x: tokens.right() + 10.0,
        w: 48.0,
    };
    let used_w = (rows.w * 0.24).clamp(118.0, 170.0);
    let used = Col {
        x: rows.right() - PAD - used_w,
        w: used_w,
    };
    let source = (rows.w >= WITH_SOURCE).then_some(Col {
        x: used.x - 12.0 - 130.0,
        w: 130.0,
    });
    let name_end = source.map_or(used.x, |s| s.x) - 12.0;
    let name = Col {
        x: switch.right() + 6.0,
        w: (name_end - switch.right() - 6.0).max(0.0),
    };

    let o_idle = Col {
        x: rows.right() - PAD - 80.0,
        w: 80.0,
    };
    let o_after = Col {
        x: o_idle.x - 10.0 - 140.0,
        w: 140.0,
    };
    let o_before = Col {
        x: o_after.x - 10.0 - 120.0,
        w: 120.0,
    };
    let o_name = Col {
        x: rows.x + PAD + 4.0,
        w: (o_before.x - 12.0 - rows.x - PAD - 4.0).max(0.0),
    };
    ContextLayout {
        size: (w, h),
        close,
        back,
        label,
        project,
        total,
        measured,
        note,
        list,
        head,
        rows,
        tokens,
        switch,
        name,
        source,
        used,
        o_name,
        o_before,
        o_after,
        o_idle,
    }
}

impl ContextLayout {
    /// Row `i`'s band, `scroll` DIPs down the list.
    pub fn row(&self, i: usize, scroll: f32) -> Rect {
        Rect::new(
            self.rows.x,
            self.rows.y + i as f32 * ROW_H - scroll,
            self.rows.w,
            ROW_H,
        )
    }

    /// The rows that show of `n`, at least partly.
    pub fn visible(&self, n: usize, scroll: f32) -> std::ops::Range<usize> {
        let first = (scroll / ROW_H).floor().max(0.0) as usize;
        let last = ((scroll + self.rows.h) / ROW_H).ceil().max(0.0) as usize;
        first.min(n)..last.min(n)
    }

    pub fn max_scroll(&self, n: usize) -> f32 {
        (n as f32 * ROW_H - self.rows.h).max(0.0)
    }

    /// The switch of row `i`, where it is drawn and taken.
    pub fn switch_at(&self, i: usize, scroll: f32) -> Rect {
        let band = self.row(i, scroll);
        Rect::new(self.switch.x, band.y + 3.0, self.switch.w, band.h - 6.0)
    }

    /// What is under `(x, y)`, with `n` rows scrolled `scroll` down. The
    /// overview has no back key and no switches.
    pub fn hit(&self, n: usize, scroll: f32, overview: bool, x: f32, y: f32) -> ContextHit {
        if self.close.contains(x, y) {
            return ContextHit::Close;
        }
        if !overview && self.back.contains(x, y) {
            return ContextHit::Back;
        }
        if y < HEADER_H {
            return ContextHit::Caption;
        }
        if self.rows.contains(x, y) {
            let i = ((y - self.rows.y + scroll) / ROW_H).floor();
            if i >= 0.0 && (i as usize) < n {
                let i = i as usize;
                if !overview && self.switch_at(i, scroll).contains(x, y) {
                    return ContextHit::Switch(i);
                }
                return ContextHit::Row(i);
            }
        }
        ContextHit::Nothing
    }
}

/// Tokens as a row shows them: `850`, `6.6k`, `52.1k`.
pub fn kilo(tokens: u64) -> String {
    if tokens < 1000 {
        tokens.to_string()
    } else {
        format!("{:.1}k", tokens as f64 / 1000.0)
    }
}

/// How long ago, short: `5m ago`, `3h ago`, `2d ago`.
pub fn ago(secs: u64) -> String {
    match secs {
        s if s < 3600 => format!("{}m ago", (s / 60).max(1)),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

/// A line of a project's list as it is drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRow {
    /// Once per turn, or per prompt for a hook.
    pub tokens: String,
    pub name: String,
    pub source: &'static str,
    /// `41 calls, 2d ago`, `unused 30d`, `per prompt, 12 times`.
    pub used: String,
    /// Likely waste: on and never used.
    pub warn: bool,
    /// On or off, none when nothing in the project's settings switches it.
    pub switch: Option<bool>,
}

/// What row `line` says, `now` in Unix seconds.
pub fn row_of(line: &Line, now: u64) -> ContextRow {
    let item = &line.item;
    let hook = item.source == Source::Hook;
    let tokens = if hook {
        item.tokens() / item.times.max(1)
    } else {
        item.tokens()
    };
    let used = if hook {
        match item.times {
            1 => "per prompt, once".to_string(),
            n => format!("per prompt, {n} times"),
        }
    } else if let Some(u) = &line.used {
        let calls = match u.calls {
            1 => "1 call".to_string(),
            n => format!("{n} calls"),
        };
        match uptake::millis(&u.last) {
            Some(ms) => format!("{calls}, {}", ago(now.saturating_sub(ms / 1000))),
            None => calls,
        }
    } else if line.switch == Switch::None {
        String::new()
    } else {
        format!("unused {DAYS}d")
    };
    ContextRow {
        tokens: kilo(tokens),
        name: item.name.clone(),
        source: item.source.label(),
        used,
        warn: line.idle(),
        switch: (line.switch != Switch::None).then_some(line.on),
    }
}

/// The header's first line: what a turn costs, and after the switches.
pub fn totals_line(before: u64, after: u64) -> String {
    if after == before {
        format!("{} per turn, nothing switched off", kilo(before))
    } else {
        format!(
            "{} per turn, {} after your switches",
            kilo(before),
            kilo(after)
        )
    }
}

/// The header's second line: which transcript the costs were read from,
/// written at `at`, and how many sessions the use was counted over.
pub fn measured_line(at: Option<u64>, sessions: usize, now: u64, offset: i64) -> String {
    let Some(at) = at else {
        return "No session of this project carried a full listing yet: start one, \
then look again."
            .to_string();
    };
    let over = match sessions {
        1 => "1 session".to_string(),
        n => format!("{n} sessions"),
    };
    format!(
        "Measured from the session of {}. Use counted over {over} of the last {DAYS} days.",
        when(at, now, offset)
    )
}

/// A project of the overview as it is drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewRow {
    pub name: String,
    pub before: String,
    pub after: String,
    pub idle: String,
    pub warn: bool,
}

/// What the overview's row for a project says, `None` while it is read.
pub fn overview_row(name: &str, inv: Option<&Inventory>) -> OverviewRow {
    let Some(inv) = inv else {
        return OverviewRow {
            name: name.to_string(),
            before: "reading\u{2026}".into(),
            after: String::new(),
            idle: String::new(),
            warn: false,
        };
    };
    if inv.measured.is_none() {
        return OverviewRow {
            name: name.to_string(),
            before: "no session yet".into(),
            after: String::new(),
            idle: String::new(),
            warn: false,
        };
    }
    let (before, after) = inv.per_turn();
    let idle = inv.lines.iter().filter(|l| l.idle()).count();
    OverviewRow {
        name: name.to_string(),
        before: format!("{} per turn", kilo(before)),
        after: format!("{} after", kilo(after)),
        idle: match idle {
            0 => String::new(),
            n => format!("{n} idle"),
        },
        warn: idle > 0,
    }
}

/// The overview's first line: every project together.
pub fn overview_total(known: &[Option<&Inventory>]) -> String {
    let read: Vec<&Inventory> = known.iter().flatten().copied().collect();
    let n = known.len();
    if read.len() < n {
        return format!("Reading {} of {n} projects\u{2026}", read.len() + 1);
    }
    let idle: usize = read
        .iter()
        .map(|i| i.lines.iter().filter(|l| l.idle()).count())
        .sum();
    let projects = match n {
        1 => "1 project".to_string(),
        n => format!("{n} projects"),
    };
    match idle {
        0 => format!("{projects}, nothing loaded that went unused"),
        1 => format!("{projects}, 1 thing loaded that went unused for {DAYS} days"),
        i => format!("{projects}, {i} things loaded that went unused for {DAYS} days"),
    }
}

/// A project the window shows or lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub key: String,
    pub name: String,
    /// Its main folder, whose settings the switches write.
    pub main: PathBuf,
}

/// Something the window asks of the app, or the app is asked for it.
pub enum Ask {
    /// Show the project with this key.
    Open(String),
    /// Show every known project.
    Overview,
    Close,
}

static GATHERED: Mutex<Vec<(String, Inventory)>> = Mutex::new(Vec::new());

/// Reads each project's inventory on a thread, posting `msg` to `notify`,
/// the app's message window, after each, so the overview fills in as they
/// come rather than after the slowest.
pub fn gather(projects: Vec<(String, PathBuf)>, notify: isize, msg: u32) {
    std::thread::spawn(move || {
        for (key, main) in projects {
            let inv = inventory::gather(&main);
            if let Ok(mut g) = GATHERED.lock() {
                g.push((key, inv));
            }
            unsafe {
                let _ = PostMessageW(Some(HWND(notify as *mut c_void)), msg, WPARAM(0), LPARAM(0));
            }
        }
    });
}

/// What [`gather`] has read since last asked.
pub fn take() -> Vec<(String, Inventory)> {
    GATHERED
        .lock()
        .map(|mut g| std::mem::take(&mut *g))
        .unwrap_or_default()
}

pub fn register_class() -> Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hIcon: LoadIconW(
                Some(instance.into()),
                PCWSTR(std::ptr::without_provenance(1)),
            )
            .unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        Ok(())
    }
}

/// One project as read, and when the transcript it was measured from was
/// written.
struct Known {
    inv: Inventory,
    at: Option<u64>,
}

enum View {
    Project(Project),
    Overview(Vec<Project>),
}

pub struct ContextPanel {
    pub hwnd: HWND,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    view: RefCell<View>,
    /// Every project read since the window opened, by key.
    known: RefCell<HashMap<String, Known>>,
    /// What the last switch failed on, shown in the header until the next.
    error: RefCell<Option<String>>,
    scroll: Cell<f32>,
    hot: Cell<ContextHit>,
    pressed: Cell<Option<ContextHit>>,
    tracking: Cell<bool>,
    active: Cell<bool>,
}

thread_local! {
    /// Where the window was when it last closed, so it opens there again.
    static LAST: Cell<Option<RECT>> = const { Cell::new(None) };
}

fn mtime(p: &Path) -> Option<u64> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(m.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs())
}

impl ContextPanel {
    pub fn open(shared: Rc<Shared>) -> Result<Box<Self>> {
        let mut win = Box::new(ContextPanel {
            hwnd: HWND::default(),
            shared,
            target: RefCell::new(None),
            view: RefCell::new(View::Overview(Vec::new())),
            known: RefCell::new(HashMap::new()),
            error: RefCell::new(None),
            scroll: Cell::new(0.0),
            hot: Cell::new(ContextHit::Nothing),
            pressed: Cell::new(None),
            tracking: Cell::new(false),
            active: Cell::new(true),
        });
        let place = LAST
            .with(Cell::get)
            .unwrap_or_else(|| place_for(FIRST_SIZE));
        unsafe {
            // Born where it shows and at its size: a window that starts
            // elsewhere and moves has been seen not to paint.
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS,
                w!("Horadric context"),
                WS_POPUP | WS_THICKFRAME | WS_SYSMENU | WS_MINIMIZEBOX,
                place.left,
                place.top,
                place.right - place.left,
                place.bottom - place.top,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*win as *const ContextPanel as *const c_void),
            )?;
            win.hwnd = hwnd;
            let dark = BOOL(1);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark as *const _ as *const c_void,
                std::mem::size_of::<BOOL>() as u32,
            );
            let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(hwnd);
        }
        Ok(win)
    }

    /// Shows `p`, as last read until the app hands a fresh reading.
    pub fn show_project(&self, p: Project) {
        *self.view.borrow_mut() = View::Project(p);
        *self.error.borrow_mut() = None;
        self.scroll.set(0.0);
        self.invalidate();
    }

    /// Lists `projects`, each as last read until its fresh reading comes.
    pub fn show_overview(&self, projects: Vec<Project>) {
        *self.view.borrow_mut() = View::Overview(projects);
        *self.error.borrow_mut() = None;
        self.scroll.set(0.0);
        self.invalidate();
    }

    /// A fresh reading of the project `key`.
    pub fn gathered(&self, key: String, inv: Inventory) {
        let at = inv.measured.as_deref().and_then(mtime);
        self.known.borrow_mut().insert(key, Known { inv, at });
        self.clamp_scroll();
        self.invalidate();
    }

    pub fn raise(&self) {
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    pub fn destroy(&self) {
        unsafe {
            if !IsIconic(self.hwnd).as_bool() && !IsZoomed(self.hwnd).as_bool() {
                let mut r = RECT::default();
                if GetWindowRect(self.hwnd, &mut r).is_ok() {
                    LAST.with(|l| l.set(Some(r)));
                }
            }
            let _ = DestroyWindow(self.hwnd);
        }
    }

    pub fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn dpi(&self) -> u32 {
        unsafe { GetDpiForWindow(self.hwnd) }.max(96)
    }

    fn scale(&self) -> f32 {
        self.dpi() as f32 / 96.0
    }

    fn client_px(&self) -> (i32, i32) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        (r.right - r.left, r.bottom - r.top)
    }

    fn layout(&self) -> ContextLayout {
        let (w, h) = self.client_px();
        let s = self.scale();
        layout((w as f32 / s, h as f32 / s))
    }

    fn overview(&self) -> bool {
        matches!(*self.view.borrow(), View::Overview(_))
    }

    /// How many rows the list has.
    fn count(&self) -> usize {
        match &*self.view.borrow() {
            View::Overview(ps) => ps.len(),
            View::Project(p) => self
                .known
                .borrow()
                .get(&p.key)
                .map_or(0, |k| k.inv.lines.len()),
        }
    }

    fn clamp_scroll(&self) {
        let most = self.layout().max_scroll(self.count());
        self.scroll.set(self.scroll.get().clamp(0.0, most));
    }

    fn paint(&self) {
        let (w, h) = self.client_px();
        if w <= 0 || h <= 0 {
            return;
        }
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            match Target::new(&self.shared.gpu, self.hwnd, w as u32, h as u32, self.dpi()) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for the context window: {e}");
                    return;
                }
            }
        }
        let l = self.layout();
        let now = unix_now();
        let known = self.known.borrow();
        let view = self.view.borrow();
        let error = self.error.borrow();
        let (title, total, measured, rows, overview_rows, empty) = match &*view {
            View::Project(p) => match known.get(&p.key) {
                Some(k) => {
                    let (before, after) = k.inv.per_turn();
                    let rows: Vec<ContextRow> =
                        k.inv.lines.iter().map(|l| row_of(l, now)).collect();
                    (
                        p.name.clone(),
                        totals_line(before, after),
                        measured_line(k.at, k.inv.sessions, now, utc_offset(now)),
                        rows,
                        Vec::new(),
                        "Nothing measured yet.",
                    )
                }
                None => (
                    p.name.clone(),
                    "Reading the project's transcripts\u{2026}".to_string(),
                    String::new(),
                    Vec::new(),
                    Vec::new(),
                    "",
                ),
            },
            View::Overview(ps) => {
                let read: Vec<Option<&Inventory>> = ps
                    .iter()
                    .map(|p| known.get(&p.key).map(|k| &k.inv))
                    .collect();
                let rows: Vec<OverviewRow> = ps
                    .iter()
                    .zip(&read)
                    .map(|(p, i)| overview_row(&p.name, *i))
                    .collect();
                (
                    "All projects".to_string(),
                    overview_total(&read),
                    "Each project's turn as its newest session measured it. \
Click one to see what it loads and switch it."
                        .to_string(),
                    Vec::new(),
                    rows,
                    "No recent projects.",
                )
            }
        };
        let scene = ContextScene {
            layout: &l,
            title: &title,
            overview: matches!(*view, View::Overview(_)),
            total: &total,
            measured: error.as_deref().unwrap_or(&measured),
            failed: error.is_some(),
            note: NOTE,
            rows: &rows,
            overview_rows: &overview_rows,
            empty,
            hot: self.hot.get(),
            pressed: self.pressed.get(),
            scroll: self.scroll.get(),
            active: self.active.get(),
        };
        let drawn = slot
            .as_ref()
            .map(|t| t.draw_context(&self.shared.gpu, &self.shared.metrics, &scene));
        if let Some(Err(_)) = drawn {
            *slot = None;
        }
    }

    fn point(&self, lparam: LPARAM) -> (f32, f32) {
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32 / s;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
        (x, y)
    }

    fn hit_at(&self, x: f32, y: f32) -> ContextHit {
        let hit = self
            .layout()
            .hit(self.count(), self.scroll.get(), self.overview(), x, y);
        // A line nothing switches has no switch to light.
        match hit {
            ContextHit::Switch(i) if !self.switchable(i) => ContextHit::Row(i),
            h => h,
        }
    }

    fn switchable(&self, i: usize) -> bool {
        let View::Project(p) = &*self.view.borrow() else {
            return false;
        };
        self.known
            .borrow()
            .get(&p.key)
            .and_then(|k| k.inv.lines.get(i))
            .is_some_and(|l| l.switch != Switch::None)
    }

    fn hover(&self, hot: ContextHit) {
        if self.hot.replace(hot) != hot {
            self.invalidate();
        }
    }

    fn track(&self) {
        if self.tracking.replace(true) {
            return;
        }
        let mut t = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: self.hwnd,
            dwHoverTime: 0,
        };
        unsafe {
            let _ = TrackMouseEvent(&mut t);
        }
    }

    fn click(&self, hit: ContextHit) {
        match hit {
            ContextHit::Close => app::push(Input::Context(Ask::Close)),
            ContextHit::Back => app::push(Input::Context(Ask::Overview)),
            ContextHit::Switch(i) => self.flip(i),
            ContextHit::Row(i) => {
                let key = match &*self.view.borrow() {
                    View::Overview(ps) => ps.get(i).map(|p| p.key.clone()),
                    View::Project(_) => None,
                };
                if let Some(key) = key {
                    app::push(Input::Context(Ask::Open(key)));
                }
            }
            ContextHit::Caption | ContextHit::Nothing => {}
        }
    }

    /// Turns line `i` of the project shown off or on, then reads the
    /// settings back for every line, since one plugin's switch can take
    /// several lines with it.
    fn flip(&self, i: usize) {
        let View::Project(p) = &*self.view.borrow() else {
            return;
        };
        let mut known = self.known.borrow_mut();
        let Some(k) = known.get_mut(&p.key) else {
            return;
        };
        let Some(line) = k.inv.lines.get(i) else {
            return;
        };
        let result = inventory::toggle(&p.main, &line.item.key, !line.on);
        *self.error.borrow_mut() = match result {
            Ok(_) => {
                let settings = std::fs::read_to_string(inventory::settings_path(&p.main))
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::Value::Null);
                for l in &mut k.inv.lines {
                    l.on = context::is_on(&settings, &l.switch);
                }
                None
            }
            Err(e) => Some(e),
        };
        self.invalidate();
    }

    fn wheel(&self, wparam: WPARAM) {
        let notches = ((wparam.0 >> 16) & 0xffff) as i16 as f32 / 120.0;
        self.scroll.set(self.scroll.get() - notches * ROW_H * 3.0);
        self.clamp_scroll();
        self.invalidate();
    }

    fn key_down(&self, vk: u16) {
        if vk == VK_ESCAPE.0 {
            app::push(Input::Context(Ask::Close));
            return;
        }
        if vk == VK_BACK.0 && !self.overview() {
            app::push(Input::Context(Ask::Overview));
            return;
        }
        let page = (self.layout().rows.h - ROW_H).max(ROW_H);
        let at = self.scroll.get();
        let to = match vk {
            v if v == VK_UP.0 => at - ROW_H,
            v if v == VK_DOWN.0 => at + ROW_H,
            v if v == VK_PRIOR.0 => at - page,
            v if v == VK_NEXT.0 => at + page,
            v if v == VK_HOME.0 => 0.0,
            v if v == VK_END.0 => f32::MAX,
            _ => return,
        };
        self.scroll.set(to);
        self.clamp_scroll();
        self.invalidate();
    }

    fn frame(&self) -> i32 {
        let dpi = self.dpi();
        unsafe {
            GetSystemMetricsForDpi(SM_CYFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
        }
    }

    fn handle(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_NCCALCSIZE if wparam.0 != 0 => {
                if unsafe { IsZoomed(self.hwnd) }.as_bool() {
                    let f = self.frame();
                    let params = unsafe { &mut *(lparam.0 as *mut NCCALCSIZE_PARAMS) };
                    let r = &mut params.rgrc[0];
                    r.left += f;
                    r.top += f;
                    r.right -= f;
                    r.bottom -= f;
                }
                Some(LRESULT(0))
            }
            WM_NCHITTEST => {
                let mut p = POINT {
                    x: (lparam.0 & 0xffff) as i16 as i32,
                    y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
                };
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut p);
                }
                let s = self.scale();
                let (x, y) = (p.x as f32 / s, p.y as f32 / s);
                let l = self.layout();
                if !unsafe { IsZoomed(self.hwnd) }.as_bool() {
                    if let Some(e) = edge(x, y, l.size.0, l.size.1, EDGE) {
                        return Some(LRESULT(ht(e) as isize));
                    }
                }
                let hit = match self.hit_at(x, y) {
                    ContextHit::Caption => HTCAPTION,
                    _ => HTCLIENT,
                };
                Some(LRESULT(hit as isize))
            }
            WM_GETMINMAXINFO => {
                let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
                let s = unsafe { GetDpiForWindow(self.hwnd) }.max(96) as f32 / 96.0;
                info.ptMinTrackSize = POINT {
                    x: (MIN_SIZE.0 * s).round() as i32,
                    y: (MIN_SIZE.1 * s).round() as i32,
                };
                Some(LRESULT(0))
            }
            WM_NCACTIVATE => {
                if self.active.replace(wparam.0 != 0) != (wparam.0 != 0) {
                    self.invalidate();
                }
                Some(unsafe { DefWindowProcW(self.hwnd, msg, wparam, LPARAM(-1)) })
            }
            WM_PAINT => {
                self.paint();
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd), None);
                }
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_SIZE => {
                let w = (lparam.0 & 0xffff) as u32;
                let h = ((lparam.0 >> 16) & 0xffff) as u32;
                if let Some(t) = self.target.borrow().as_ref() {
                    if t.resize(w, h).is_err() {
                        self.target.replace(None);
                    }
                }
                self.clamp_scroll();
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                self.target.replace(None);
                let suggested = unsafe { *(lparam.0 as *const RECT) };
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        suggested.left,
                        suggested.top,
                        suggested.right - suggested.left,
                        suggested.bottom - suggested.top,
                        SWP_NOACTIVATE | SWP_NOZORDER,
                    );
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                self.track();
                let (x, y) = self.point(lparam);
                self.hover(self.hit_at(x, y));
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                let (x, y) = self.point(lparam);
                let hit = self.hit_at(x, y);
                unsafe {
                    SetCapture(self.hwnd);
                }
                if self.pressed.replace(Some(hit)) != Some(hit) {
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                // Taken first: releasing capture sends WM_CAPTURECHANGED at
                // once, and that clears the press.
                let pressed = self.pressed.take();
                unsafe {
                    let _ = ReleaseCapture();
                }
                let (x, y) = self.point(lparam);
                let hit = self.hit_at(x, y);
                self.invalidate();
                if pressed == Some(hit) {
                    self.click(hit);
                }
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                if self.pressed.take().is_some() {
                    self.invalidate();
                }
                None
            }
            WM_MOUSELEAVE => {
                self.tracking.set(false);
                self.hover(ContextHit::Nothing);
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                self.wheel(wparam);
                Some(LRESULT(0))
            }
            WM_KEYDOWN => {
                self.key_down(wparam.0 as u16);
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                app::push(Input::Context(Ask::Close));
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const ContextPanel;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The Box lives in the app for as long as the window exists, and the app
    // destroys the window before dropping the Box.
    let win = &*ptr;
    match win.handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::context::{Item, Use};

    fn line(source: Source, chars: u64, switch: Switch, on: bool, used: Option<Use>) -> Line {
        Line {
            item: Item {
                key: "k".into(),
                name: "Slack".into(),
                source,
                chars,
                times: 1,
            },
            switch,
            on,
            used,
        }
    }

    #[test]
    fn tokens_read_short() {
        assert_eq!(kilo(850), "850");
        assert_eq!(kilo(6600), "6.6k");
        assert_eq!(kilo(52_140), "52.1k");
        assert_eq!(kilo(31_000), "31.0k");
    }

    #[test]
    fn ages_read_in_the_largest_whole_unit() {
        assert_eq!(ago(10), "1m ago");
        assert_eq!(ago(600), "10m ago");
        assert_eq!(ago(7200), "2h ago");
        assert_eq!(ago(2 * 86_400 + 5), "2d ago");
    }

    #[test]
    fn a_used_line_says_how_often_and_when_last() {
        // 2026-10-29T12:00:00Z
        let now = 1_793_275_200;
        let used = Use {
            calls: 41,
            last: "2026-10-27T12:00:00.000Z".into(),
        };
        let l = line(
            Source::Account,
            26_400,
            Switch::Server("claude.ai Slack".into()),
            true,
            Some(used),
        );
        let r = row_of(&l, now);
        assert_eq!(r.tokens, "6.6k");
        assert_eq!(r.used, "41 calls, 2d ago");
        assert_eq!(r.source, "claude.ai connector");
        assert_eq!(r.switch, Some(true));
        assert!(!r.warn);
    }

    #[test]
    fn an_unused_line_left_on_is_a_warning_and_one_switched_off_is_not() {
        let on = line(
            Source::Plugin,
            4000,
            Switch::Plugin("x@y".into()),
            true,
            None,
        );
        let r = row_of(&on, 0);
        assert_eq!(r.used, format!("unused {DAYS}d"));
        assert!(r.warn);
        let off = line(
            Source::Plugin,
            4000,
            Switch::Plugin("x@y".into()),
            false,
            None,
        );
        let r = row_of(&off, 0);
        assert!(!r.warn);
        assert_eq!(r.switch, Some(false));
    }

    #[test]
    fn a_fixed_line_has_no_switch_and_says_nothing_of_use() {
        let l = line(Source::ClaudeCode, 40_000, Switch::None, true, None);
        let r = row_of(&l, 0);
        assert_eq!(r.switch, None);
        assert_eq!(r.used, "");
        assert!(!r.warn);
    }

    #[test]
    fn a_hook_counts_per_prompt() {
        let mut l = line(Source::Hook, 48_000, Switch::None, true, None);
        l.item.times = 12;
        let r = row_of(&l, 0);
        assert_eq!(r.tokens, "1.0k");
        assert_eq!(r.used, "per prompt, 12 times");
        l.item.times = 1;
        assert_eq!(row_of(&l, 0).used, "per prompt, once");
    }

    #[test]
    fn the_totals_say_what_the_switches_save() {
        assert_eq!(
            totals_line(52_100, 31_000),
            "52.1k per turn, 31.0k after your switches"
        );
        assert_eq!(totals_line(800, 800), "800 per turn, nothing switched off");
    }

    #[test]
    fn the_measure_names_its_session_and_how_many_were_counted() {
        let now = 1_793_275_200;
        let s = measured_line(Some(now - 3600), 41, now, 0);
        assert_eq!(
            s,
            "Measured from the session of today 11:00. Use counted over 41 sessions of the last 30 days."
        );
        assert!(measured_line(None, 0, now, 0).starts_with("No session"));
    }

    #[test]
    fn an_overview_row_says_reading_until_read_and_counts_idle_lines() {
        assert_eq!(overview_row("a", None).before, "reading\u{2026}");
        let inv = Inventory {
            lines: vec![
                line(Source::Plugin, 8000, Switch::Plugin("p".into()), true, None),
                line(Source::Skill, 4000, Switch::Skill("s".into()), false, None),
            ],
            measured: Some(PathBuf::from("x.jsonl")),
            sessions: 3,
        };
        let r = overview_row("a", Some(&inv));
        assert_eq!(r.before, "3.0k per turn");
        assert_eq!(r.after, "2.0k after");
        assert_eq!(r.idle, "1 idle");
        assert!(r.warn);
        assert_eq!(
            overview_total(&[Some(&inv), None]),
            "Reading 2 of 2 projects\u{2026}"
        );
        assert_eq!(
            overview_total(&[Some(&inv)]),
            "1 project, 1 thing loaded that went unused for 30 days"
        );
    }

    #[test]
    fn the_columns_fit_and_a_narrow_window_drops_the_source() {
        let wide = layout((820.0, 660.0));
        let s = wide.source.expect("room for the source");
        assert!(wide.name.right() <= s.x);
        assert!(s.right() <= wide.used.x);
        assert!(wide.used.right() <= wide.rows.right());
        assert!(wide.o_name.right() <= wide.o_before.x);
        assert!(wide.back.right() <= wide.close.x);
        let narrow = layout(MIN_SIZE);
        assert!(narrow.source.is_none());
        assert!(narrow.name.w > 60.0);
    }

    #[test]
    fn a_point_finds_the_row_and_its_switch_scrolled_or_not() {
        let l = layout((820.0, 660.0));
        let y = l.rows.y + ROW_H * 2.0 + ROW_H / 2.0;
        let sx = l.switch.x + l.switch.w / 2.0;
        assert_eq!(l.hit(10, 0.0, false, sx, y), ContextHit::Switch(2));
        assert_eq!(
            l.hit(10, 0.0, false, l.name.x + 20.0, y),
            ContextHit::Row(2)
        );
        assert_eq!(l.hit(10, ROW_H, false, sx, y), ContextHit::Switch(3));
        // The overview has no switches and no back key.
        assert_eq!(l.hit(10, 0.0, true, sx, y), ContextHit::Row(2));
        let (bx, by) = (l.back.x + 4.0, l.back.y + 4.0);
        assert_eq!(l.hit(10, 0.0, false, bx, by), ContextHit::Back);
        assert_eq!(l.hit(10, 0.0, true, bx, by), ContextHit::Caption);
        assert_eq!(l.hit(1, 0.0, false, sx, y), ContextHit::Nothing);
        assert_eq!(l.max_scroll(1), 0.0);
        assert!(l.max_scroll(100) > 0.0);
    }
}
