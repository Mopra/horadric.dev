//! While you were away: the one screen that says what happened, opened on
//! coming back after a while, from the tray menu and from a hotkey.
//!
//! It is read in a glance. One line on top says how long, how much landed
//! and how much needs you. Under it, in full, only what needs you: the
//! questions only you can answer, each with a one line field (Enter does
//! what `quest tell` does), and the errands that failed. Everything else is
//! one line a project, which a click unfolds into what landed, what the
//! errands filed and what was assumed, each assumption with a field to
//! overrule it.
//!
//! It stands over the stage and stays there while the human looks
//! elsewhere: answering often means reading a session first. Esc or the
//! cross closes it. With nothing to tell it says so and goes by itself.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::time::Duration;

use horadric_core::chronicle::{self, Away, AwayLine};
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::DirectWrite::IDWriteTextLayout;
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MonitorFromPoint, MonitorFromWindow, ScreenToClient,
    ValidateRect, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE,
    VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCaretBlinkTime, GetWindowLongPtrW,
    GetWindowRect, IsIconic, IsWindowVisible, KillTimer, LoadCursorW, RegisterClassW, SetCursor,
    SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, CREATESTRUCTW,
    GWLP_USERDATA, IDC_ARROW, IDC_HAND, IDC_IBEAM, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOW,
    WM_CHAR, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SETCURSOR, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW,
    WS_POPUP,
};

use crate::app::{self, Input};
use crate::backdrop;
use crate::clipboard;
use crate::field::{self, Field};
use crate::layout::{self, CatchupKind, CatchupLayout, CatchupRow, Rect};
use crate::render::{self, CatchupLook, CatchupScene, FieldLook, Target};
use crate::theme::{self, Color};
use crate::window::Shared;

pub(crate) const CLASS: PCWSTR = w!("HoradricAway");

/// No real input for this long counts as away, in seconds.
pub const AWAY_AFTER: u64 = 15 * 60;
/// Input this recent counts as being back, in seconds.
const BACK_WITHIN: u64 = 5;
/// The widest it gets, in DIPs; a narrow stage gets a narrower card.
const MOST_W: f32 = 520.0;
const LEAST_W: f32 = 300.0;
/// Between the card and the edges of what it stands over, in DIPs.
const MARGIN: f32 = 24.0;
/// An answer is one line typed into a terminal.
const MAX_ANSWER: usize = 600;
const BLINK: usize = 1;
/// "Nothing new" stays this long, in milliseconds, and goes.
const QUIET: usize = 2;
const QUIET_FOR: u32 = 1600;

/// Whether you are away, from how long since the last input and whether
/// the screen is locked.
#[derive(Debug, Default)]
pub struct Absence {
    /// Since when, in Unix seconds, while away.
    since: Option<u64>,
    locked: bool,
}

impl Absence {
    /// Since when you have been away, in Unix seconds, while you are.
    pub fn since(&self) -> Option<u64> {
        self.since
    }

    pub fn locked(&self) -> bool {
        self.locked
    }

    pub fn lock(&mut self, now: u64) {
        self.locked = true;
        self.since.get_or_insert(now);
    }

    /// Unlocking is coming back. Returns since when you were away.
    pub fn unlock(&mut self) -> Option<u64> {
        self.locked = false;
        self.since.take()
    }

    /// A look at the input clock, `idle` seconds since the last input.
    /// Returns since when you were away, once, as you come back. Input on
    /// the lock screen is not coming back: unlocking is.
    pub fn idle(&mut self, idle: u64, now: u64) -> Option<u64> {
        if self.locked {
            return None;
        }
        if idle >= AWAY_AFTER {
            self.since.get_or_insert(now.saturating_sub(idle));
            None
        } else if idle <= BACK_WITHIN {
            self.since.take()
        } else {
            None
        }
    }
}

/// The local time of day at `at`, as "09:05", from the seconds since local
/// midnight `local` at `now`.
pub fn clock(local: u64, now: u64, at: u64) -> String {
    let secs = (local as i64 - (now as i64 - at as i64)).rem_euclid(86_400);
    format!("{:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

/// What the summary starts with: how long you were away, when you were,
/// else since when it tells.
pub fn lead(away_for: Option<u64>, since_clock: &str) -> String {
    match away_for {
        Some(secs) => format!("{} away", chronicle::short_span(secs)),
        None => format!("Since {since_clock}"),
    }
}

/// One project's news.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct News {
    pub name: String,
    /// Its folder, where an answer goes.
    pub dir: String,
    pub away: Away,
}

/// What a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// A question only the human can answer.
    Asks,
    Failed,
    /// A project's folded line.
    Fold,
    Landed,
    Filed,
    /// A choice made while the human was away, for them to overrule.
    Assumed,
    /// A question answered or an assumption overruled.
    Told,
    Field,
}

/// One row of the card.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub kind: CatchupKind,
    pub tone: Tone,
    pub text: String,
    pub detail: String,
    pub age: String,
    /// The project a click on it unfolds or folds.
    pub fold: Option<usize>,
    /// The answer a field is for.
    pub slot: Option<usize>,
}

/// A question's answer, for the project at `dir`, or the human's word
/// against what was `assumed` on the quest.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub dir: String,
    pub title: String,
    pub assumed: Option<String>,
}

/// The answers the fields stand for: every question first, then every
/// assumption, each in the projects' order. [`rows`] numbers its fields
/// the same way, so a field keeps its answer as projects fold.
pub fn answers(news: &[News]) -> Vec<Answer> {
    let questions = news.iter().flat_map(|n| {
        n.away.questions.iter().map(|q| Answer {
            dir: n.dir.clone(),
            title: q.title.clone(),
            assumed: None,
        })
    });
    let assumed = news.iter().flat_map(|n| {
        n.away.assumed.iter().map(|x| Answer {
            dir: n.dir.clone(),
            title: x.title.clone(),
            assumed: Some(x.text.clone()),
        })
    });
    questions.chain(assumed).collect()
}

/// The rows for `news`: what needs you in full, then a line a project for
/// the rest, unfolded where `open` says. `sent` says which answers went,
/// by slot.
pub fn rows(news: &[News], open: &[bool], sent: &[bool], now: u64) -> Vec<Row> {
    let age = |at: u64| horadric_core::format_age(Duration::from_secs(now.saturating_sub(at)));
    // With one project its name would only repeat.
    let named = |name: &str, what: &str| {
        if news.len() > 1 && !what.is_empty() {
            format!("{name} \u{00B7} {what}")
        } else if news.len() > 1 {
            name.to_string()
        } else {
            what.to_string()
        }
    };
    let told = |slot: usize| sent.get(slot).copied().unwrap_or(false);
    let mut out = Vec::new();
    let line = |tone: Tone, text: String, detail: String, age: String| Row {
        kind: CatchupKind::Line {
            detail: !detail.is_empty(),
        },
        tone,
        text,
        detail,
        age,
        fold: None,
        slot: None,
    };
    let field = |slot: usize| Row {
        kind: CatchupKind::Field,
        tone: Tone::Field,
        text: String::new(),
        detail: String::new(),
        age: String::new(),
        fold: None,
        slot: Some(slot),
    };
    let mut slot = 0;
    for n in news {
        for q in &n.away.questions {
            let (tone, age) = if told(slot) {
                (Tone::Told, "told".to_string())
            } else {
                (Tone::Asks, String::new())
            };
            out.push(line(tone, q.title.clone(), named(&n.name, &q.question), age));
            out.push(field(slot));
            slot += 1;
        }
    }
    for n in news {
        for l in &n.away.failed {
            out.push(line(
                Tone::Failed,
                l.text.clone(),
                named(&n.name, &l.detail),
                age(l.at),
            ));
        }
    }
    for (p, n) in news.iter().enumerate() {
        let a = &n.away;
        let fold = a.fold();
        let unfolded = open.get(p).copied().unwrap_or(false);
        if !fold.is_empty() {
            out.push(Row {
                fold: Some(p),
                ..line(
                    Tone::Fold,
                    format!("{} \u{00B7} {fold}", n.name),
                    String::new(),
                    if unfolded { "hide" } else { "show" }.to_string(),
                )
            });
        }
        if fold.is_empty() || !unfolded {
            slot += a.assumed.len();
            continue;
        }
        let plain = |tone: Tone, l: &AwayLine| line(tone, l.text.clone(), l.detail.clone(), age(l.at));
        out.extend(a.landed.iter().map(|l| plain(Tone::Landed, l)));
        out.extend(a.filed.iter().map(|l| plain(Tone::Filed, l)));
        for x in &a.assumed {
            let (tone, when) = if told(slot) {
                (Tone::Told, "overruled".to_string())
            } else {
                (Tone::Assumed, age(x.at))
            };
            out.push(line(tone, x.title.clone(), format!("assumed {}", x.text), when));
            out.push(field(slot));
            slot += 1;
        }
    }
    out
}

/// How wide the card is over something `room` DIPs wide.
pub fn width(room: f32) -> f32 {
    (room - 2.0 * MARGIN).clamp(LEAST_W, MOST_W)
}

/// The next field from `from` that can take an answer, going forward or
/// back and round, if any can.
pub fn next_open(open: &[bool], from: usize, forward: bool) -> Option<usize> {
    let n = open.len();
    (1..=n)
        .map(|k| {
            if forward {
                (from + k) % n
            } else {
                (from + n * 2 - k) % n
            }
        })
        .find(|&i| open[i])
}

/// What the last line says about the lines that do not show: how many
/// are under it while any are, else how many are above.
pub fn more(above: usize, below: usize) -> String {
    if below > 0 || above == 0 {
        format!("and {below} more")
    } else {
        format!("{above} more above")
    }
}

fn tone(t: Tone) -> Option<Color> {
    match t {
        Tone::Asks | Tone::Failed => Some(theme::error()),
        Tone::Landed => Some(theme::done()),
        Tone::Filed => Some(theme::quest()),
        Tone::Assumed => Some(theme::waiting()),
        Tone::Fold | Tone::Told | Tone::Field => None,
    }
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

/// One answer field as it is edited.
struct Slot {
    field: Field,
    scroll: f32,
    sent: bool,
}

pub struct AwayCard {
    pub hwnd: HWND,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    layout: RefCell<CatchupLayout>,
    width: f32,
    max_h: f32,
    /// What it stands over, in screen pixels, which it stays inside as it
    /// grows.
    over: [i32; 4],
    kinds: RefCell<Vec<CatchupKind>>,
    scale: f32,
    title: String,
    now: u64,
    news: Vec<News>,
    open: RefCell<Vec<bool>>,
    rows: RefCell<Vec<Row>>,
    answers: Vec<Answer>,
    slots: RefCell<Vec<Slot>>,
    focus: Cell<usize>,
    lit: Cell<bool>,
    /// The first half of a surrogate pair typed.
    high: Cell<Option<u16>>,
    /// The fold line under the mouse, as an index into the rows placed.
    hot: Cell<Option<usize>>,
    pressed: Cell<Option<usize>>,
    close_hot: Cell<bool>,
    pressed_close: Cell<bool>,
    closed: Cell<bool>,
}

impl AwayCard {
    /// Opens over `stage` when it shows, else in the middle of the primary
    /// screen. With no `news` it says "Nothing new" and goes by itself.
    pub fn open(
        shared: Rc<Shared>,
        stage: Option<HWND>,
        title: String,
        news: Vec<News>,
        now: u64,
    ) -> Result<Box<Self>> {
        let stage =
            stage.filter(|&h| unsafe { IsWindowVisible(h).as_bool() && !IsIconic(h).as_bool() });
        let monitor = match stage {
            Some(h) => unsafe { MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST) },
            None => unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) },
        };
        let (mut dx, mut dy) = (96u32, 96u32);
        unsafe {
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        }
        let s = dx.max(96) as f32 / 96.0;
        let work = work_area(monitor);
        let over = match stage {
            Some(h) => {
                let mut r = RECT::default();
                unsafe {
                    let _ = GetWindowRect(h, &mut r);
                }
                // The part of the stage on the work area.
                [
                    r.left.max(work[0]),
                    r.top.max(work[1]),
                    r.right.min(work[2]),
                    r.bottom.min(work[3]),
                ]
            }
            None => work,
        };
        let width = width((over[2] - over[0]) as f32 / s);
        let max_h = ((over[3] - over[1]) as f32 / s - 2.0 * MARGIN).max(200.0);
        let answers = answers(&news);
        let open = vec![false; news.len()];
        let sent = vec![false; answers.len()];
        let rows = rows(&news, &open, &sent, now);
        let kinds: Vec<CatchupKind> = rows.iter().map(|r| r.kind).collect();
        let layout = layout::catchup(&kinds, width, max_h, 0);
        let size = (
            (layout.size.0 * s).round() as i32,
            (layout.size.1 * s).round() as i32,
        );
        let (x, y) = layout::catchup_place(size, over);
        let slots = answers
            .iter()
            .map(|_| Slot {
                field: Field::new("", false, MAX_ANSWER),
                scroll: 0.0,
                sent: false,
            })
            .collect();
        let quiet = rows.is_empty();
        let mut win = Box::new(AwayCard {
            hwnd: HWND::default(),
            shared,
            target: RefCell::new(None),
            layout: RefCell::new(layout),
            width,
            max_h,
            over,
            kinds: RefCell::new(kinds),
            scale: s,
            title,
            now,
            news,
            open: RefCell::new(open),
            rows: RefCell::new(rows),
            answers,
            slots: RefCell::new(slots),
            focus: Cell::new(0),
            lit: Cell::new(true),
            high: Cell::new(None),
            hot: Cell::new(None),
            pressed: Cell::new(None),
            close_hot: Cell::new(false),
            pressed_close: Cell::new(false),
            closed: Cell::new(false),
        });
        unsafe {
            // Owned by the stage, so it stands over it and goes where it
            // goes, without standing over everything else.
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                CLASS,
                w!("Horadric: while you were away"),
                WS_POPUP,
                x,
                y,
                size.0,
                size.1,
                stage,
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*win as *const AwayCard as *const c_void),
            )?;
            win.hwnd = hwnd;
            let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            backdrop::border(hwnd, None);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            SetTimer(Some(hwnd), BLINK, GetCaretBlinkTime(), None);
            if quiet {
                SetTimer(Some(hwnd), QUIET, QUIET_FOR, None);
            }
        }
        if let Some(i) = next_open(&win.open_slots(), win.answers.len().saturating_sub(1), true) {
            win.focus.set(i);
        }
        Ok(win)
    }

    pub fn destroy(&self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// Which fields can take an answer: shown, and not answered yet.
    fn open_slots(&self) -> Vec<bool> {
        let rows = self.rows.borrow();
        let slots = self.slots.borrow();
        (0..slots.len())
            .map(|i| !slots[i].sent && rows.iter().any(|r| r.slot == Some(i)))
            .collect()
    }

    /// The index among the rows of field `i`, when it is one of them.
    fn field_row(&self, i: usize) -> Option<usize> {
        self.rows.borrow().iter().position(|r| r.slot == Some(i))
    }

    /// The well of field `i`, when it shows.
    fn field_rect(&self, i: usize) -> Option<Rect> {
        let index = self.field_row(i)?;
        let l = self.layout.borrow();
        match l.rows.get(index.checked_sub(l.first)?) {
            Some(CatchupRow::Field(r)) => Some(*r),
            _ => None,
        }
    }

    fn text_layout(&self, f: &Field, rect: &Rect) -> Option<IDWriteTextLayout> {
        let inner = layout::ask_inner(rect);
        render::field_layout(&self.shared.gpu, &f.text, &inner, false).ok()
    }

    fn paint(&self) {
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            let s = self.scale;
            let size = self.layout.borrow().size;
            let (w, h) = ((size.0 * s).round() as u32, (size.1 * s).round() as u32);
            match Target::new(&self.shared.gpu, self.hwnd, w, h, (s * 96.0).round() as u32) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for the away card: {e}");
                    return;
                }
            }
        }
        let mut slots = self.slots.borrow_mut();
        let rects: Vec<Option<Rect>> = (0..slots.len()).map(|i| self.field_rect(i)).collect();
        let texts: Vec<Option<IDWriteTextLayout>> = slots
            .iter()
            .zip(&rects)
            .map(|(s, r)| r.and_then(|r| self.text_layout(&s.field, &r)))
            .collect();
        // The fields in the order they stand, as the scene wants them.
        let mut order: Vec<usize> = (0..slots.len()).filter(|&i| rects[i].is_some()).collect();
        order.sort_by_key(|&i| self.field_row(i));
        let mut fields = Vec::new();
        for i in order {
            let (Some(rect), Some(text)) = (&rects[i], &texts[i]) else {
                continue;
            };
            let s = &mut slots[i];
            let inner = layout::ask_inner(rect);
            let f = &s.field;
            let caret = render::caret_at(text, field::utf16_at(&f.text, f.caret));
            let (w, _) = render::text_size(text);
            s.scroll = field::follow(s.scroll, caret.x, 2.0, inner.w, w + 2.0);
            let (a, b) = f.selection();
            let (a, b) = (field::utf16_at(&f.text, a), field::utf16_at(&f.text, b));
            let focused = i == self.focus.get() && !s.sent;
            fields.push(FieldLook {
                rect: *rect,
                text,
                placeholder: s.field.text.is_empty().then_some(match self.answers.get(i) {
                    _ if s.sent => "",
                    Some(a) if a.assumed.is_some() => "Leave it, or overrule it in one line",
                    _ => "Answer, and Enter tells the quest",
                }),
                multiline: false,
                scroll: (s.scroll, 0.0),
                selection: if focused {
                    render::range_rects(text, a, b - a)
                } else {
                    Vec::new()
                },
                caret: (focused && self.lit.get()).then_some(caret),
                focused,
            });
        }
        let layout = self.layout.borrow();
        let rows = self.rows.borrow();
        let more = more_of(&rows, &layout);
        let looks: Vec<CatchupLook> = rows[layout.first.min(rows.len())..]
            .iter()
            .map(|r| CatchupLook {
                text: &r.text,
                detail: &r.detail,
                age: &r.age,
                tone: tone(r.tone),
                background: false,
            })
            .collect();
        let scene = CatchupScene {
            layout: &layout,
            title: &self.title,
            sub: "",
            rows: &looks,
            more: &more,
            hot: self.hot.get(),
            close_hot: self.close_hot.get(),
            fields,
        };
        let failed = slot
            .as_ref()
            .map(|t| t.draw_catchup(&self.shared.gpu, &self.shared.metrics, &scene))
            .is_some_and(|r| r.is_err());
        if failed {
            *slot = None;
        }
    }

    fn point(&self, lparam: LPARAM) -> (f32, f32) {
        let x = (lparam.0 & 0xffff) as i16 as f32 / self.scale;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / self.scale;
        (x, y)
    }

    fn on_close(&self, lparam: LPARAM) -> bool {
        let (x, y) = self.point(lparam);
        self.layout.borrow().close.contains(x, y)
    }

    /// The fold line under a point, as an index into the rows placed.
    fn fold_at(&self, lparam: LPARAM) -> Option<usize> {
        let (x, y) = self.point(lparam);
        let l = self.layout.borrow();
        let i = layout::catchup_hit(&l, x, y)?;
        let rows = self.rows.borrow();
        rows.get(l.first + i)?.fold.map(|_| i)
    }

    /// The field under a point, if one is.
    fn field_at(&self, lparam: LPARAM) -> Option<usize> {
        let (x, y) = self.point(lparam);
        (0..self.answers.len()).find(|&i| self.field_rect(i).is_some_and(|r| r.contains(x, y)))
    }

    /// The wheel moves the rows a notch at a time, the size staying.
    fn wheel(&self, wparam: WPARAM) {
        let notches = ((wparam.0 >> 16) & 0xffff) as i16 as i32 / 120;
        let first = self.layout.borrow().first;
        let kinds = self.kinds.borrow();
        let to = layout::catchup_scroll(&kinds, self.width, self.max_h, first, notches);
        if to == first {
            return;
        }
        *self.layout.borrow_mut() = layout::catchup(&kinds, self.width, self.max_h, to);
        self.hot.set(None);
        self.invalidate();
    }

    /// A project's line clicked: it unfolds, or folds back, and the card
    /// grows or shrinks to fit, staying inside what it stands over.
    fn toggle(&self, placed: usize) {
        let Some(p) = self
            .rows
            .borrow()
            .get(self.layout.borrow().first + placed)
            .and_then(|r| r.fold)
        else {
            return;
        };
        if let Some(o) = self.open.borrow_mut().get_mut(p) {
            *o = !*o;
        }
        let sent: Vec<bool> = self.slots.borrow().iter().map(|s| s.sent).collect();
        let rows = rows(&self.news, &self.open.borrow(), &sent, self.now);
        let kinds: Vec<CatchupKind> = rows.iter().map(|r| r.kind).collect();
        let all = layout::catchup(&kinds, self.width, self.max_h, 0);
        let l = if all.more.is_none() {
            all
        } else {
            let first = self.layout.borrow().first.min(kinds.len());
            layout::catchup(&kinds, self.width, self.max_h, first)
        };
        let s = self.scale;
        let size = (
            (l.size.0 * s).round() as i32,
            (l.size.1 * s).round() as i32,
        );
        *self.rows.borrow_mut() = rows;
        *self.kinds.borrow_mut() = kinds;
        *self.layout.borrow_mut() = l;
        self.hot.set(None);
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut r);
        }
        let top = r.top.min(self.over[3] - size.1).max(self.over[1]);
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                r.left,
                top,
                size.0,
                size.1,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        if let Some(t) = self.target.borrow().as_ref() {
            if t.resize(size.0 as u32, size.1 as u32).is_err() {
                *self.target.borrow_mut() = None;
            }
        }
        let open = self.open_slots();
        if !open.get(self.focus.get()).copied().unwrap_or(false) {
            if let Some(n) = next_open(&open, self.focus.get(), true) {
                self.focus.set(n);
            }
        }
        self.changed();
    }

    /// Something changed: the caret shows at once and starts its blink
    /// over.
    fn changed(&self) {
        self.lit.set(true);
        unsafe {
            SetTimer(Some(self.hwnd), BLINK, GetCaretBlinkTime(), None);
        }
        self.invalidate();
    }

    fn edit(&self, f: impl FnOnce(&mut Field)) {
        let i = self.focus.get();
        if !self.open_slots().get(i).copied().unwrap_or(false) {
            return;
        }
        if let Some(s) = self.slots.borrow_mut().get_mut(i) {
            f(&mut s.field);
        }
        self.changed();
    }

    /// Moves the keyboard to field `i`, scrolling it into view.
    fn focus_on(&self, i: usize) {
        self.focus.set(i);
        if self.field_rect(i).is_none() {
            let index = self.field_row(i).unwrap_or(0);
            let kinds = self.kinds.borrow();
            // Down until the field shows, its line above it.
            let mut first = self.layout.borrow().first.min(index.saturating_sub(1));
            loop {
                let l = layout::catchup(&kinds, self.width, self.max_h, first);
                let shows = l.first <= index && index < l.first + l.rows.len();
                if shows || first >= index {
                    *self.layout.borrow_mut() = l;
                    break;
                }
                first += 1;
            }
        }
        self.changed();
    }

    /// Enter: the answer goes to its quest's session as `quest tell` would
    /// send it, and the keyboard moves to the next field.
    fn send(&self) {
        let i = self.focus.get();
        if !self.open_slots().get(i).copied().unwrap_or(false) {
            return;
        }
        let text = self.slots.borrow()[i].field.text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let Some(a) = self.answers.get(i) else { return };
        app::push(Input::Answered {
            dir: a.dir.clone(),
            title: a.title.clone(),
            assumed: a.assumed.clone(),
            text,
        });
        self.slots.borrow_mut()[i].sent = true;
        // Its line reads as told, the lamp out.
        let told = if a.assumed.is_some() {
            "overruled"
        } else {
            "told"
        };
        if let Some(k) = self.field_row(i).and_then(|k| k.checked_sub(1)) {
            if let Some(r) = self.rows.borrow_mut().get_mut(k) {
                r.tone = Tone::Told;
                r.age = told.to_string();
            }
        }
        match next_open(&self.open_slots(), i, true) {
            Some(n) => self.focus_on(n),
            None => self.changed(),
        }
    }

    fn key(&self, vk: u16) {
        let down = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) } < 0;
        let (ctrl, shift) = (down(VK_CONTROL), down(VK_SHIFT));
        let open = self.open_slots();
        let step = |forward: bool| {
            if let Some(n) = next_open(&open, self.focus.get(), forward) {
                self.focus_on(n);
            }
        };
        match VIRTUAL_KEY(vk) {
            VK_ESCAPE => self.close(),
            VK_RETURN => self.send(),
            VK_TAB => step(!shift),
            VK_DOWN => step(true),
            VK_UP => step(false),
            VK_LEFT => self.edit(|f| f.left(ctrl, shift)),
            VK_RIGHT => self.edit(|f| f.right(ctrl, shift)),
            VK_HOME => self.edit(|f| f.home(true, shift)),
            VK_END => self.edit(|f| f.end(true, shift)),
            VK_BACK => self.edit(|f| f.backspace(ctrl)),
            VK_DELETE => self.edit(|f| f.delete(ctrl)),
            _ if !ctrl => {}
            k => match char::from_u32(k.0 as u32) {
                Some('A') => self.edit(Field::select_all),
                Some('C') => {
                    let slots = self.slots.borrow();
                    if let Some(s) = slots.get(self.focus.get()) {
                        let t = s.field.selected();
                        if !t.is_empty() {
                            clipboard::set_text(t);
                        }
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

    /// Hides the card and tells the app, which destroys it.
    fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), BLINK);
            let _ = KillTimer(Some(self.hwnd), QUIET);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        app::push(Input::AwayClosed);
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
            WM_TIMER if wparam.0 == BLINK => {
                self.lit.set(!self.lit.get());
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == QUIET => {
                self.close();
                Some(LRESULT(0))
            }
            WM_SETCURSOR => {
                let mut p = POINT::default();
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p);
                    let _ = ScreenToClient(self.hwnd, &mut p);
                }
                let at = LPARAM(((p.y as u16 as isize) << 16) | p.x as u16 as isize);
                let cursor = if self.field_at(at).is_some() {
                    IDC_IBEAM
                } else if self.fold_at(at).is_some() {
                    IDC_HAND
                } else {
                    IDC_ARROW
                };
                unsafe {
                    if let Ok(c) = LoadCursorW(None, cursor) {
                        SetCursor(Some(c));
                    }
                }
                Some(LRESULT(1))
            }
            WM_MOUSEMOVE => {
                let close = self.on_close(lparam);
                let hot = self.fold_at(lparam);
                if self.close_hot.replace(close) != close || self.hot.replace(hot) != hot {
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                unsafe {
                    let _ = SetFocus(Some(self.hwnd));
                }
                self.pressed_close.set(self.on_close(lparam));
                self.pressed.set(self.fold_at(lparam));
                if let Some(i) = self.field_at(lparam) {
                    if self.open_slots().get(i).copied().unwrap_or(false) {
                        self.focus.set(i);
                        self.changed();
                    }
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                let pressed = self.pressed.take();
                if self.pressed_close.take() && self.on_close(lparam) {
                    self.close();
                } else if let (Some(p), Some(h)) = (pressed, self.fold_at(lparam)) {
                    if p == h {
                        self.toggle(h);
                    }
                }
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                self.wheel(wparam);
                Some(LRESULT(0))
            }
            WM_KEYDOWN => {
                self.key(wparam.0 as u16);
                Some(LRESULT(0))
            }
            WM_CHAR => {
                self.typed(wparam.0 as u16);
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

/// What the last line says for `layout` over `rows`, counting lines only.
fn more_of(rows: &[Row], layout: &CatchupLayout) -> String {
    let lines = |rows: &[Row]| {
        rows.iter()
            .filter(|r| matches!(r.kind, CatchupKind::Line { .. }))
            .count()
    };
    more(
        lines(&rows[..layout.first.min(rows.len())]),
        lines(&rows[layout.below(rows.len())]),
    )
}

/// The work area of `monitor`, as left, top, right, bottom.
fn work_area(monitor: HMONITOR) -> [i32; 4] {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let w = info.rcWork;
            [w.left, w.top, w.right, w.bottom]
        } else {
            [0, 0, 1280, 720]
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const AwayCard;
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
    use horadric_core::chronicle::{Assumption, Question};

    fn line(at: u64, text: &str, detail: &str) -> AwayLine {
        AwayLine {
            at,
            text: text.into(),
            detail: detail.into(),
        }
    }

    fn news(name: &str, away: Away) -> News {
        News {
            name: name.into(),
            dir: format!("c:/code/{name}"),
            away,
        }
    }

    fn app() -> Away {
        Away {
            landed: vec![line(40, "Serve the API", "Served on 4100")],
            filed: vec![line(60, "Feedback", "filed \u{201C}B\u{201D}")],
            failed: vec![line(50, "Clean", "failed: exit 1")],
            questions: vec![Question {
                title: "Pick a port".into(),
                question: "Which port?".into(),
                new: true,
            }],
            assumed: vec![Assumption {
                at: 70,
                title: "Serve the API".into(),
                text: "port 4100".into(),
            }],
        }
    }

    fn tones(rows: &[Row]) -> Vec<Tone> {
        rows.iter().map(|r| r.tone).collect()
    }

    #[test]
    fn what_needs_you_comes_first_and_the_rest_is_one_folded_line() {
        let n = [news("app", app())];
        let rows = rows(&n, &[false], &[false, false], 100);
        assert_eq!(tones(&rows), [Tone::Asks, Tone::Field, Tone::Failed, Tone::Fold]);
        assert_eq!(rows[0].text, "Pick a port");
        assert_eq!(rows[0].detail, "Which port?", "one project, no name");
        assert_eq!(rows[1].slot, Some(0));
        assert_eq!(rows[2].age, "50 s");
        assert_eq!(rows[3].text, "app \u{00B7} 1 landed, 1 errand, 1 assumed");
        assert_eq!(rows[3].fold, Some(0));
        assert_eq!(rows[3].age, "show");
        assert_eq!(rows[3].kind, CatchupKind::Line { detail: false });
    }

    #[test]
    fn an_unfolded_project_shows_its_lines_and_a_field_for_each_assumption() {
        let n = [news("app", app())];
        let rows = rows(&n, &[true], &[false, false], 100);
        assert_eq!(
            tones(&rows),
            [
                Tone::Asks,
                Tone::Field,
                Tone::Failed,
                Tone::Fold,
                Tone::Landed,
                Tone::Filed,
                Tone::Assumed,
                Tone::Field,
            ]
        );
        assert_eq!(rows[3].age, "hide");
        assert_eq!(rows[6].detail, "assumed port 4100");
        assert_eq!(rows[7].slot, Some(1));
    }

    #[test]
    fn with_more_than_one_project_each_line_names_its_own() {
        let quiet = Away {
            questions: vec![Question {
                title: "Name it".into(),
                question: String::new(),
                new: false,
            }],
            assumed: vec![Assumption {
                at: 1,
                title: "X".into(),
                text: "y".into(),
            }],
            ..Away::default()
        };
        let n = [news("app", app()), news("web", quiet)];
        let answers = answers(&n);
        let got: Vec<(&str, bool)> = answers
            .iter()
            .map(|a| (a.title.as_str(), a.assumed.is_some()))
            .collect();
        assert_eq!(
            got,
            [
                ("Pick a port", false),
                ("Name it", false),
                ("Serve the API", true),
                ("X", true)
            ]
        );
        assert_eq!(answers[1].dir, "c:/code/web");
        let rows = rows(&n, &[false, true], &[false; 4], 100);
        assert_eq!(rows[0].detail, "app \u{00B7} Which port?");
        assert_eq!(rows[2].detail, "web", "no question, just the name");
        assert_eq!(rows[3].slot, Some(1));
        // The first project folded, its assumption keeps slot 2 and the
        // second's is slot 3.
        let last = rows.iter().rev().find(|r| r.slot.is_some()).unwrap();
        assert_eq!(last.slot, Some(3));
    }

    #[test]
    fn an_answer_sent_reads_as_told() {
        let n = [news("app", app())];
        let rows = rows(&n, &[true], &[true, true], 100);
        assert_eq!(rows[0].tone, Tone::Told);
        assert_eq!(rows[0].age, "told");
        assert_eq!(rows[6].tone, Tone::Told);
        assert_eq!(rows[6].age, "overruled");
    }

    #[test]
    fn nothing_to_tell_is_no_rows() {
        assert!(rows(&[], &[], &[], 100).is_empty());
        assert!(rows(&[news("app", Away::default())], &[false], &[], 100).is_empty());
    }

    #[test]
    fn the_lead_says_how_long_you_were_away_or_since_when() {
        assert_eq!(lead(Some(3 * 3600 + 300), "09:00"), "3 h away");
        assert_eq!(lead(None, "08:00"), "Since 08:00");
    }

    #[test]
    fn the_card_fits_a_narrow_stage_and_stops_growing_on_a_wide_one() {
        assert_eq!(width(2000.0), MOST_W);
        assert_eq!(width(400.0), 400.0 - 2.0 * MARGIN);
        assert_eq!(width(100.0), LEAST_W);
    }

    #[test]
    fn the_keyboard_goes_round_the_fields_that_can_take_an_answer() {
        assert_eq!(next_open(&[true, false, true], 0, true), Some(2));
        assert_eq!(next_open(&[true, false, true], 2, true), Some(0));
        assert_eq!(next_open(&[true, false, true], 0, false), Some(2));
        assert_eq!(next_open(&[false, false, true], 2, true), Some(2));
        assert_eq!(next_open(&[false, false], 0, true), None);
        assert_eq!(next_open(&[], 0, true), None);
    }

    #[test]
    fn idling_long_enough_is_away_and_input_after_it_is_back() {
        let mut a = Absence::default();
        assert_eq!(a.idle(60, 1000), None);
        assert_eq!(a.idle(0, 1000), None, "never away, nothing to tell");
        assert_eq!(a.idle(AWAY_AFTER, 5000), None);
        assert_eq!(a.idle(AWAY_AFTER + 600, 5600), None);
        assert_eq!(a.idle(1, 5700), Some(5000 - AWAY_AFTER));
        assert_eq!(a.idle(1, 5701), None, "said once");
    }

    #[test]
    fn a_locked_screen_is_away_until_it_unlocks() {
        let mut a = Absence::default();
        a.lock(100);
        assert_eq!(a.idle(0, 200), None, "typing the password");
        assert_eq!(a.idle(AWAY_AFTER + 10, 2000), None);
        assert_eq!(a.unlock(), Some(100));
        assert_eq!(a.unlock(), None);
    }

    #[test]
    fn the_last_line_counts_what_is_under_it_then_what_is_above() {
        assert_eq!(more(0, 4), "and 4 more");
        assert_eq!(more(3, 4), "and 4 more");
        assert_eq!(more(7, 0), "7 more above");
    }

    #[test]
    fn the_clock_gives_the_local_time_of_a_moment() {
        // 10:30 local now; an hour and a quarter ago was 09:15.
        let now = 1_000_000;
        assert_eq!(clock(10 * 3600 + 1800, now, now - 4500), "09:15");
        // Before midnight wraps to the day before.
        assert_eq!(clock(600, now, now - 1200), "23:50");
    }
}
