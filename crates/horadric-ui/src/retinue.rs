//! A pane's retinue: a row per hireling of its session along the bottom of
//! the glass, under the grid. See docs/plan/hirelings.md.
//!
//! A child window of the pane with its own render target, like the
//! Folio's bar, so the grid's drawing is left alone: the pane only gives
//! up the rows it needs. It reads the session's band from the registry
//! on a timer of its own, fast while a rune carves and slow while all
//! rest, and tells the pane when it needs more or fewer rows. A click on
//! a row opens that hireling's conversation in a pane beside it.
//!
//! The band only learns a hireling's tokens and tool count from its
//! transcript, so the strip reads what each one's has grown by.
//!
//! Turned over (Ctrl+Shift+W, or the link in its heading) it is the war
//! table: the same window over the whole glass, the turn drawn on one
//! axis of time, a track for the main loop and one per hireling. The
//! grid under it keeps its size, so the program never redraws for it.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::io::{Read, Seek, SeekFrom};
use std::rc::Rc;
use std::time::SystemTime;

use horadric_core::hireling::{self, Band, Craft, Lot, Mark};
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{InvalidateRect, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetParent, GetWindowLongPtrW,
    LoadCursorW, PostMessageW, RegisterClassW, SetCursor, SetTimer, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, CREATESTRUCTW, GWLP_USERDATA, HWND_TOP, IDC_HAND, SWP_NOACTIVATE,
    SW_HIDE, SW_SHOWNA, WINDOW_EX_STYLE, WM_ERASEBKGND, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SETCURSOR, WM_SIZE, WM_TIMER, WM_USER,
    WNDCLASSW, WS_CHILD, WS_CLIPSIBLINGS,
};

use crate::app::{self, Input};
use crate::render::{
    RetinueRow, RetinueScene, TableScene, TableTrack, Target, RETINUE_LINK_W, RETINUE_ROW_H,
    RETINUE_TOP, TABLE_AXIS, TABLE_HEAD, TABLE_ROW_H,
};
use crate::theme::{self, Color};
use crate::window::Shared;
use crate::{tip, tip::Line};

const CLASS: PCWSTR = w!("HoradricRetinue");
const TIMER: usize = 1;
const WM_MOUSELEAVE: u32 = 0x02A3;
/// The beat while a rune carves, and while all rest.
const FAST_MS: u32 = 50;
const SLOW_MS: u32 = 700;
/// More rows than this and the strip shows the first, most in need.
pub const MAX_ROWS: usize = 4;
/// Posted to the pane when the strip wants another number of rows.
pub const WM_RETINUE_ROWS: u32 = WM_USER + 21;

/// How tall a strip of `rows` rows is, in DIPs. None takes nothing.
pub fn height(rows: usize) -> f32 {
    if rows == 0 {
        0.0
    } else {
        RETINUE_TOP + rows as f32 * RETINUE_ROW_H + 3.0
    }
}

pub struct Retinue {
    hwnd: Cell<HWND>,
    shared: Rc<Shared>,
    /// The session whose hirelings it shows.
    session: String,
    target: RefCell<Option<Target>>,
    /// What each row stands for, by hireling id, as last drawn.
    ids: RefCell<Vec<String>>,
    rows: Cell<usize>,
    /// The row under the cursor; `usize::MAX` for the heading's link.
    hot: Cell<Option<usize>>,
    tracking: Cell<bool>,
    /// Turned over into the war table.
    table: Cell<bool>,
    beat: Cell<u32>,
    accent: Cell<Color>,
}

impl Retinue {
    pub fn create(shared: Rc<Shared>, session: &str, parent: HWND) -> Result<Box<Retinue>> {
        register_class();
        let r = Box::new(Retinue {
            hwnd: Cell::new(HWND::default()),
            shared,
            session: session.to_string(),
            target: RefCell::new(None),
            ids: RefCell::new(Vec::new()),
            rows: Cell::new(0),
            hot: Cell::new(None),
            tracking: Cell::new(false),
            table: Cell::new(false),
            beat: Cell::new(0),
            accent: Cell::new(theme::ACCENTS[0].0),
        });
        unsafe {
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS,
                PCWSTR::null(),
                WS_CHILD | WS_CLIPSIBLINGS,
                0,
                0,
                0,
                0,
                Some(parent),
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*r as *const Retinue as *const c_void),
            )?;
            r.hwnd.set(hwnd);
        }
        r.poll();
        Ok(r)
    }

    /// How many rows it shows now.
    pub fn rows(&self) -> usize {
        self.rows.get()
    }

    /// Whether it is the war table.
    pub fn table(&self) -> bool {
        self.table.get()
    }

    /// Turns it into the war table or back. The pane places it after.
    pub fn set_table(&self, on: bool) {
        if self.table.replace(on) != on {
            self.hot.set(None);
            self.poll();
            self.invalidate();
        }
    }

    /// Asks the pane to lay it out again.
    fn tell_pane(&self) {
        unsafe {
            if let Ok(pane) = GetParent(self.hwnd.get()) {
                let _ = PostMessageW(Some(pane), WM_RETINUE_ROWS, WPARAM(0), LPARAM(0));
            }
        }
    }

    pub fn set_accent(&self, c: Color) {
        if self.accent.replace(c) != c {
            self.invalidate();
        }
    }

    /// Lays it along the bottom of the glass, `rect` in pixels of the pane.
    pub fn place(&self, rect: [i32; 4]) {
        let [l, t, r, b] = rect;
        let show = (self.rows.get() > 0 || self.table.get()) && r > l && b > t;
        unsafe {
            let _ = SetWindowPos(
                self.hwnd.get(),
                Some(HWND_TOP),
                l,
                t,
                r - l,
                b - t,
                SWP_NOACTIVATE,
            );
            let _ = ShowWindow(self.hwnd.get(), if show { SW_SHOWNA } else { SW_HIDE });
        }
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, false);
        }
    }

    /// The session's band, with what its hirelings' transcripts added
    /// since the last look read into it.
    fn band(&self) -> Option<Band> {
        let now = SystemTime::now();
        let reading: Vec<(String, String, u64)> = {
            let reg = self.shared.registry.lock().ok()?;
            let band = &reg.get(&self.session)?.band;
            band.shown(now, self.table.get())
                .iter()
                .filter_map(|h| Some((h.id.clone(), band.transcript_of(h)?, h.read)))
                .collect()
        };
        // Read with the registry let go: the listener waits on it.
        let more: Vec<(String, u64, String)> = reading
            .into_iter()
            .filter_map(|(id, path, from)| Some((id, from, read_from(&path, from)?)))
            .collect();
        let mut reg = self.shared.registry.lock().ok()?;
        let band = &mut reg.get_mut(&self.session)?.band;
        for (id, from, text) in more {
            // Another pane of the same session may have read it first.
            if band.hirelings.iter().any(|h| h.id == id && h.read == from) {
                band.tally(&id, &text);
            }
        }
        Some(band.clone())
    }

    /// Reads the band again, redraws, and tells the pane when the number
    /// of rows changed. Sets the next beat.
    fn poll(&self) {
        let now = SystemTime::now();
        let band = self.band().unwrap_or_default();
        let shown = band.shown(now, false);
        let rows = shown.len().min(MAX_ROWS);
        *self.ids.borrow_mut() = shown.iter().map(|h| h.id.clone()).collect();
        if self.rows.replace(rows) != rows {
            self.tell_pane();
        }
        if self.table.get() {
            *self.ids.borrow_mut() = table_order(&band, now).iter().map(|h| h.id.clone()).collect();
        }
        let carving = shown.iter().any(|h| h.lot_at(now) == Lot::Working)
            || (self.table.get() && band.turn_end.is_none() && band.turn.is_some());
        let beat = if carving { FAST_MS } else { SLOW_MS };
        if self.beat.replace(beat) != beat {
            unsafe {
                SetTimer(Some(self.hwnd.get()), TIMER, beat, None);
            }
        }
        if rows > 0 || self.table.get() {
            self.invalidate();
        }
    }

    fn paint(&self) {
        let now = SystemTime::now();
        let (band, label) = self
            .shared
            .registry
            .lock()
            .ok()
            .and_then(|r| {
                let s = r.get(&self.session)?;
                Some((s.band.clone(), s.label().to_string()))
            })
            .unwrap_or_default();
        let rows: Vec<RetinueRow> = band
            .shown(now, false)
            .iter()
            .take(MAX_ROWS)
            .map(|h| RetinueRow {
                rune: hireling::rune_of(&h.kind),
                kind: h.kind.clone(),
                task: h.task.clone(),
                doing: h.doing(now),
                ran: horadric_core::heavy::duration(h.ran(now).as_secs()),
                tokens: (h.tokens > 0)
                    .then(|| hireling::tokens(h.tokens))
                    .unwrap_or_default(),
                lot: h.lot_at(now),
                age: now.duration_since(h.started).unwrap_or_default(),
            })
            .collect();
        let hwnd = self.hwnd.get();
        let mut rc = Default::default();
        unsafe {
            let _ = GetClientRect(hwnd, &mut rc);
        }
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let scale = dpi as f32 / 96.0;
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            match Target::new(
                &self.shared.gpu,
                hwnd,
                rc.right as u32,
                rc.bottom as u32,
                dpi,
            ) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for a retinue: {e}");
                    return;
                }
            }
        }
        let Some(t) = slot.as_ref() else {
            return;
        };
        t.set_dpi(dpi);
        let size = (rc.right as f32 / scale, rc.bottom as f32 / scale);
        let drawn = if self.table.get() {
            let (tracks, span) = tracks(&band, now, self.accent.get());
            let title = format!(
                "War table \u{b7} {label} \u{b7} {} hireling{} \u{b7} {}",
                tracks.len() - 1,
                if tracks.len() == 2 { "" } else { "s" },
                horadric_core::heavy::duration(span as u64)
            );
            let scene = TableScene {
                tracks: &tracks,
                span,
                title: &title,
                // Track 0 is the main loop's; the hot ids are hirelings'.
                hot: self.hot.get().map(|i| if i == usize::MAX { i } else { i + 1 }),
                size,
                accent: self.accent.get(),
                age: now.duration_since(band.turn.unwrap_or(now)).unwrap_or_default(),
            };
            t.draw_table(&self.shared.gpu, &scene)
        } else {
            let scene = RetinueScene {
                rows: &rows,
                hot: self.hot.get(),
                size,
                accent: self.accent.get(),
            };
            t.draw_retinue(&self.shared.gpu, &scene)
        };
        if drawn.is_err() {
            *slot = None;
        }
    }

    /// What is at `x`, `y` pixels in: a hireling's row by its place in
    /// `ids`, `usize::MAX` for the heading's link (the war table's, or its
    /// Close), or nothing.
    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        let hwnd = self.hwnd.get();
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let k = 96.0 / dpi as f32;
        let (x, y) = (x as f32 * k, y as f32 * k);
        let mut rc = Default::default();
        unsafe {
            let _ = GetClientRect(hwnd, &mut rc);
        }
        let w = rc.right as f32 * k;
        let (head, top, row_h) = if self.table.get() {
            // Track 0 is the main loop's, which opens nothing.
            (TABLE_HEAD, TABLE_HEAD + TABLE_AXIS + 6.0 + TABLE_ROW_H, TABLE_ROW_H)
        } else {
            (RETINUE_TOP, RETINUE_TOP, RETINUE_ROW_H)
        };
        if y < head {
            return (x > w - RETINUE_LINK_W).then_some(usize::MAX);
        }
        let i = ((y - top) / row_h).floor();
        (y >= top && (i as usize) < self.ids.borrow().len()).then_some(i as usize)
    }

    fn hover(&self, row: Option<usize>) {
        if self.hot.replace(row) != row {
            self.invalidate();
        }
        let line: Option<Line> = row.and_then(|i| {
            if i == usize::MAX {
                let what = if self.table.get() {
                    "Back to the terminal (Esc)"
                } else {
                    "The war table: this turn and its hirelings over time (Ctrl+Shift+W)"
                };
                return Some(what.into());
            }
            let id = self.ids.borrow().get(i)?.clone();
            let reg = self.shared.registry.lock().ok()?;
            let h = reg
                .get(&self.session)?
                .band
                .hirelings
                .iter()
                .find(|h| h.id == id)?
                .clone();
            let now = SystemTime::now();
            Some(format!("{}\n\nShow its conversation beside this session", h.summary(now)).into())
        });
        tip::over_line(&self.shared, self.hwnd.get(), line);
    }

    fn handle(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let x = (lparam.0 & 0xffff) as i16 as i32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
        match msg {
            WM_PAINT => {
                self.paint();
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd.get()), None);
                }
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_SIZE => {
                let (w, h) = ((lparam.0 & 0xffff) as u32, ((lparam.0 >> 16) & 0xffff) as u32);
                if let Some(t) = self.target.borrow().as_ref() {
                    let _ = t.resize(w, h);
                }
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TIMER => {
                self.poll();
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                if !self.tracking.replace(true) {
                    let mut t = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: self.hwnd.get(),
                        dwHoverTime: 0,
                    };
                    unsafe {
                        let _ = TrackMouseEvent(&mut t);
                    }
                }
                self.hover(self.row_at(x, y));
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking.set(false);
                self.hot.set(None);
                tip::away(self.hwnd.get());
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_SETCURSOR if self.hot.get().is_some() => {
                unsafe {
                    if let Ok(c) = LoadCursorW(None, IDC_HAND) {
                        SetCursor(Some(c));
                    }
                }
                Some(LRESULT(1))
            }
            WM_LBUTTONUP => {
                tip::press(self.hwnd.get());
                let at = self.row_at(x, y);
                if at == Some(usize::MAX) {
                    self.set_table(!self.table.get());
                    self.tell_pane();
                    return Some(LRESULT(0));
                }
                let id = at.and_then(|i| self.ids.borrow().get(i).cloned());
                if let Some(agent) = id {
                    app::push(Input::OpenHireling(self.session.clone(), agent));
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

/// The war table's hirelings, in the order they started.
fn table_order(band: &Band, now: SystemTime) -> Vec<&hireling::Hireling> {
    let mut all = band.shown(now, true);
    all.sort_by_key(|h| h.started);
    all
}

/// The war table's tracks, the main loop's first, in seconds from the
/// turn's start, and how many seconds the table spans.
fn tracks(band: &Band, now: SystemTime, accent: Color) -> (Vec<TableTrack>, f32) {
    let hirelings = table_order(band, now);
    let origin = band
        .turn
        .into_iter()
        .chain(hirelings.iter().map(|h| h.started))
        .chain(band.main.iter().map(|m| m.start))
        .min()
        .unwrap_or(now);
    let secs = |t: SystemTime| t.duration_since(origin).unwrap_or_default().as_secs_f32();
    let marks = |marks: &[Mark], end: f32| -> Vec<(f32, f32, Color, bool)> {
        marks
            .iter()
            .map(|m| {
                let to = m.end.map_or(end, secs);
                (secs(m.start), to, craft_ink(hireling::craft(&m.tool), accent), m.failed)
            })
            .collect()
    };
    let main_end = band.turn_end.map_or(secs(now), secs);
    let mut out = vec![TableTrack {
        rune: None,
        label: "main loop".to_string(),
        start: 0.0,
        end: main_end,
        lot: if band.turn_end.is_some() { Lot::Done } else { Lot::Working },
        marks: marks(&band.main, main_end),
    }];
    for h in hirelings {
        let lot = h.lot_at(now);
        let end = match (lot.at_work(), h.ended) {
            (true, _) | (false, None) => secs(h.started + h.ran(now)),
            (false, Some(t)) => secs(t),
        };
        let label = match h.task.as_str() {
            "" => h.kind.clone(),
            t => format!("{}: {t}", h.kind),
        };
        out.push(TableTrack {
            rune: Some(hireling::rune_of(&h.kind)),
            label,
            start: secs(h.started),
            end,
            lot,
            marks: marks(&h.marks, end),
        });
    }
    let span = out.iter().map(|t| t.end).fold(1.0, f32::max);
    (out, span)
}

/// The colour of a tool's craft on the war table.
fn craft_ink(c: Craft, accent: Color) -> Color {
    match c {
        Craft::Read => theme::git_modified(),
        Craft::Edit => theme::git_added(),
        Craft::Shell => theme::waiting(),
        Craft::Web => theme::git_untracked(),
        Craft::Agent => accent,
        Craft::Other => theme::palette().term_fg.with_alpha(0.5),
    }
}

/// What a file holds past byte `from`, or None when it holds no more.
fn read_from(path: &str, from: u64) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if len <= from {
        return None;
    }
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    f.take(len - from).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn register_class() {
    thread_local!(static DONE: Cell<bool> = const { Cell::new(false) });
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            return;
        };
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: module.into(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassW(&wc);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Retinue;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The Box lives in its pane until after the window is destroyed.
    match (*ptr).handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
