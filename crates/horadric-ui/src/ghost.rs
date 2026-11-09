//! What the human sees while a session drives a desktop program: every
//! screen framed by a pulsing glow, a ghost cursor that glides to where the
//! next click goes before it is made, and a Thinking plate in the top right
//! corner saying what the session is about to do and what it did.
//!
//! The ghost is slow on purpose. A click waits for it to arrive, two
//! seconds, and Esc while it glides stops that click: the human sees the
//! aim before the act and has time to say no.
//!
//! Every window here is layered with its own alpha, topmost, never
//! activated, and the mouse goes through it. Each is left out of screen
//! captures, so the session's own screenshots show the program and not the
//! frame around it.
//!
//! Its state lives here, off the app, since the frames come to its own
//! window and must not wait on the app's borrow.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::f32::consts::PI;
use std::rc::Rc;
use std::time::{Duration, Instant};

use horadric_hooks::listener::Reply;
use serde_json::json;
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED,
    D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DCRenderTarget, ID2D1RenderTarget, ID2D1SolidColorBrush,
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_GAMMA_2_2,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{IDWriteTextFormat, DWRITE_MEASURING_MODE_NATURAL};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EnumDisplayMonitors,
    GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
    HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, RegisterClassW,
    SetWindowDisplayAffinity, ShowWindow, UpdateLayeredWindow, HTTRANSPARENT, MA_NOACTIVATE,
    SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, WDA_EXCLUDEFROMCAPTURE, WM_HOTKEY, WM_MOUSEACTIVATE,
    WM_NCHITTEST, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use windows_numerics::Vector2;

use crate::motion;
use crate::render::{self, color};
use crate::theme::{self, Color};
use crate::window::Shared;

const CLASS: PCWSTR = w!("HoradricGhost");
const FRAME: usize = 1;
/// The id of the Esc that stops a glide, on the clock window, so no other
/// hotkey of the app's can share it.
const STOP_KEY: i32 = 0x6057;

/// Control counts as running this long after the last desktop call.
pub(super) const ACTIVE_FOR: Duration = Duration::from_secs(20);
/// How long the ghost takes to reach a click. Slow, so the human can see
/// where it goes and stop it before it acts.
pub(super) const GLIDE: Duration = Duration::from_secs(2);
/// The ring the ghost gives when it arrives.
const RING: Duration = Duration::from_millis(450);
/// The ghost stays this long after its last action, then fades.
const GHOST_STAYS: Duration = Duration::from_secs(3);
const GHOST_FADE: Duration = Duration::from_millis(400);
/// One breath of the frame around the screens.
const PULSE: Duration = Duration::from_secs(2);
const FADE_IN: Duration = Duration::from_millis(250);
const FADE_OUT: Duration = Duration::from_millis(700);
/// Lines of history the plate keeps.
pub(super) const HISTORY: usize = 10;

/// The glow's depth into the screen, in DIPs.
const EDGE: f32 = 8.0;
/// The plate's width, its padding, and its distance from the work area's
/// corner, clear of the frame, in DIPs.
const BOX_W: f32 = 380.0;
const PAD: f32 = 14.0;
const BOX_MARGIN: f32 = 18.0;
const ROW: f32 = 18.0;
/// The reason gets at most this many lines.
const REASON_LINES: f32 = 3.0;
const REASON_LINE: f32 = 17.0;
/// The ghost's window, with the tip of its arrow at `GHOST_TIP` from its
/// corner, room around it for the ring and beside it for the name.
const GHOST_SIZE: (f32, f32) = (230.0, 86.0);
const GHOST_TIP: f32 = 38.0;
const TAG_MAX: f32 = 150.0;

// Glyphs of the system's icon font.
const CHECK: char = '\u{E73E}';
const ERROR: char = '\u{E783}';
const CROSS: char = '\u{E711}';
const STOP: char = '\u{E71A}';

/// How an action in the history went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mark {
    Ok,
    Failed,
    /// The human said no in a question.
    Refused,
    /// The human pressed Esc while the ghost was on its way.
    Stopped,
}

impl Mark {
    pub(super) fn glyph(self) -> char {
        match self {
            Mark::Ok => CHECK,
            Mark::Failed => ERROR,
            Mark::Refused => CROSS,
            Mark::Stopped => STOP,
        }
    }

    fn colour(self) -> Color {
        match self {
            Mark::Ok => theme::done(),
            Mark::Failed | Mark::Stopped => theme::error(),
            Mark::Refused => theme::waiting(),
        }
    }
}

/// One line of the history.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Entry {
    pub at: String,
    pub mark: Mark,
    pub what: String,
    pub outcome: String,
}

/// The plate's line for an entry, after its time and mark.
pub(super) fn entry_line(e: &Entry) -> String {
    match (e.what.trim(), e.outcome.trim()) {
        ("", "") => String::new(),
        (what, "") => what.to_string(),
        ("", outcome) => outcome.to_string(),
        (what, outcome) => format!("{what}: {outcome}"),
    }
}

/// A wall clock time as the history shows it.
pub(super) fn clock(h: u16, m: u16, s: u16) -> String {
    format!("{h:02}:{m:02}:{s:02}")
}

/// `e` on top of `history`, the oldest dropped past [`HISTORY`].
pub(super) fn remember(history: &mut VecDeque<Entry>, e: Entry) {
    history.push_front(e);
    history.truncate(HISTORY);
}

/// Until when control counts as running after a call at `now`. An allowed
/// call starts it; any other call only keeps it going.
pub(super) fn extend(until: Option<Instant>, now: Instant, allowed: bool) -> Option<Instant> {
    match until {
        Some(u) if u > now => Some(now + ACTIVE_FOR),
        _ if allowed => Some(now + ACTIVE_FOR),
        _ => None,
    }
}

/// Where the ghost is `elapsed` into a glide from `from` to `to`, soft at
/// both ends.
pub(super) fn glide_at(from: (f32, f32), to: (f32, f32), elapsed: Duration) -> (f32, f32) {
    let t = motion::ease_in_out(motion::progress(elapsed, GLIDE));
    (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t)
}

/// How much of a glide is left, 1 to 0, for the bar that counts it down.
pub(super) fn left_of_glide(elapsed: Duration) -> f32 {
    1.0 - motion::progress(elapsed, GLIDE)
}

/// The frame's alpha `t` into control, breathing between faint and full.
pub(super) fn pulse(t: Duration) -> f32 {
    let phase = t.as_secs_f32() / PULSE.as_secs_f32();
    0.45 + 0.55 * (0.5 - 0.5 * (2.0 * PI * phase).cos())
}

/// The ghost's alpha `since` its last action.
pub(super) fn ghost_alpha(since: Duration) -> f32 {
    match since.checked_sub(GHOST_STAYS) {
        None => 1.0,
        Some(fading) => 1.0 - motion::progress(fading, GHOST_FADE),
    }
}

/// The four strips that frame a monitor, as x, y, width, height, `thick`
/// pixels deep: top and bottom the whole width, the sides between them.
pub(super) fn edges(monitor: [i32; 4], thick: i32) -> [[i32; 4]; 4] {
    let [l, t, r, b] = monitor;
    let (w, h) = (r - l, b - t);
    let thick = thick.min(w / 2).min(h / 2).max(1);
    let side = (h - 2 * thick).max(0);
    [
        [l, t, w, thick],
        [l, b - thick, w, thick],
        [l, t + thick, thick, side],
        [r - thick, t + thick, thick, side],
    ]
}

/// The plate's place: the top right corner of the work area, `margin`
/// in from it.
pub(super) fn box_place(work: [i32; 4], size: (i32, i32), margin: i32) -> (i32, i32) {
    let [l, t, r, _] = work;
    ((r - size.0 - margin).max(l), t + margin)
}

/// The plate's height in DIPs for a reason `reason_h` tall and `rows`
/// lines of history.
pub(super) fn box_height(reason_h: f32, rows: usize) -> f32 {
    let reason = reason_h.min(REASON_LINES * REASON_LINE);
    let now = 40.0 + 22.0 + if reason > 0.0 { reason + 4.0 } else { 0.0 } + ROW;
    let history = if rows == 0 {
        0.0
    } else {
        14.0 + 22.0 + rows as f32 * ROW
    };
    (now + history + PAD).ceil()
}

/// What is shown under NOW.
#[derive(Clone, Default)]
struct Now {
    name: String,
    program: String,
    reason: String,
    what: String,
}

/// An allowed call, as the plate and the ghost show it.
pub(super) struct Allowed {
    pub name: String,
    pub program: String,
    pub reason: String,
    pub what: String,
    /// Where the click goes, in physical screen pixels.
    pub point: Option<(i32, i32)>,
    /// The program's window, for the screen the plate goes on.
    pub hwnd: isize,
}

/// A click on its way, its answer held until the ghost arrives.
struct Glide {
    from: (f32, f32),
    to: (f32, f32),
    began: Instant,
    reply: Reply,
    what: String,
}

/// A click waiting for the one before it to land.
struct Queued {
    to: (i32, i32),
    reply: Reply,
    name: String,
    what: String,
}

/// A window drawn with its own alpha through a DIB, the way layered windows
/// take per pixel alpha.
struct Layer {
    hwnd: HWND,
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    rt: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    pos: (i32, i32),
    size: (i32, i32),
    scale: f32,
    shown: bool,
}

impl Layer {
    fn new(gpu: &render::Gpu, pos: (i32, i32), size: (i32, i32), scale: f32) -> Result<Layer> {
        unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW
                    | WS_EX_TOPMOST
                    | WS_EX_NOACTIVATE
                    | WS_EX_TRANSPARENT
                    | WS_EX_LAYERED,
                CLASS,
                w!(""),
                WS_POPUP,
                pos.0,
                pos.1,
                size.0.max(1),
                size.1.max(1),
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                None,
            )?;
            // A debug run can show them to a screen copy, to check them.
            if std::env::var_os("HORADRIC_DEBUG_OVERLAY").is_none() {
                let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
            }
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 0.0,
                dpiY: 0.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = gpu.d2d.CreateDCRenderTarget(&props)?;
            // ClearType needs to know what is behind the text, and here
            // nothing is.
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let brush = rt.CreateSolidColorBrush(&color(theme::text()), None)?;
            let dc = CreateCompatibleDC(None);
            let mut layer = Layer {
                hwnd,
                dc,
                bitmap: HBITMAP::default(),
                old: HGDIOBJ::default(),
                rt,
                brush,
                pos,
                size: (0, 0),
                scale,
                shown: false,
            };
            layer.resize(size)?;
            Ok(layer)
        }
    }

    fn resize(&mut self, size: (i32, i32)) -> Result<()> {
        let size = (size.0.max(1), size.1.max(1));
        if size == self.size {
            return Ok(());
        }
        unsafe {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: size.0,
                    // Top down, as Direct2D draws.
                    biHeight: -size.1,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap =
                CreateDIBSection(Some(self.dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let was = SelectObject(self.dc, bitmap.into());
            if self.bitmap.is_invalid() {
                self.old = was;
            } else {
                let _ = DeleteObject(self.bitmap.into());
            }
            self.bitmap = bitmap;
        }
        self.size = size;
        Ok(())
    }

    /// Draws the whole window afresh, in DIPs.
    fn paint(&self, draw: impl FnOnce(&Pen)) -> Result<()> {
        unsafe {
            let all = RECT {
                left: 0,
                top: 0,
                right: self.size.0,
                bottom: self.size.1,
            };
            self.rt.BindDC(self.dc, &all)?;
            let dpi = 96.0 * self.scale;
            self.rt.SetDpi(dpi, dpi);
            self.rt.BeginDraw();
            self.rt.Clear(Some(&D2D1_COLOR_F::default()));
            draw(&Pen {
                rt: &self.rt,
                brush: &self.brush,
            });
            self.rt.EndDraw(None, None)
        }
    }

    /// Puts what was painted on screen at `pos`, `alpha` over all of it.
    fn present(&mut self, pos: (i32, i32), alpha: f32) {
        self.pos = pos;
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let at = POINT { x: pos.0, y: pos.1 };
        let size = SIZE {
            cx: self.size.0,
            cy: self.size.1,
        };
        let from = POINT { x: 0, y: 0 };
        unsafe {
            let _ = UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&at),
                Some(&size),
                Some(self.dc),
                Some(&from),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            if !self.shown {
                self.shown = true;
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }

    fn hide(&mut self) {
        if self.shown {
            self.shown = false;
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }
}

impl Drop for Layer {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Drawing on a layer.
struct Pen<'a> {
    rt: &'a ID2D1RenderTarget,
    brush: &'a ID2D1SolidColorBrush,
}

fn d2rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}

impl Pen<'_> {
    unsafe fn fill_rounded(&self, r: D2D_RECT_F, radius: f32, c: Color) {
        self.brush.SetColor(&color(c));
        self.rt.FillRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: r,
                radiusX: radius,
                radiusY: radius,
            },
            self.brush,
        );
    }

    unsafe fn stroke_rounded(&self, r: D2D_RECT_F, radius: f32, c: Color, width: f32) {
        self.brush.SetColor(&color(c));
        self.rt.DrawRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: r,
                radiusX: radius,
                radiusY: radius,
            },
            self.brush,
            width,
            None,
        );
    }

    unsafe fn circle(&self, at: (f32, f32), r: f32, c: Color, stroke: Option<f32>) {
        self.brush.SetColor(&color(c));
        let e = D2D1_ELLIPSE {
            point: Vector2 { X: at.0, Y: at.1 },
            radiusX: r,
            radiusY: r,
        };
        match stroke {
            Some(w) => self.rt.DrawEllipse(&e, self.brush, w, None),
            None => self.rt.FillEllipse(&e, self.brush),
        }
    }

    unsafe fn text(&self, fmt: &IDWriteTextFormat, c: Color, s: &str, r: D2D_RECT_F) {
        if r.right <= r.left {
            return;
        }
        let wide: Vec<u16> = s.encode_utf16().collect();
        self.brush.SetColor(&color(c));
        self.rt.DrawText(
            &wide,
            fmt,
            &r,
            self.brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }

    /// A gradient from `stops[0]` at `from` to the last at `to`.
    unsafe fn fill_gradient(
        &self,
        r: D2D_RECT_F,
        from: (f32, f32),
        to: (f32, f32),
        stops: &[(f32, Color)],
    ) {
        let stops: Vec<D2D1_GRADIENT_STOP> = stops
            .iter()
            .map(|&(position, c)| D2D1_GRADIENT_STOP {
                position,
                color: color(c),
            })
            .collect();
        let Ok(collection) =
            self.rt
                .CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)
        else {
            return;
        };
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
            startPoint: Vector2 {
                X: from.0,
                Y: from.1,
            },
            endPoint: Vector2 { X: to.0, Y: to.1 },
        };
        if let Ok(brush) = self.rt.CreateLinearGradientBrush(&props, None, &collection) {
            self.rt.FillRectangle(&r, &brush);
        }
    }
}

/// Everything shown while control runs.
struct Overlay {
    shared: Rc<Shared>,
    /// A window never shown, where the frames and the Esc come.
    clock: HWND,
    framing: bool,
    edges: Vec<Layer>,
    plate: Option<Layer>,
    /// The plate's work area, and its monitor's scale.
    work: [i32; 4],
    scale: f32,
    ghost: Option<Layer>,
    /// The tip of the arrow, in physical pixels.
    ghost_at: Option<(f32, f32)>,
    ghost_name: String,
    /// The last time the ghost acted, for when it fades.
    ghost_acted: Instant,
    /// Stopped with Esc: it shows red and stays while control runs.
    ghost_stopped: bool,
    ring: Option<(Instant, Color)>,
    glide: Option<Glide>,
    queue: VecDeque<Queued>,
    stop_key: bool,
    until: Option<Instant>,
    /// When control began, for the pulse, and how far it has faded in.
    began: Instant,
    shown: f32,
    ticked: Instant,
    now: Now,
    history: VecDeque<Entry>,
    /// The plate's content changed since it was painted.
    stale: bool,
}

thread_local! {
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

fn registered() -> bool {
    thread_local! {
        static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if DONE.get() {
        return true;
    }
    let ok = unsafe {
        GetModuleHandleW(None).is_ok_and(|module| {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: module.into(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassW(&wc) != 0
        })
    };
    DONE.set(ok);
    ok
}

/// Runs `f` on the overlay, made the first time. Nothing when its windows
/// cannot be made, since control goes on without its show.
fn with(shared: &Rc<Shared>, f: impl FnOnce(&mut Overlay)) {
    OVERLAY.with(|o| {
        let Ok(mut slot) = o.try_borrow_mut() else {
            return;
        };
        if slot.is_none() && registered() {
            let clock = unsafe {
                CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    CLASS,
                    w!(""),
                    WS_POPUP,
                    0,
                    0,
                    0,
                    0,
                    None,
                    None,
                    GetModuleHandleW(None).ok().map(Into::into),
                    None,
                )
            };
            match clock {
                Ok(clock) => {
                    let now = Instant::now();
                    *slot = Some(Overlay {
                        shared: Rc::clone(shared),
                        clock,
                        framing: false,
                        edges: Vec::new(),
                        plate: None,
                        work: [0, 0, 1280, 720],
                        scale: 1.0,
                        ghost: None,
                        ghost_at: None,
                        ghost_name: String::new(),
                        ghost_acted: now,
                        ghost_stopped: false,
                        ring: None,
                        glide: None,
                        queue: VecDeque::new(),
                        stop_key: false,
                        until: None,
                        began: now,
                        shown: 0.0,
                        ticked: now,
                        now: Now::default(),
                        history: VecDeque::new(),
                        stale: true,
                    })
                }
                Err(e) => eprintln!("horadric: cannot show desktop control: {e}"),
            }
        }
        if let Some(o) = slot.as_mut() {
            f(o);
        }
    });
}

/// An allowed call. With a point the ghost glides there and the call is
/// answered when it arrives, or refused if the human presses Esc; without
/// one it is answered at once. True when this call started control, so
/// the app can say so once.
pub(super) fn allowed(shared: &Rc<Shared>, call: Allowed, reply: Reply) -> bool {
    // Without an overlay the app's own word is all the human gets.
    let mut started = true;
    let mut reply = Some(reply);
    with(shared, |o| {
        let now = Instant::now();
        started = !o.active(now);
        if started {
            o.began = now;
        }
        o.until = extend(o.until, now, true);
        o.place_plate(call.hwnd, call.point);
        o.now = Now {
            name: call.name.clone(),
            program: call.program,
            reason: call.reason,
            what: call.what.clone(),
        };
        o.stale = true;
        // Taken only with a point: a call without one is answered below.
        if let Some((to, r)) = call.point.and_then(|to| Some((to, reply.take()?))) {
            o.queue.push_back(Queued {
                to,
                reply: r,
                name: call.name,
                what: call.what,
            });
            if o.glide.is_none() {
                o.next_glide(now);
            }
        }
        o.run();
    });
    // Without a point, or without an overlay to show the way.
    if let Some(r) = reply {
        r.send(json!({ "allowed": true }));
    }
    started
}

/// A finished action, from `desktop.done`.
pub(super) fn done(shared: &Rc<Shared>, what: &str, ok: bool, outcome: &str) {
    record(
        shared,
        Entry {
            at: now_clock(),
            mark: if ok { Mark::Ok } else { Mark::Failed },
            what: what.to_string(),
            outcome: outcome.to_string(),
        },
    );
}

/// A call the human refused.
pub(super) fn refused(shared: &Rc<Shared>, what: &str, why: &str) {
    record(
        shared,
        Entry {
            at: now_clock(),
            mark: Mark::Refused,
            what: what.to_string(),
            outcome: why.to_string(),
        },
    );
}

fn record(shared: &Rc<Shared>, e: Entry) {
    with(shared, |o| {
        let now = Instant::now();
        o.until = extend(o.until, now, false);
        remember(&mut o.history, e);
        o.stale = true;
        o.run();
    });
}

fn now_clock() -> String {
    let t = unsafe { GetLocalTime() };
    clock(t.wHour, t.wMinute, t.wSecond)
}

fn monitor_scale(monitor: HMONITOR) -> f32 {
    let (mut dx, mut dy) = (96u32, 96u32);
    unsafe {
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    }
    dx.max(96) as f32 / 96.0
}

fn monitor_rects(monitor: HMONITOR) -> Option<([i32; 4], [i32; 4])> {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetMonitorInfoW(monitor, &mut info) }
        .as_bool()
        .then(|| {
            let (m, w) = (info.rcMonitor, info.rcWork);
            (
                [m.left, m.top, m.right, m.bottom],
                [w.left, w.top, w.right, w.bottom],
            )
        })
}

fn monitors() -> Vec<HMONITOR> {
    unsafe extern "system" fn each(
        m: HMONITOR,
        _: HDC,
        _: *mut RECT,
        data: LPARAM,
    ) -> windows::core::BOOL {
        let list = &mut *(data.0 as *mut Vec<HMONITOR>);
        list.push(m);
        true.into()
    }
    let mut list: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(each),
            LPARAM(&mut list as *mut Vec<HMONITOR> as isize),
        );
    }
    list
}

impl Overlay {
    fn active(&self, now: Instant) -> bool {
        self.until.is_some_and(|u| u > now)
    }

    /// Starts the frames, if they are not running.
    fn run(&mut self) {
        if !self.framing {
            self.framing = true;
            self.ticked = Instant::now();
            crate::vsync::start(self.clock, FRAME);
        }
    }

    fn place_plate(&mut self, hwnd: isize, point: Option<(i32, i32)>) {
        let monitor = unsafe {
            if hwnd != 0 {
                MonitorFromWindow(HWND(hwnd as *mut _), MONITOR_DEFAULTTONEAREST)
            } else {
                let (x, y) = point.unwrap_or_default();
                MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST)
            }
        };
        if let Some((_, work)) = monitor_rects(monitor) {
            let scale = monitor_scale(monitor);
            if (scale - self.scale).abs() > f32::EPSILON {
                self.plate = None;
            }
            self.work = work;
            self.scale = scale;
        }
    }

    /// Makes the frame around every monitor, once a run of control begins,
    /// so a screen added since is framed too.
    fn frame_screens(&mut self) {
        if !self.edges.is_empty() {
            return;
        }
        for monitor in monitors() {
            let Some((rect, _)) = monitor_rects(monitor) else {
                continue;
            };
            let scale = monitor_scale(monitor);
            let thick = (EDGE * scale).round() as i32;
            for (side, [x, y, w, h]) in edges(rect, thick).into_iter().enumerate() {
                if w <= 0 || h <= 0 {
                    continue;
                }
                let Ok(mut layer) = Layer::new(&self.shared.gpu, (x, y), (w, h), scale) else {
                    continue;
                };
                let (dw, dh) = (w as f32 / scale, h as f32 / scale);
                // From the screen's edge inward: bright at the edge, gone
                // by the glow's depth.
                let (from, to) = match side {
                    0 => ((0.0, 0.0), (0.0, dh)),
                    1 => ((0.0, dh), (0.0, 0.0)),
                    2 => ((0.0, 0.0), (dw, 0.0)),
                    _ => ((dw, 0.0), (0.0, 0.0)),
                };
                let glow = theme::working();
                let painted = layer.paint(|p| unsafe {
                    p.fill_gradient(
                        d2rect(0.0, 0.0, dw, dh),
                        from,
                        to,
                        &[
                            (0.0, glow.with_alpha(0.95)),
                            (0.35, glow.with_alpha(0.5)),
                            (1.0, glow.with_alpha(0.0)),
                        ],
                    );
                });
                if painted.is_ok() {
                    layer.present((x, y), 0.0);
                    self.edges.push(layer);
                }
            }
        }
    }

    fn next_glide(&mut self, now: Instant) {
        while let Some(q) = self.queue.pop_front() {
            // Its agent gave up waiting: acting now would act twice.
            if !q.reply.wanted() {
                continue;
            }
            let from = self.ghost_at.unwrap_or_else(|| {
                let mut p = POINT::default();
                let _ = unsafe { GetCursorPos(&mut p) };
                (p.x as f32, p.y as f32)
            });
            self.ghost_at = Some(from);
            self.ghost_name = q.name;
            self.ghost_stopped = false;
            self.ghost_acted = now;
            self.glide = Some(Glide {
                from,
                to: (q.to.0 as f32, q.to.1 as f32),
                began: now,
                reply: q.reply,
                what: q.what,
            });
            self.listen_for_stop(true);
            self.paint_ghost(now);
            return;
        }
        self.listen_for_stop(false);
    }

    /// Esc is the human's only while a glide runs, and theirs again after.
    fn listen_for_stop(&mut self, on: bool) {
        if on == self.stop_key {
            return;
        }
        unsafe {
            if on {
                self.stop_key =
                    RegisterHotKey(Some(self.clock), STOP_KEY, MOD_NOREPEAT, VK_ESCAPE.0 as u32)
                        .is_ok();
            } else {
                let _ = UnregisterHotKey(Some(self.clock), STOP_KEY);
                self.stop_key = false;
            }
        }
    }

    /// Esc while gliding: the click is not made.
    fn stop(&mut self) {
        let now = Instant::now();
        let Some(g) = self.glide.take() else {
            return;
        };
        g.reply
            .send(json!({ "error": "the human stopped this action" }));
        self.ghost_stopped = true;
        self.ghost_acted = now;
        self.ring = Some((now, theme::error()));
        self.until = extend(self.until, now, false);
        remember(
            &mut self.history,
            Entry {
                at: now_clock(),
                mark: Mark::Stopped,
                what: g.what,
                outcome: "stopped by the human".into(),
            },
        );
        self.stale = true;
        // The rest waiting were asked before the stop and still go, each
        // with its own glide to stop.
        self.next_glide(now);
        self.run();
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let dt = now
            .duration_since(self.ticked)
            .min(Duration::from_millis(50));
        self.ticked = now;
        let active = self.active(now) || self.glide.is_some();
        if active {
            self.frame_screens();
            self.shown = (self.shown + dt.as_secs_f32() / FADE_IN.as_secs_f32()).min(1.0);
        } else {
            self.shown = (self.shown - dt.as_secs_f32() / FADE_OUT.as_secs_f32()).max(0.0);
        }

        if let Some(g) = &self.glide {
            let elapsed = now.duration_since(g.began);
            self.ghost_at = Some(glide_at(g.from, g.to, elapsed));
            self.stale = true;
            if elapsed >= GLIDE {
                if let Some(g) = self.glide.take() {
                    g.reply.send(json!({ "allowed": true }));
                }
                self.ghost_acted = now;
                self.ring = Some((now, theme::working()));
                self.next_glide(now);
            }
        }
        if self.ring.is_some_and(|(t, _)| now.duration_since(t) > RING) {
            self.ring = None;
        }
        self.paint_ghost(now);

        let alpha = self.shown * pulse(now.duration_since(self.began));
        for e in &mut self.edges {
            e.present(e.pos, alpha);
        }
        self.paint_plate(now);

        let ghost_seen = self.ghost.as_ref().is_some_and(|g| g.shown);
        if self.shown <= 0.0 && !active && !ghost_seen && self.ring.is_none() {
            self.rest();
        }
    }

    /// Nothing moves: the windows hide and the frames stop.
    fn rest(&mut self) {
        self.edges.clear();
        if let Some(p) = self.plate.as_mut() {
            p.hide();
        }
        self.ghost_stopped = false;
        self.framing = false;
        crate::vsync::stop(self.clock, FRAME);
    }

    fn paint_ghost(&mut self, now: Instant) {
        let Some(at) = self.ghost_at else {
            return;
        };
        // A stopped ghost stays where it was stopped while control runs.
        let alpha = if self.glide.is_some() || (self.ghost_stopped && self.active(now)) {
            1.0
        } else if self.ghost_stopped {
            self.shown
        } else {
            ghost_alpha(now.duration_since(self.ghost_acted))
        };
        if alpha <= 0.0 {
            if let Some(g) = self.ghost.as_mut() {
                g.hide();
            }
            // Its next glide starts from the real cursor again.
            self.ghost_at = None;
            return;
        }
        let monitor = unsafe {
            MonitorFromPoint(
                POINT {
                    x: at.0 as i32,
                    y: at.1 as i32,
                },
                MONITOR_DEFAULTTONEAREST,
            )
        };
        let scale = monitor_scale(monitor);
        let size = (
            (GHOST_SIZE.0 * scale).round() as i32,
            (GHOST_SIZE.1 * scale).round() as i32,
        );
        if self.ghost.as_ref().is_some_and(|g| g.scale != scale) {
            self.ghost = None;
        }
        if self.ghost.is_none() {
            match Layer::new(&self.shared.gpu, (at.0 as i32, at.1 as i32), size, scale) {
                Ok(l) => self.ghost = Some(l),
                Err(_) => return,
            }
        }
        let gpu = &self.shared.gpu;
        let ring = self
            .ring
            .map(|(t, c)| (motion::progress(now.duration_since(t), RING), c));
        let tint = if self.ghost_stopped {
            theme::error()
        } else {
            theme::working()
        };
        let name = self.ghost_name.as_str();
        let Some(layer) = self.ghost.as_mut() else {
            return;
        };
        let tip = GHOST_TIP;
        let _ = layer.paint(|p| unsafe {
            p.circle((tip, tip), 13.0, tint.with_alpha(0.22), None);
            if let Some((t, c)) = ring {
                let r = 6.0 + 26.0 * motion::ease_out(t);
                p.circle((tip, tip), r, c.with_alpha(1.0 - t), Some(2.5));
            }
            arrow(gpu, p, tip, tint);
            tag(gpu, p, (tip + 14.0, tip + 20.0), name, tint);
        });
        let off = (tip * scale).round() as i32;
        layer.present(
            (at.0.round() as i32 - off, at.1.round() as i32 - off),
            alpha,
        );
    }

    fn paint_plate(&mut self, now: Instant) {
        if self.shown <= 0.0 {
            if let Some(p) = self.plate.as_mut() {
                p.hide();
            }
            return;
        }
        let gliding = self
            .glide
            .as_ref()
            .map(|g| left_of_glide(now.duration_since(g.began)));
        let gpu = &self.shared.gpu;
        let content_w = BOX_W - 2.0 * PAD;
        let reason = (!self.now.reason.trim().is_empty())
            .then(|| render::wrapped(gpu, &gpu.small, self.now.reason.trim(), content_w).ok())
            .flatten();
        let reason_h = reason
            .as_ref()
            .map_or(0.0, |l| render::text_size(l).1.ceil());
        let h = box_height(reason_h, self.history.len());
        let s = self.scale;
        let size = ((BOX_W * s).round() as i32, (h * s).round() as i32);
        let margin = (BOX_MARGIN * s).round() as i32;
        let pos = box_place(self.work, size, margin);
        if self.plate.is_none() {
            match Layer::new(gpu, pos, size, s) {
                Ok(l) => self.plate = Some(l),
                Err(_) => return,
            }
            self.stale = true;
        }
        let Some(plate) = self.plate.as_mut() else {
            return;
        };
        if self.stale || plate.size != size {
            self.stale = false;
            if plate.resize(size).is_err() {
                return;
            }
            let now_line = &self.now;
            let history = &self.history;
            let metrics_radius = self.shared.metrics.window_radius;
            let _ = plate.paint(|p| unsafe {
                draw_plate(
                    gpu,
                    p,
                    PlateScene {
                        h,
                        radius: metrics_radius,
                        now: now_line,
                        reason: reason.as_ref(),
                        reason_h,
                        gliding,
                        history,
                    },
                );
            });
        }
        plate.present(pos, self.shown);
    }
}

/// The ghost's arrow, its tip at (`tip`, `tip`).
unsafe fn arrow(gpu: &render::Gpu, p: &Pen, tip: f32, tint: Color) {
    let Ok(path) = gpu.d2d.CreatePathGeometry() else {
        return;
    };
    let Ok(sink) = path.Open() else {
        return;
    };
    let k = 1.25;
    let pts = [
        (0.0, 17.0),
        (4.5, 13.0),
        (7.5, 20.0),
        (10.0, 19.0),
        (7.0, 12.5),
        (12.5, 12.5),
    ];
    let at = |(x, y): (f32, f32)| Vector2 {
        X: tip + x * k,
        Y: tip + y * k,
    };
    sink.BeginFigure(at((0.0, 0.0)), D2D1_FIGURE_BEGIN_FILLED);
    for pt in pts {
        sink.AddLine(at(pt));
    }
    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
    if sink.Close().is_err() {
        return;
    }
    p.brush.SetColor(&color(tint.with_alpha(0.75)));
    p.rt.FillGeometry(&path, p.brush, None);
    p.brush
        .SetColor(&color(Color::rgb(0xFFFFFF).with_alpha(0.95)));
    p.rt.DrawGeometry(&path, p.brush, 1.3, None);
}

/// The session's name on a pill by the arrow.
unsafe fn tag(gpu: &render::Gpu, p: &Pen, at: (f32, f32), name: &str, tint: Color) {
    if name.is_empty() {
        return;
    }
    let wide: Vec<u16> = name.encode_utf16().collect();
    let Ok(layout) = gpu.dw.CreateTextLayout(&wide, &gpu.chip, TAG_MAX, 18.0) else {
        return;
    };
    let w = render::text_size(&layout).0.min(TAG_MAX).ceil() + 14.0;
    let r = d2rect(at.0, at.1, w, 19.0);
    p.fill_rounded(r, 9.5, tint.with_alpha(0.92));
    p.text(
        &gpu.chip,
        Color::rgb(0x0B0C0E),
        name,
        d2rect(at.0 + 7.0, at.1, w - 14.0, 19.0),
    );
}

struct PlateScene<'a> {
    h: f32,
    radius: f32,
    now: &'a Now,
    reason: Option<&'a windows::Win32::Graphics::DirectWrite::IDWriteTextLayout>,
    reason_h: f32,
    /// How much of the glide is left, while one runs.
    gliding: Option<f32>,
    history: &'a VecDeque<Entry>,
}

/// The Thinking plate, drawn like the app's other plates: dark metal lit
/// from above, a seam cut round it, the lamp of what it is doing.
unsafe fn draw_plate(gpu: &render::Gpu, p: &Pen, s: PlateScene) {
    let (w, h) = (BOX_W, s.h);
    let radius = s.radius.max(6.0);
    let all = d2rect(0.0, 0.0, w, h);
    p.fill_rounded(all, radius, theme::plate_bottom());
    p.rt.PushAxisAlignedClip(&d2rect(0.0, 0.0, w, h), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    p.fill_gradient(
        d2rect(0.0, 0.0, w, h.min(120.0)),
        (0.0, 0.0),
        (0.0, h.min(120.0)),
        &[
            (0.0, theme::plate_top()),
            (1.0, theme::plate_bottom().with_alpha(0.0)),
        ],
    );
    p.rt.PopAxisAlignedClip();
    p.stroke_rounded(
        d2rect(0.5, 0.5, w - 1.0, h - 1.0),
        radius,
        theme::working().with_alpha(0.55),
        1.0,
    );
    let seam = d2rect(5.5, 5.5, w - 11.0, h - 11.0);
    let seam_r = (radius - 3.0).max(2.0);
    p.stroke_rounded(
        d2rect(5.5, 6.5, w - 11.0, h - 11.0),
        seam_r,
        theme::engrave_light(),
        1.0,
    );
    p.stroke_rounded(seam, seam_r, theme::engrave_dark(), 1.0);

    let x = PAD;
    let cw = w - 2.0 * PAD;
    // Title: the lamp, the name of the plate, and who is at work where.
    p.circle((x + 4.0, 24.0), 4.0, theme::working(), None);
    p.circle(
        (x + 4.0, 24.0),
        7.0,
        theme::working().with_alpha(0.25),
        None,
    );
    p.text(
        &gpu.title,
        theme::text(),
        "Thinking",
        d2rect(x + 16.0, 14.0, 80.0, 20.0),
    );
    let who = match (s.now.name.trim(), s.now.program.trim()) {
        ("", program) => program.to_string(),
        (name, "") => name.to_string(),
        (name, program) => format!("{name} in {program}"),
    };
    p.text(
        &gpu.small_right,
        theme::text_dim(),
        &who,
        d2rect(x + 100.0, 14.0, cw - 100.0, 20.0),
    );

    let mut y = 40.0;
    p.text(&gpu.chip, theme::legend(), "NOW", d2rect(x, y, 60.0, 16.0));
    if let Some(left) = s.gliding {
        p.text(
            &gpu.small_right,
            theme::waiting(),
            "Esc to stop",
            d2rect(x + 60.0, y, cw - 60.0, 16.0),
        );
        p.fill_rounded(d2rect(x, y + 18.0, cw, 2.0), 1.0, theme::well());
        p.fill_rounded(d2rect(x, y + 18.0, cw * left, 2.0), 1.0, theme::waiting());
    }
    y += 22.0;
    if let Some(reason) = s.reason {
        let shown = s.reason_h.min(REASON_LINES * REASON_LINE);
        p.rt.PushAxisAlignedClip(&d2rect(x, y, cw, shown), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        p.brush.SetColor(&color(theme::text()));
        p.rt.DrawTextLayout(
            Vector2 { X: x, Y: y },
            reason,
            p.brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
        );
        p.rt.PopAxisAlignedClip();
        y += shown + 4.0;
    }
    let what = match s.now.what.trim() {
        "" => "looking",
        what => what,
    };
    p.text(&gpu.small, theme::text_dim(), what, d2rect(x, y, cw, ROW));
    y += ROW;

    if s.history.is_empty() {
        return;
    }
    y += 6.0;
    p.fill_rounded(d2rect(x, y + 1.0, cw, 1.0), 0.0, theme::engrave_light());
    p.fill_rounded(d2rect(x, y, cw, 1.0), 0.0, theme::engrave_dark());
    y += 8.0;
    p.text(
        &gpu.chip,
        theme::legend(),
        "HISTORY",
        d2rect(x, y, 80.0, 16.0),
    );
    y += 22.0;
    for (i, e) in s.history.iter().enumerate() {
        // The newest reads plainly, the older ones step back.
        let ink = if i == 0 {
            theme::text()
        } else {
            theme::text_dim()
        };
        p.text(&gpu.small, theme::legend(), &e.at, d2rect(x, y, 60.0, ROW));
        let mut buf = [0u8; 4];
        p.text(
            &gpu.icon_small,
            e.mark.colour(),
            e.mark.glyph().encode_utf8(&mut buf),
            d2rect(x + 58.0, y, 16.0, ROW),
        );
        p.text(
            &gpu.small,
            ink,
            &entry_line(e),
            d2rect(x + 80.0, y, cw - 80.0, ROW),
        );
        y += ROW;
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_TIMER if wparam.0 == FRAME => {
            crate::vsync::took(hwnd, FRAME);
            OVERLAY.with(|o| {
                if let Ok(mut o) = o.try_borrow_mut() {
                    if let Some(o) = o.as_mut() {
                        o.tick();
                    }
                }
            });
            LRESULT(0)
        }
        WM_HOTKEY if wparam.0 as i32 == STOP_KEY => {
            OVERLAY.with(|o| {
                if let Ok(mut o) = o.try_borrow_mut() {
                    if let Some(o) = o.as_mut() {
                        o.stop();
                    }
                }
            });
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(n: usize) -> Entry {
        Entry {
            at: clock(9, 5, n as u16),
            mark: Mark::Ok,
            what: format!("click {n}"),
            outcome: String::new(),
        }
    }

    #[test]
    fn control_starts_with_an_allowed_call_and_lasts_after_the_last() {
        let t0 = Instant::now();
        assert_eq!(extend(None, t0, false), None);
        let until = extend(None, t0, true);
        assert_eq!(until, Some(t0 + ACTIVE_FOR));
        let later = t0 + Duration::from_secs(15);
        assert_eq!(extend(until, later, false), Some(later + ACTIVE_FOR));
        // Over: a refusal or a done does not bring it back.
        let after = t0 + Duration::from_secs(21);
        assert_eq!(extend(until, after, false), None);
        assert_eq!(extend(until, after, true), Some(after + ACTIVE_FOR));
    }

    #[test]
    fn the_ghost_glides_softly_from_start_to_point() {
        let (a, b) = ((0.0, 100.0), (200.0, -100.0));
        assert_eq!(glide_at(a, b, Duration::ZERO), a);
        assert_eq!(glide_at(a, b, GLIDE), b);
        assert_eq!(glide_at(a, b, GLIDE * 3), b);
        let half = glide_at(a, b, GLIDE / 2);
        assert!((half.0 - 100.0).abs() < 0.01 && half.1.abs() < 0.01);
        // Slow at the start: a tenth of the time is far less than a tenth
        // of the way.
        assert!(glide_at(a, b, GLIDE / 10).0 < 10.0);
    }

    #[test]
    fn the_glide_counts_down_its_bar() {
        assert_eq!(left_of_glide(Duration::ZERO), 1.0);
        assert!((left_of_glide(GLIDE / 4) - 0.75).abs() < 1e-4);
        assert_eq!(left_of_glide(GLIDE * 2), 0.0);
    }

    #[test]
    fn the_frame_breathes_between_faint_and_full() {
        assert!((pulse(Duration::ZERO) - 0.45).abs() < 1e-4);
        assert!((pulse(PULSE / 2) - 1.0).abs() < 1e-4);
        assert!((pulse(PULSE) - 0.45).abs() < 1e-4);
        for ms in (0..4000).step_by(37) {
            let a = pulse(Duration::from_millis(ms));
            assert!((0.45..=1.0001).contains(&a), "{ms}: {a}");
        }
    }

    #[test]
    fn the_ghost_stays_then_fades() {
        assert_eq!(ghost_alpha(Duration::ZERO), 1.0);
        assert_eq!(ghost_alpha(GHOST_STAYS), 1.0);
        assert!((ghost_alpha(GHOST_STAYS + GHOST_FADE / 2) - 0.5).abs() < 1e-4);
        assert_eq!(ghost_alpha(GHOST_STAYS + GHOST_FADE), 0.0);
    }

    #[test]
    fn four_strips_frame_a_monitor_without_overlapping() {
        let m = [-2560, 0, 0, 1440];
        assert_eq!(
            edges(m, 10),
            [
                [-2560, 0, 2560, 10],
                [-2560, 1430, 2560, 10],
                [-2560, 10, 10, 1420],
                [-10, 10, 10, 1420],
            ]
        );
        // A monitor too small for the depth keeps strips that fit.
        let tiny = edges([0, 0, 6, 6], 10);
        assert!(tiny.iter().all(|r| r[2] >= 0 && r[3] >= 0));
    }

    #[test]
    fn the_plate_sits_in_the_top_right_of_the_work_area() {
        assert_eq!(box_place([0, 0, 1920, 1040], (380, 200), 18), (1522, 18));
        assert_eq!(box_place([-2560, 40, 0, 1440], (380, 200), 18), (-398, 58));
        // Narrower than the plate: it keeps its left edge on the screen.
        assert_eq!(box_place([100, 0, 300, 600], (380, 200), 18), (100, 18));
    }

    #[test]
    fn the_plate_grows_with_its_reason_and_history() {
        let bare = box_height(0.0, 0);
        assert!(box_height(17.0, 0) > bare);
        assert_eq!(
            box_height(500.0, 0),
            box_height(REASON_LINES * REASON_LINE, 0)
        );
        let one = box_height(0.0, 1);
        assert!(one > bare);
        assert_eq!(box_height(0.0, 3) - one, 2.0 * ROW);
    }

    #[test]
    fn history_keeps_the_newest_on_top() {
        let mut h = VecDeque::new();
        for n in 0..HISTORY + 3 {
            remember(&mut h, entry(n));
        }
        assert_eq!(h.len(), HISTORY);
        assert_eq!(h.front(), Some(&entry(HISTORY + 2)));
        assert_eq!(h.back(), Some(&entry(3)));
    }

    #[test]
    fn history_lines_read_what_then_how_it_went() {
        let mut e = entry(1);
        assert_eq!(entry_line(&e), "click 1");
        e.outcome = "dialog opened".into();
        assert_eq!(entry_line(&e), "click 1: dialog opened");
        e.what = " ".into();
        assert_eq!(entry_line(&e), "dialog opened");
        assert_eq!(clock(7, 3, 9), "07:03:09");
    }

    #[test]
    fn each_mark_has_its_own_glyph() {
        let marks = [Mark::Ok, Mark::Failed, Mark::Refused, Mark::Stopped];
        let glyphs: std::collections::HashSet<char> = marks.iter().map(|m| m.glyph()).collect();
        assert_eq!(glyphs.len(), marks.len());
    }
}
