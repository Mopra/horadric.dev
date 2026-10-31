//! A pane's retinue: a portrait per hireling of its session, in a column
//! at the glass's left, like the party frame in the games. See
//! docs/plan/hirelings.md.
//!
//! A child window of the pane with its own render target, like the
//! Folio's bar, so the grid's drawing is left alone: the pane only gives
//! up the width it needs, and as the column is the pane's and not the
//! scrollback's it stays in sight however long the conversation grows.
//! It reads the session's band from the registry on a timer of its own,
//! fast while a rune carves or beats and slow while all rest, and tells
//! the pane when the column comes or goes. A click on a portrait opens
//! that hireling's conversation in a pane beside it.
//!
//! The band only learns a hireling's tokens and tool count from its
//! transcript, so the column reads what each one's has grown by.
//!
//! Turned over (Ctrl+Shift+W, the glyph atop the column, or the line on
//! the tile that counts them) it is the war table: the same window over
//! the whole glass, the turn drawn on one axis of time, a track for the
//! main loop and one per hireling. The grid under it keeps its size, so
//! the program never redraws for it. Hovering a tick on a track says
//! which tool it was and what it was given.
//!
//! Alt+Down hands it the keyboard: the arrows walk the portraits (or the
//! tracks), Enter opens one, Esc gives the keyboard back. The pane holds
//! the keys and asks it what each one does, since the focus stays in the
//! pane for the program's sake.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::io::{Read, Seek, SeekFrom};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use horadric_core::hireling::{self, Band, Craft, Lot, Mark};
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{InvalidateRect, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetParent, GetWindowLongPtrW, LoadCursorW,
    PostMessageW, RegisterClassW, SetCursor, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    CREATESTRUCTW, GWLP_USERDATA, HWND_TOP, IDC_HAND, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNA,
    WINDOW_EX_STYLE, WM_ERASEBKGND, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY,
    WM_PAINT, WM_SETCURSOR, WM_SIZE, WM_TIMER, WM_USER, WNDCLASSW, WS_CHILD, WS_CLIPSIBLINGS,
};

use crate::app::{self, Input};
use crate::render::{
    retinue_fit, tick_span, RetinueRow, RetinueScene, TableScene, TableTrack, Target,
    RETINUE_CELL, RETINUE_HEAD, RETINUE_W, TABLE_AXIS, TABLE_HEAD, TABLE_ROW_H,
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
/// The latest tool calls a portrait's track is given.
const TICKS: usize = 16;
/// The war table's Close at the right of its heading, as wide, in DIPs.
const CLOSE_W: f32 = 150.0;
/// Posted to the pane when the column comes or goes.
pub const WM_RETINUE_SHOWN: u32 = WM_USER + 21;

/// How wide a column of `rows` portraits is, in DIPs. None takes nothing.
pub fn width(rows: usize) -> f32 {
    if rows == 0 {
        0.0
    } else {
        RETINUE_W
    }
}

pub struct Retinue {
    hwnd: Cell<HWND>,
    shared: Rc<Shared>,
    /// The session whose hirelings it shows.
    session: String,
    target: RefCell<Option<Target>>,
    /// What each portrait stands for, by hireling id, as last drawn.
    ids: RefCell<Vec<String>>,
    rows: Cell<usize>,
    /// The portrait under the cursor; `usize::MAX` for the war table's
    /// glyph, or its Close.
    hot: Cell<Option<usize>>,
    /// The war table's tick under the cursor: its track, the main loop's
    /// being 0, and its place in that track's marks.
    tick: Cell<Option<(usize, usize)>>,
    /// Holding the keyboard: the arrows move `hot`, Enter opens it.
    keyed: Cell<bool>,
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
            tick: Cell::new(None),
            keyed: Cell::new(false),
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

    /// How many portraits it has now, shown or behind the "+N".
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
            self.tick.set(None);
            self.poll();
            // Keys held carry over to the first of what is shown now.
            self.hot.set(self.keyed.get().then(|| self.first()));
            self.invalidate();
        }
    }

    /// Whether it holds the keyboard.
    pub fn keyed(&self) -> bool {
        self.keyed.get()
    }

    /// Takes the keyboard, on the first portrait or track, or on the war
    /// table's glyph where there is none. False when nothing is shown.
    pub fn take_keys(&self) -> bool {
        if self.rows.get() == 0 && !self.table.get() {
            return false;
        }
        self.keyed.set(true);
        self.hot.set(Some(self.first()));
        self.invalidate();
        true
    }

    /// Gives the keyboard back.
    pub fn drop_keys(&self) {
        if self.keyed.replace(false) {
            self.hot.set(None);
            self.invalidate();
        }
    }

    /// The first portrait, or track, that a key can land on.
    fn first(&self) -> usize {
        if self.reachable() > 0 {
            0
        } else {
            usize::MAX
        }
    }

    /// How many portraits, or hirelings' tracks, are in sight to be
    /// reached by a key or a click.
    fn reachable(&self) -> usize {
        let hwnd = self.hwnd.get();
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let mut rc = Default::default();
        unsafe {
            let _ = GetClientRect(hwnd, &mut rc);
        }
        let h = rc.bottom as f32 * 96.0 / dpi as f32;
        let n = self.ids.borrow().len();
        if self.table.get() {
            // Track 0 is the main loop's, which opens nothing.
            let top = TABLE_HEAD + TABLE_AXIS + 6.0 + TABLE_ROW_H;
            n.min(((h - top) / TABLE_ROW_H).max(0.0).floor() as usize)
        } else {
            retinue_fit(n, h)
        }
    }

    /// Up or Down while it holds the keyboard: the glyph atop counts as
    /// one above the first.
    pub fn key(&self, down: bool) {
        let at = step(self.hot.get(), self.reachable(), down);
        if self.hot.replace(Some(at)) != Some(at) {
            self.invalidate();
        }
    }

    /// Opens what the keys lit, as a click on it would.
    pub fn enter(&self) {
        if let Some(at) = self.hot.get() {
            self.open(at);
        }
    }

    /// A portrait or track opens its hireling's conversation; the glyph
    /// or Close turns the war table over.
    fn open(&self, at: usize) {
        if at == usize::MAX {
            self.set_table(!self.table.get());
            self.tell_pane();
            return;
        }
        let id = self.ids.borrow().get(at).cloned();
        if let Some(agent) = id {
            // Its conversation takes the keyboard.
            self.drop_keys();
            app::push(Input::OpenHireling(self.session.clone(), agent));
        }
    }

    /// Asks the pane to lay it out again.
    fn tell_pane(&self) {
        unsafe {
            if let Ok(pane) = GetParent(self.hwnd.get()) {
                let _ = PostMessageW(Some(pane), WM_RETINUE_SHOWN, WPARAM(0), LPARAM(0));
            }
        }
    }

    pub fn set_accent(&self, c: Color) {
        if self.accent.replace(c) != c {
            self.invalidate();
        }
    }

    /// Lays it down the glass's left, or over all of it as the war table,
    /// `rect` in pixels of the pane.
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

    /// Reads the band again, redraws, and tells the pane when the column
    /// comes or goes. Sets the next beat.
    fn poll(&self) {
        let now = SystemTime::now();
        let band = self.band().unwrap_or_default();
        let shown = party(&band, now);
        let rows = shown.len();
        *self.ids.borrow_mut() = shown.iter().map(|(h, _)| h.id.clone()).collect();
        if (self.rows.replace(rows) > 0) != (rows > 0) {
            self.tell_pane();
        }
        if self.table.get() {
            *self.ids.borrow_mut() = table_order(&band, now)
                .iter()
                .map(|h| h.id.clone())
                .collect();
        }
        // A waiting rune beats, so it wants the fast beat as a carving one.
        let carving = band
            .shown(now, false)
            .iter()
            .any(|h| h.lot_at(now).at_work())
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
        let accent = self.accent.get();
        let since = |t: SystemTime| now.duration_since(t).unwrap_or_default();
        let rows: Vec<RetinueRow> = party(&band, now)
            .into_iter()
            .map(|(h, nested)| RetinueRow {
                rune: hireling::rune_of(&h.kind),
                lot: h.lot_at(now),
                age: since(h.started),
                waited: since(h.heard),
                tool: h
                    .tool
                    .as_deref()
                    .map(|t| craft_ink(hireling::craft(t), accent)),
                ticks: h.marks[h.marks.len().saturating_sub(TICKS)..]
                    .iter()
                    .map(|m| match m.failed {
                        true => theme::error(),
                        false => craft_ink(hireling::craft(&m.tool), accent),
                    })
                    .collect(),
                nested,
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
                hot: self
                    .hot
                    .get()
                    .map(|i| if i == usize::MAX { i } else { i + 1 }),
                hot_tick: self.tick.get(),
                size,
                accent: self.accent.get(),
                age: now
                    .duration_since(band.turn.unwrap_or(now))
                    .unwrap_or_default(),
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

    /// What is at `x`, `y` pixels in: a hireling's portrait or track by
    /// its place in `ids`, `usize::MAX` for the war table's glyph or its
    /// Close, or nothing.
    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        let hwnd = self.hwnd.get();
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let k = 96.0 / dpi as f32;
        let (x, y) = (x as f32 * k, y as f32 * k);
        let mut rc = Default::default();
        unsafe {
            let _ = GetClientRect(hwnd, &mut rc);
        }
        let (w, h) = (rc.right as f32 * k, rc.bottom as f32 * k);
        if !self.table.get() {
            if y < RETINUE_HEAD {
                return Some(usize::MAX);
            }
            // The "+N" past the last portrait that fits opens nothing.
            let fit = retinue_fit(self.ids.borrow().len(), h);
            let i = ((y - RETINUE_HEAD) / RETINUE_CELL).floor() as usize;
            return (i < fit).then_some(i);
        }
        if y < TABLE_HEAD {
            return (x > w - CLOSE_W).then_some(usize::MAX);
        }
        // Track 0 is the main loop's, which opens nothing.
        let top = TABLE_HEAD + TABLE_AXIS + 6.0 + TABLE_ROW_H;
        let i = ((y - top) / TABLE_ROW_H).floor();
        (y >= top && (i as usize) < self.ids.borrow().len()).then_some(i as usize)
    }

    /// The war table's tick at `x`, `y` pixels in, as its track and its
    /// place among the track's marks, and what its tip says.
    fn tick_at(&self, x: i32, y: i32) -> Option<((usize, usize), String)> {
        if !self.table.get() {
            return None;
        }
        let hwnd = self.hwnd.get();
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let k = 96.0 / dpi as f32;
        let (x, y) = (x as f32 * k, y as f32 * k);
        let mut rc = Default::default();
        unsafe {
            let _ = GetClientRect(hwnd, &mut rc);
        }
        let w = rc.right as f32 * k;
        let top = TABLE_HEAD + TABLE_AXIS + 6.0;
        let i = ((y - top) / TABLE_ROW_H).floor();
        // Only near the lane, so the rest of a row still says whose it is.
        let mid = top + (i + 0.5) * TABLE_ROW_H;
        if i < 0.0 || (y - mid).abs() > 8.0 {
            return None;
        }
        let i = i as usize;
        let now = SystemTime::now();
        let band = self
            .shared
            .registry
            .lock()
            .ok()?
            .get(&self.session)?
            .band
            .clone();
        let (tracks, span) = tracks(&band, now, self.accent.get());
        let spans: Vec<(f32, f32)> = tracks.get(i)?.marks.iter().map(|m| (m.0, m.1)).collect();
        let at = tick_under(&spans, x, span, w)?;
        let mark = match i {
            0 => band.main.get(at)?,
            _ => table_order(&band, now).get(i - 1)?.marks.get(at)?,
        };
        Some(((i, at), tick_tip(mark, now)))
    }

    fn hover(&self, row: Option<usize>, tick: Option<((usize, usize), String)>) {
        let (tick, said) = tick.unzip();
        // Moving the mouse takes over from the keys.
        let row = row.or(self.hot.get().filter(|_| self.keyed.get()));
        if self.hot.replace(row) != row || self.tick.replace(tick) != tick {
            self.invalidate();
        }
        if let Some(said) = said {
            tip::over_line(&self.shared, self.hwnd.get(), Some(said.into()));
            return;
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
            if self.table.get() {
                let text = format!(
                    "{}\n\nShow its conversation beside this session",
                    h.summary(now)
                );
                return Some(text.into());
            }
            // A portrait shows only a rune, so its tip says the rest a line
            // at a time.
            let mut lines = vec![h.kind.clone()];
            if !h.task.is_empty() {
                lines.push(h.task.clone());
            }
            let mut stats = vec![
                h.doing(now),
                horadric_core::heavy::duration(h.ran(now).as_secs()),
            ];
            if h.tokens > 0 {
                stats.push(hireling::tokens(h.tokens));
            }
            lines.push(stats.join(" \u{b7} "));
            let nested = reg
                .get(&self.session)
                .map(|s| party(&s.band, now))
                .and_then(|p| p.into_iter().find(|(p, _)| p.id == id).map(|(_, n)| n))
                .unwrap_or(0);
            match nested {
                0 => {}
                1 => lines.push("1 hireling of its own".to_string()),
                n => lines.push(format!("{n} hirelings of its own")),
            }
            lines.push(String::new());
            lines.push("Show its conversation beside this session".to_string());
            Some(lines.join("\n").into())
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
                let (w, h) = (
                    (lparam.0 & 0xffff) as u32,
                    ((lparam.0 >> 16) & 0xffff) as u32,
                );
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
                self.hover(self.row_at(x, y), self.tick_at(x, y));
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking.set(false);
                // Keys held keep what they lit.
                if !self.keyed.get() {
                    self.hot.set(None);
                }
                self.tick.set(None);
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
                if let Some(at) = self.row_at(x, y) {
                    self.open(at);
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

/// Where Up or Down from `at` lands among `n` portraits or tracks, the
/// glyph atop (`usize::MAX`) above the first. Neither wraps, so holding
/// a key stops at an end.
fn step(at: Option<usize>, n: usize, down: bool) -> usize {
    match (at, down) {
        _ if n == 0 => usize::MAX,
        (None, _) | (Some(usize::MAX), true) => 0,
        (Some(usize::MAX), false) | (Some(0), false) => usize::MAX,
        (Some(i), false) => (i - 1).min(n - 1),
        (Some(i), true) => (i + 1).min(n - 1),
    }
}

/// Which of a track's ticks, each `(from, to)` in seconds, is under `x`
/// DIPs across a war table `w` wide spanning `span` seconds. A few DIPs
/// either side count, as a quick call is drawn a sliver; where several
/// are that near, the closest wins, and of those the later, drawn on top.
fn tick_under(spans: &[(f32, f32)], x: f32, span: f32, w: f32) -> Option<usize> {
    const SLOP: f32 = 3.0;
    let mut best: Option<(usize, f32)> = None;
    for (k, &(from, to)) in spans.iter().enumerate() {
        let (a, b) = tick_span(from, to, span, w);
        let off = if x < a {
            a - x
        } else if x > b {
            x - b
        } else {
            0.0
        };
        if off <= SLOP && best.is_none_or(|(_, o)| off <= o) {
            best = Some((k, off));
        }
    }
    best.map(|(k, _)| k)
}

/// A tick's tip: the tool, how long it took or that it still runs, and
/// what it was given.
fn tick_tip(m: &Mark, now: SystemTime) -> String {
    let took = match m.end {
        Some(end) => took(end.duration_since(m.start).unwrap_or_default()),
        None => format!(
            "running {}",
            took(now.duration_since(m.start).unwrap_or_default())
        ),
    };
    let mut head = format!("{} \u{b7} {took}", m.tool);
    if m.failed {
        head.push_str(" \u{b7} failed");
    }
    match m.given.as_str() {
        "" => head,
        given => format!("{head}\n{given}"),
    }
}

/// How long a tool call took, to a tenth of a second while short, as
/// most take less than one.
fn took(d: Duration) -> String {
    let s = d.as_secs_f32();
    if s < 10.0 {
        format!("{s:.1}s")
    } else {
        horadric_core::heavy::duration(d.as_secs())
    }
}

/// The hirelings that get a portrait, most in need first, each with how
/// many of those shown are its own, sent by it or by one of its own. One
/// whose sender is no longer shown stands for itself.
fn party(band: &Band, now: SystemTime) -> Vec<(&hireling::Hireling, usize)> {
    let shown = band.shown(now, false);
    let sender = |h: &hireling::Hireling| -> Option<usize> {
        let up = h.parent.as_deref()?;
        shown.iter().position(|s| s.id == up)
    };
    // Bounded, so a loop in what the band was told cannot hang the pane.
    let heads: Vec<&str> = shown
        .iter()
        .map(|&h| {
            let mut at = h;
            for _ in 0..shown.len() {
                match sender(at) {
                    Some(i) => at = shown[i],
                    None => break,
                }
            }
            at.id.as_str()
        })
        .collect();
    shown
        .iter()
        .zip(&heads)
        .filter(|(h, top)| h.id == **top)
        .map(|(h, _)| {
            let own = heads.iter().filter(|&&t| t == h.id).count() - 1;
            (*h, own)
        })
        .collect()
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
                (
                    secs(m.start),
                    to,
                    craft_ink(hireling::craft(&m.tool), accent),
                    m.failed,
                )
            })
            .collect()
    };
    let main_end = band.turn_end.map_or(secs(now), secs);
    let mut out = vec![TableTrack {
        rune: None,
        label: "main loop".to_string(),
        start: 0.0,
        end: main_end,
        lot: if band.turn_end.is_some() {
            Lot::Done
        } else {
            Lot::Working
        },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_walk_from_the_glyph_down_and_stop_at_the_ends() {
        let g = usize::MAX;
        assert_eq!(step(None, 3, true), 0);
        assert_eq!(step(Some(g), 3, true), 0);
        assert_eq!(step(Some(0), 3, false), g);
        assert_eq!(step(Some(g), 3, false), g);
        assert_eq!(step(Some(1), 3, true), 2);
        assert_eq!(step(Some(2), 3, true), 2);
        // Portraits went while it was lit.
        assert_eq!(step(Some(5), 2, false), 1);
        assert_eq!(step(Some(5), 2, true), 1);
        assert_eq!(step(Some(1), 0, true), g);
    }

    #[test]
    fn the_nearest_tick_is_hovered_and_a_sliver_is_easy_to_hit() {
        // 1000 DIPs wide over 100 seconds: the track runs 170 to 984.
        let w = 1000.0;
        let x = |t: f32| crate::render::table_x(t, 100.0, w);
        let spans = [(0.0, 10.0), (10.0, 10.0), (50.0, 60.0)];
        assert_eq!(tick_under(&spans, x(5.0), 100.0, w), Some(0));
        // The quick one at 10 s shares an edge with the first; it is drawn
        // on top and wins.
        assert_eq!(tick_under(&spans, x(10.0) + 1.0, 100.0, w), Some(1));
        assert_eq!(tick_under(&spans, x(10.0) + 4.5, 100.0, w), Some(1));
        assert_eq!(tick_under(&spans, x(30.0), 100.0, w), None);
        assert_eq!(tick_under(&spans, x(60.0) + 2.0, 100.0, w), Some(2));
    }
}
