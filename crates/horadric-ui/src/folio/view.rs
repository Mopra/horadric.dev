//! The Folio's window over a pane's glass: its graphics, the paint loop,
//! input from the pane and the mouse, and what a click hits.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::time::{Duration, Instant};

use windows::core::{w, Interface, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1BitmapRenderTarget, ID2D1HwndRenderTarget, ID2D1LinearGradientBrush, ID2D1RenderTarget,
    ID2D1SolidColorBrush, ID2D1StrokeStyle, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_CAP_STYLE_ROUND,
    D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE, D2D1_DASH_STYLE_SOLID, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_ELLIPSE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_LINE_JOIN_ROUND, D2D1_ROUNDED_RECT,
    D2D1_STROKE_STYLE_PROPERTIES,
};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteTextFormat, IDWriteTextLayout, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_ITALIC,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_LINE_SPACING_METHOD_UNIFORM,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Gdi::{
    CreateRoundRectRgn, GetUpdateRect, InvalidateRect, ScreenToClient, SetWindowRgn, ValidateRect,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_NEXT, VK_PRIOR, VK_SHIFT,
};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetAncestor, GetClientRect, GetCursorPos, GetParent,
    GetWindowLongPtrW, IsIconic, IsWindowVisible, KillTimer, LoadCursorW, RegisterClassW,
    SetCursor, SetTimer, SetWindowLongPtrW, SetWindowPos, CREATESTRUCTW, CS_DBLCLKS, GA_ROOT,
    GWLP_USERDATA, HTCLIENT, IDC_ARROW, IDC_HAND, IDC_IBEAM, SWP_NOACTIVATE, SWP_NOZORDER,
    WINDOW_EX_STYLE, WM_CAPTURECHANGED, WM_DESTROY, WM_DROPFILES, WM_ERASEBKGND, WM_LBUTTONDBLCLK,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SETCURSOR,
    WM_SIZE, WM_TIMER, WNDCLASSW, WS_CHILD, WS_CLIPSIBLINGS, WS_VISIBLE,
};
use windows_numerics::Vector2;

use super::composer::{self, Act, STRIP_H};
use super::banish::{self, Ghost};
use super::lightbox;
use super::{answer, interrupt, redraw, select, transcript, with, Chat, CHATS};
use crate::app::{self, Input};
use crate::clipboard;
use crate::folio_look as look;
use crate::keys::Mods;
use crate::layout::Rect;
use crate::motion;
use crate::render::{self, color, Gpu};
use crate::sparks::RUNES;
use crate::theme::{self, Color};
use crate::window::Shared;

const CLASS: PCWSTR = w!("HoradricFolio");
const TICK: usize = 1;
/// Turns the caret on or off, at the system's blink rate.
const BLINK: usize = 2;
/// Lays the conversation out again once its width stops changing. See
/// [`transcript`](super::transcript) for why it waits.
pub(super) const SETTLE: usize = 3;
/// The caret in every field that can hold the keys.
pub(super) const CARET_W: f32 = 1.5;
/// Between frames while the background moves. It drifts slowly, so this is
/// smooth enough and costs a third of the display's rate.
const FRAME_MS: u32 = 40;
/// Between frames while a cleared page burns away, which moves fast.
const BANISH_MS: u32 = 16;
/// The glass's corner, less the line of its rim the conversation leaves.
const CORNER: f32 = 7.0;

pub(super) const FONT: PCWSTR = w!("Segoe UI Variable Text");
/// The size prose is read in, in the conversation and the composer alike.
/// On screens, text a little above the usual 16 reads faster and is
/// understood better.
pub(super) const READ: f32 = 17.0;
/// How far the words' white is softened toward the background. Near white
/// on near black glows at the strokes' edges, worst with astigmatism; a
/// touch off white keeps the contrast well past what reading needs.
const SOFTEN: f32 = 0.09;
/// Prose lines this many times their size apart. DirectWrite's own spacing
/// is set for interfaces and packs a long answer too tight to follow.
const LEADING: f32 = 1.6;
const MONO: PCWSTR = w!("Cascadia Mono");
const ICONS: PCWSTR = w!("Segoe Fluent Icons");

/// Not in the windows crate's WindowsAndMessaging.
const WM_MOUSELEAVE: u32 = 0x02A3;

/// A wheel notch, in DIPs.
const NOTCH: f32 = 54.0;
/// The scroll bar's reach at the glass's right edge, wider than the thumb
/// drawn in it so it is easy to grab, and what it keeps clear of the
/// glass's top and bottom.
const BAR_W: f32 = 14.0;
const BAR_INSET: f32 = 8.0;

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
/// `parent`, and its settings into the pane's header row. They are placed
/// by [`place`] and [`place_bar`] and go with the pane.
pub fn attach(key: &str, shared: &Rc<Shared>, parent: HWND) {
    if let Some(hwnd) = make(key, shared, parent, false) {
        // Files dropped on the conversation go into the composer.
        unsafe { DragAcceptFiles(hwnd, true) };
        with(key, |c| c.view = Some(hwnd));
    }
    if let Some(hwnd) = make(key, shared, parent, true) {
        with(key, |c| c.bar = Some(hwnd));
    }
}

fn make(key: &str, shared: &Rc<Shared>, parent: HWND, bar: bool) -> Option<HWND> {
    let view = Box::new(View {
        bar,
        hwnd: Cell::new(HWND::default()),
        key: key.to_string(),
        shared: Rc::clone(shared),
        gfx: RefCell::new(None),
        hits: RefCell::new(Vec::new()),
        composer: RefCell::new(None),
        ticking: Cell::new(0),
        banished: Cell::new(None),
        caret_since: Cell::new(Instant::now()),
        caret_was: Cell::new(0),
        last: Cell::new(None),
        clock: Cell::new(0.0),
        warp: Cell::new(0.0),
        selecting: Cell::new(false),
        track: Cell::new(None),
        grab: Cell::new(None),
        bar_lit: Cell::new(false),
        dots: Cell::new(None),
    });
    let hwnd = unsafe {
        let Ok(instance) = GetModuleHandleW(None) else {
            return None;
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
    hwnd.ok()
}

/// Puts the settings at `r` in the pane's header row, in the pane's
/// pixels. `shift` is DIPs from its left to the glass's, and `plate` the
/// pane's place on the stage, so the plate's light runs on across it.
pub fn place_bar(key: &str, r: RECT, shift: f32, plate: (f32, f32)) {
    let Some((Some(bar), moved)) = with(key, |c| {
        let moved = c.bar_shift != shift || c.plate != plate;
        c.bar_shift = shift;
        c.plate = plate;
        (c.bar, moved)
    }) else {
        return;
    };
    unsafe {
        let mut now = RECT::default();
        let _ = GetClientRect(bar, &mut now);
        let mut at = POINT::default();
        let _ = windows::Win32::Graphics::Gdi::MapWindowPoints(
            Some(bar),
            GetParent(bar).ok(),
            std::slice::from_mut(&mut at),
        );
        let (w, h) = ((r.right - r.left).max(0), (r.bottom - r.top).max(0));
        if (at.x, at.y, now.right, now.bottom) != (r.left, r.top, w, h) {
            let _ = SetWindowPos(
                bar,
                None,
                r.left,
                r.top,
                w,
                h,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        } else if moved {
            let _ = InvalidateRect(Some(bar), None, false);
        }
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

pub(super) fn dpi(hwnd: HWND) -> u32 {
    unsafe { GetDpiForWindow(hwnd) }.max(96)
}

/// The pane gained or lost the keyboard. The conversation is a child
/// window the pane's own repaint does not reach, and unless Claude is
/// working nothing else would redraw it, so its caret and rims would
/// keep showing the keyboard where it no longer is.
pub fn keyboard_moved(key: &str) {
    super::redraw(key);
}

/// `/clear` emptied the conversation of session `key`: the page as last
/// drawn burns away over the empty one. Called before the repaint that
/// draws the empty page, while the kept layer still holds the old.
pub(super) fn banish(key: &str) {
    let Some(view) = view_of(key) else {
        return;
    };
    let Some(bottom) = with(key, |c| c.tr.state.area.bottom()) else {
        return;
    };
    let Ok(mut slot) = view.gfx.try_borrow_mut() else {
        return;
    };
    let Some(gfx) = slot.as_mut() else {
        return;
    };
    unsafe {
        let Some(page) = gfx.kept().and_then(|l| l.GetBitmap().ok()) else {
            return;
        };
        let w = gfx.win.GetSize().width;
        // The next frame draws the empty page into a layer of its own.
        gfx.layer = None;
        gfx.ghost = Some(Ghost {
            page,
            area: Rect::new(0.0, 0.0, w, bottom),
            since: Instant::now(),
        });
    }
    view.banished.set(Some(Instant::now()));
    view.clock(true);
}

/// A key that makes a character, from the pane: the composer's.
pub fn char(key: &str, c: char, mods: Mods) {
    if select::char(key, c, mods) || super::is_watch(key) {
        return;
    }
    composer::on_char(key, c, mods);
}

/// A key that makes no character, from the pane. False leaves it to the
/// pane.
pub fn key(key: &str, vk: u16, mods: Mods) -> bool {
    // Before the composer, whose Esc would stop Claude.
    if lightbox::key(key, vk) {
        return true;
    }
    if select::key(key, vk, mods) {
        return true;
    }
    let page = with(key, |c| c.view_h * 0.85).unwrap_or(0.0);
    let done = match vk {
        v if v == VK_PRIOR.0 => {
            scroll_by(key, -page);
            true
        }
        v if v == VK_NEXT.0 => {
            scroll_by(key, page);
            true
        }
        // A hireling's conversation has no composer to type into.
        _ if super::is_watch(key) => false,
        _ => return composer::on_key(key, vk, mods),
    };
    if done {
        redraw(key);
    }
    done
}

/// The wheel, from the pane, in Windows' units of 120 a notch.
pub fn wheel(key: &str, delta: i32) {
    if lightbox::wheel(key, delta) {
        return;
    }
    // Over an open menu the wheel scrolls the menu, not the conversation.
    let on_menu = with(key, |c| {
        let (Some(at), Some(view)) = (c.input.menu_at, c.view) else {
            return false;
        };
        let mut p = POINT::default();
        let s = dpi(view) as f32 / 96.0;
        let inside = unsafe {
            GetCursorPos(&mut p).is_ok()
                && ScreenToClient(view, &mut p).as_bool()
                && at.contains(p.x as f32 / s, p.y as f32 / s)
        };
        if inside {
            let rows = -(delta as f32 / 120.0 * 3.0).round() as isize;
            c.input.scroll_menu(rows);
        }
        inside
    })
    .unwrap_or(false);
    if !on_menu {
        scroll_by(key, -delta as f32 / 120.0 * NOTCH);
    }
    redraw(key);
}

pub(super) fn scroll_by(key: &str, by: f32) {
    with(key, |c| {
        c.scroll = look::clamp_scroll(c.scroll + by, c.content_h, c.view_h);
        c.follow = look::at_bottom(c.scroll, c.content_h, c.view_h);
    });
}

/// What a click on part of the conversation does.
#[derive(Clone)]
pub(super) enum Hit {
    Answer(String, bool),
    Fold(usize),
    /// The composer and its controls.
    Input(Act),
    /// Trusts the folder and starts the session, or not and closes it.
    Trust(bool),
    /// The transcript's own buttons and links.
    Tr(transcript::Act),
}

pub(super) struct Gfx {
    pub(super) win: ID2D1HwndRenderTarget,
    /// What the helpers draw on: the kept layer while the conversation is
    /// drawn, the window while a frame is put together.
    pub(super) rt: ID2D1RenderTarget,
    /// The conversation as last drawn (see [`View::draw`]), with the
    /// pixel size and DPI it was made for.
    layer: Option<(ID2D1BitmapRenderTarget, (u32, u32), u32)>,
    /// The page before a `/clear`, burning away over the new one.
    ghost: Option<Ghost>,
    /// The window's DPI the target draws at. Moving to another monitor
    /// changes the window's, and the target is told.
    pub(super) dpi: u32,
    pub(super) brush: ID2D1SolidColorBrush,
    pub(super) shade: ID2D1LinearGradientBrush,
    pub(super) round: ID2D1StrokeStyle,
    pub(super) accent: Color,
    pub(super) text: IDWriteTextFormat,
    pub(super) line: IDWriteTextFormat,
    pub(super) italic: IDWriteTextFormat,
    pub(super) small: IDWriteTextFormat,
    pub(super) title: IDWriteTextFormat,
    pub(super) mono: IDWriteTextFormat,
    pub(super) button: IDWriteTextFormat,
    pub(super) icon: IDWriteTextFormat,
    pub(super) big: IDWriteTextFormat,
}

pub(super) struct View {
    pub(super) hwnd: Cell<HWND>,
    /// The settings in the header row, not the conversation.
    pub(super) bar: bool,
    pub(super) key: String,
    pub(super) shared: Rc<Shared>,
    pub(super) gfx: RefCell<Option<Gfx>>,
    pub(super) hits: RefCell<Vec<(Rect, Hit)>>,
    /// The composer's text laid out and where it was drawn, in DIPs, for
    /// placing the caret by a click or an arrow.
    pub(super) composer: RefCell<Option<(IDWriteTextLayout, f32, f32)>>,
    /// The frame clock's interval in ms, 0 while it is stopped.
    pub(super) ticking: Cell<u32>,
    /// When the page was last cleared, for the clock to run fast while
    /// it burns.
    pub(super) banished: Cell<Option<Instant>>,
    /// When the caret last moved or the keyboard came, which restarts its
    /// blink lit, and what it was then.
    pub(super) caret_since: Cell<Instant>,
    pub(super) caret_was: Cell<u64>,
    pub(super) last: Cell<Option<Instant>>,
    /// The background's own time, in seconds. It runs only while it moves,
    /// so warming again picks up where it stopped.
    pub(super) clock: Cell<f32>,
    /// The clock's seconds each weighed by the heat, which carries the
    /// runes on faster while Claude works without any of them jumping.
    pub(super) warp: Cell<f32>,
    pub(super) selecting: Cell<bool>,
    /// Where the scroll bar was drawn, None when everything fits.
    pub(super) track: Cell<Option<Rect>>,
    /// How far into the thumb it was grabbed, while it is dragged.
    pub(super) grab: Cell<Option<f32>>,
    /// The mouse is over the scroll bar.
    pub(super) bar_lit: Cell<bool>,
    /// Where the dots that pulse while Claude works go, and the area they
    /// are clipped to: they move on every frame, so they are not in the
    /// kept layer.
    pub(super) dots: Cell<Option<(Vector2, Rect)>>,
}

pub(super) fn view_of(key: &str) -> Option<&'static View> {
    let hwnd = with(key, |c| c.view)??;
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const View;
        // The window owns its View until WM_NCDESTROY, and every caller is
        // on the UI thread, done with it before any message comes.
        (!ptr.is_null()).then(|| &*ptr)
    }
}

pub(super) unsafe fn text_format(
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

/// A wrapping format for prose at `size`, its lines spread by [`LEADING`]
/// with the baseline where the font's own spacing would put it, centred.
pub(super) unsafe fn reading(
    gpu: &Gpu,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
) -> Result<IDWriteTextFormat> {
    let f = text_format(gpu, FONT, size, weight, false, false)?;
    // Segoe UI's ascent and descent, as fractions of its size.
    let (ascent, descent) = (1.079, 0.251);
    let line = (size * LEADING).round();
    let baseline = (line - size * (ascent + descent)) / 2.0 + size * ascent;
    f.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, line, baseline)?;
    Ok(f)
}

pub(super) fn d2d(r: &Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.x,
        top: r.y,
        right: r.right(),
        bottom: r.bottom(),
    }
}

pub(super) fn rounded(r: &Rect, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT {
        rect: d2d(r),
        radiusX: radius,
        radiusY: radius,
    }
}

impl Gfx {
    pub(super) unsafe fn new(gpu: &Gpu, hwnd: HWND, key: &str) -> Result<Gfx> {
        let mut r = RECT::default();
        let _ = GetClientRect(hwnd, &mut r);
        let dpi = dpi(hwnd);
        let win = render::hwnd_target(gpu, hwnd, r.right as u32, r.bottom as u32, dpi, false)?;
        let rt: ID2D1RenderTarget = win.cast()?;
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
            win,
            rt,
            layer: None,
            ghost: None,
            dpi,
            brush,
            shade,
            round,
            accent,
            text: reading(gpu, READ, normal)?,
            line: text_format(gpu, FONT, 14.0, normal, false, true)?,
            italic: text_format(gpu, FONT, 14.0, normal, true, false)?,
            small: text_format(gpu, FONT, 12.5, normal, false, true)?,
            title: text_format(gpu, FONT, 14.5, semi, false, true)?,
            mono: text_format(gpu, MONO, 13.0, normal, false, false)?,
            button,
            icon,
            big,
        })
    }

    /// The layer the conversation is drawn into, made again when the
    /// window's size or DPI changed.
    unsafe fn layer(&mut self, gpu: &Gpu) -> Result<ID2D1BitmapRenderTarget> {
        if let Some(l) = self.kept() {
            return Ok(l.clone());
        }
        let px = self.win.GetPixelSize();
        let format = D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        };
        let l = self.win.CreateCompatibleRenderTarget(
            None,
            None,
            Some(&format),
            D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE,
        )?;
        l.SetTextRenderingParams(&gpu.text_params);
        // ClearType needs an opaque target, and the layer is see-through
        // so the runes show behind the conversation.
        l.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        self.layer = Some((l.clone(), (px.width, px.height), self.dpi));
        Ok(l)
    }

    /// The kept layer, if it still fits the window.
    unsafe fn kept(&self) -> Option<&ID2D1BitmapRenderTarget> {
        let px = self.win.GetPixelSize();
        let (l, size, dpi) = self.layer.as_ref()?;
        (*size == (px.width, px.height) && *dpi == self.dpi).then_some(l)
    }

    pub(super) unsafe fn fill(&self, r: &Rect, c: Color) {
        self.brush.SetColor(&color(c));
        self.rt.FillRectangle(&d2d(r), &self.brush);
    }

    pub(super) unsafe fn fill_round(&self, r: &Rect, radius: f32, c: Color) {
        self.brush.SetColor(&color(c));
        self.rt
            .FillRoundedRectangle(&rounded(r, radius), &self.brush);
    }

    pub(super) unsafe fn stroke_round(&self, r: &Rect, radius: f32, c: Color, width: f32) {
        self.brush.SetColor(&color(c));
        let r = Rect::new(r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0);
        self.rt
            .DrawRoundedRectangle(&rounded(&r, radius), &self.brush, width, None);
    }

    pub(super) unsafe fn text_in(&self, s: &str, f: &IDWriteTextFormat, r: &Rect, c: Color) {
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

    pub(super) unsafe fn layout_at(&self, l: &IDWriteTextLayout, x: f32, y: f32, c: Color) {
        self.brush.SetColor(&color(c));
        self.rt.DrawTextLayout(
            Vector2 { X: x, Y: y },
            l,
            &self.brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
    }

    /// A rune of the rim's, `size` tall with its top at `y`, centred on `x`.
    pub(super) unsafe fn rune(
        &self,
        which: usize,
        x: f32,
        y: f32,
        size: f32,
        c: Color,
        width: f32,
    ) {
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
pub(super) struct Palette {
    pub(super) bg: Color,
    pub(super) fg: Color,
    pub(super) dim: Color,
    pub(super) faint: Color,
    pub(super) accent: Color,
}

impl Palette {
    pub(super) fn caret(&self) -> Color {
        self.accent.mix(self.fg, 0.4)
    }
}

impl View {
    pub(super) fn hwnd(&self) -> HWND {
        self.hwnd.get()
    }

    pub(super) fn scale(&self) -> f32 {
        dpi(self.hwnd()) as f32 / 96.0
    }

    pub(super) fn dip(&self, lparam: LPARAM) -> (f32, f32) {
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32;
        (x / s, y / s)
    }

    /// Shown, and on a stage that is not minimized: worth drawing frames
    /// for.
    pub(super) fn seen(&self) -> bool {
        unsafe {
            IsWindowVisible(self.hwnd()).as_bool()
                && !IsIconic(GetAncestor(self.hwnd(), GA_ROOT)).as_bool()
        }
    }

    /// Whether the caret shows this frame. It blinks as a terminal pane's
    /// does, lit again whenever it moves or the keyboard comes back, and
    /// only while the pane has the keys.
    fn blink(&self, chat: &Chat, now: Instant) -> bool {
        use std::hash::{Hash, Hasher};
        let st = &chat.tr.state;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        st.keys.hash(&mut h);
        st.focus.hash(&mut h);
        let f = &chat.input.ed.field;
        (f.caret, f.text.len()).hash(&mut h);
        if let Some(f) = st.focus.as_deref().and_then(|id| st.fields.get(id)) {
            (f.caret, f.text.len()).hash(&mut h);
        }
        if let Some(f) = &st.find {
            (f.field.caret, f.field.text.len()).hash(&mut h);
        }
        if self.caret_was.replace(h.finish()) != h.finish() {
            self.caret_since.set(now);
        }
        // INFINITE when blinking is off in Settings, zero on failure.
        let half = match unsafe { windows::Win32::UI::WindowsAndMessaging::GetCaretBlinkTime() } {
            u32::MAX => Duration::ZERO,
            ms => Duration::from_millis(ms as u64),
        };
        let elapsed = now.duration_since(self.caret_since.get());
        let next = motion::caret_turns(elapsed, half).filter(|_| st.keys);
        unsafe {
            match next {
                Some(wait) => {
                    SetTimer(Some(self.hwnd()), BLINK, wait.as_millis() as u32 + 1, None);
                }
                None => {
                    let _ = KillTimer(Some(self.hwnd()), BLINK);
                }
            }
        }
        st.keys && motion::caret_lit(elapsed, half)
    }

    /// Runs the frame clock while the background moves and it is seen,
    /// and stops it otherwise.
    pub(super) fn clock(&self, moving: bool) {
        let on = moving && self.seen();
        let fast = self.burning() || lightbox::moving(&self.key);
        let rate = match (on, fast) {
            (false, _) => 0,
            (true, true) => BANISH_MS,
            (true, false) => FRAME_MS,
        };
        if self.ticking.replace(rate) != rate {
            unsafe {
                if rate > 0 {
                    SetTimer(Some(self.hwnd()), TICK, rate, None);
                } else {
                    let _ = KillTimer(Some(self.hwnd()), TICK);
                }
            }
        }
        if !on {
            self.last.set(None);
        }
    }

    pub(super) fn paint(&self) {
        let hwnd = self.hwnd();
        let mut slot = self.gfx.borrow_mut();
        // The session's id is the key here; its colour is its project's.
        let project = with(&self.key, |c| c.key.clone()).unwrap_or_default();
        let accent = theme::accent(&project);
        if slot.as_ref().is_some_and(|g| g.accent != accent) {
            *slot = None;
        }
        if slot.is_none() {
            match unsafe { Gfx::new(&self.shared.gpu, hwnd, &project) } {
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
            unsafe { gfx.win.SetDpi(now as f32, now as f32) };
            gfx.dpi = now;
        }
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

    /// A frame for a tick of the runes' clock: the kept layer over runes
    /// moved on, nothing of the conversation drawn again. False when it
    /// cannot be, and the window has to be painted.
    fn tick(&self) -> bool {
        unsafe {
            // A paint is owed anyway, and draws the runes with the rest.
            if GetUpdateRect(self.hwnd(), None, false).as_bool() {
                return true;
            }
        }
        let Ok(mut slot) = self.gfx.try_borrow_mut() else {
            return false;
        };
        let Some(gfx) = slot.as_mut() else {
            return false;
        };
        if unsafe { gfx.kept() }.is_none() {
            return false;
        }
        // None when the layer no longer shows the conversation.
        let ok = CHATS.with(|c| {
            let Ok(mut chats) = c.try_borrow_mut() else {
                return Some(true);
            };
            let Some(chat) = chats.get_mut(&self.key) else {
                return Some(true);
            };
            // Some changes reach the model without a redraw (an interrupt
            // that empties the queue), which a full paint on every tick
            // used to hide.
            if self.dots.get().is_some() != chat.folio.busy {
                return None;
            }
            let (t, heat) = self.advance(chat, Instant::now());
            Some(unsafe { self.frame(gfx, t, heat) })
        });
        match ok {
            None => false,
            Some(true) => true,
            Some(false) => {
                *slot = None;
                false
            }
        }
    }

    /// A cleared page is still burning away.
    fn burning(&self) -> bool {
        (self.banished.get()).is_some_and(|t| t.elapsed().as_secs_f32() < banish::LENGTH)
    }

    /// Moves the runes' clocks on while they move, and starts or stops the
    /// frame clock. The clock and the heat.
    fn advance(&self, chat: &mut Chat, now: Instant) -> (f32, f32) {
        chat.note_busy();
        let heat = chat.heat();
        let moving =
            chat.busy_was || heat > 0.0 || self.burning() || lightbox::moving(&self.key);
        if moving {
            let dt = self
                .last
                .replace(Some(now))
                .map_or(0.0, |t| now.duration_since(t).as_secs_f32().min(0.1));
            self.clock.set(self.clock.get() + dt);
            self.warp.set(self.warp.get() + dt * heat);
        }
        self.clock(moving);
        (self.clock.get(), heat)
    }

    fn palette(gfx: &Gfx) -> Palette {
        let p = theme::palette();
        let fg = p.term_fg.mix(p.term_bg, SOFTEN);
        Palette {
            bg: p.term_bg,
            fg,
            dim: fg.with_alpha(0.7),
            faint: fg.with_alpha(0.5),
            accent: gfx.accent,
        }
    }

    /// Draws one frame. False when the target was lost.
    ///
    /// The conversation is drawn into a layer kept between frames, and the
    /// window gets the runes with the layer over them. A tick of the runes'
    /// clock only puts those together again ([`View::tick`]): while Claude
    /// works the runes move 25 times a second, and drawing every block in
    /// view that often took a fifth of the UI thread.
    pub(super) unsafe fn draw(&self, gfx: &mut Gfx, chat: &mut Chat) -> bool {
        if self.bar {
            return self.draw_bar(gfx, chat);
        }
        let now = Instant::now();
        chat.tr.state.keys = self.has_keyboard();
        chat.tr.state.caret_lit = self.blink(chat, now);
        let (t, heat) = self.advance(chat, now);
        let Some(layer) = gfx.layer(&self.shared.gpu).ok().and_then(|l| l.cast().ok()) else {
            return false;
        };
        gfx.rt = layer;
        self.still(gfx, chat) && self.frame(gfx, t, heat)
    }

    /// The conversation and the composer, into the kept layer.
    unsafe fn still(&self, gfx: &Gfx, chat: &mut Chat) -> bool {
        let rt = &gfx.rt;
        let size = rt.GetSize();
        let (w, h) = (size.width, size.height);
        let pal = Self::palette(gfx);
        let (cx, cw) = look::column(w);
        let mut hits: Vec<(Rect, Hit)> = Vec::new();
        self.dots.set(None);

        rt.BeginDraw();
        rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        rt.Clear(Some(&color(pal.bg.with_alpha(0.0))));

        // The composer first, since the conversation ends above it.
        // Its hits go last, so they win over a block's under the composer.
        let mut composer_hits: Vec<(Rect, Hit)> = Vec::new();
        let composer = if chat.watch.is_some() {
            Rect::new(cx, h, cw, 0.0)
        } else {
            self.composer(gfx, chat, &pal, cx, cw, h, &mut composer_hits)
        };
        let area = Rect::new(0.0, 0.0, w, (composer.y - 10.0).max(0.0));
        self.transcript(gfx, &pal, chat, cx, cw, &area, &mut hits);
        // What scrolls up fades out into the glass's top edge, not cut.
        if chat.scroll > 0.0 {
            const FADE: f32 = 24.0;
            for i in 0..6 {
                let band = Rect::new(0.0, FADE * i as f32 / 6.0, w, FADE / 6.0);
                gfx.fill(&band, pal.bg.with_alpha(1.0 - (i as f32 + 0.5) / 6.0));
            }
        }

        // The glass is sunk into the plate, so its top edge is in shade.
        gfx.shade.SetStartPoint(Vector2 { X: 0.0, Y: 0.0 });
        gfx.shade.SetEndPoint(Vector2 { X: 0.0, Y: 9.0 });
        rt.FillRectangle(&d2d(&Rect::new(0.0, 0.0, w, 9.0)), &gfx.shade);
        self.scroll_bar(gfx, &pal, chat);
        if chat.pending.is_some() {
            self.trust_card(gfx, &pal, chat, cx, cw, &area, &mut composer_hits);
        }
        if chat.watch.is_none() {
            self.composer_menu(gfx, chat, &pal, cx, cw, &mut composer_hits);
        }

        hits.extend(composer_hits);
        *self.hits.borrow_mut() = hits;
        rt.EndDraw(None, None).is_ok()
    }

    /// The window: the runes at clock `t`, the kept layer over them, and
    /// the dots that pulse while Claude works.
    unsafe fn frame(&self, gfx: &mut Gfx, t: f32, heat: f32) -> bool {
        let Some(still) = gfx.kept().and_then(|l| l.GetBitmap().ok()) else {
            return false;
        };
        if gfx.ghost.as_ref().is_some_and(Ghost::over) {
            gfx.ghost = None;
        }
        let Ok(win) = gfx.win.cast() else {
            return false;
        };
        gfx.rt = win;
        let gfx = &*gfx;
        let rt = &gfx.rt;
        let size = rt.GetSize();
        let pal = Self::palette(gfx);
        rt.BeginDraw();
        rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        rt.Clear(Some(&color(pal.bg)));
        let view = (size.width, size.height);
        self.background(gfx, &pal, view, (t, self.warp.get()), heat);
        match &gfx.ghost {
            Some(g) => banish::draw(gfx, &pal, &still, g),
            None => rt.DrawBitmap(
                &still,
                None,
                1.0,
                D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                None,
            ),
        }
        if let Some((at, area)) = self.dots.get() {
            rt.PushAxisAlignedClip(&d2d(&area), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            for k in 0..3 {
                let pulse = ((t * 4.0 - k as f32 * 0.7).sin() * 0.5 + 0.5).powf(1.5);
                gfx.brush.SetColor(&color(
                    pal.accent.mix(pal.fg, 0.3).with_alpha(0.25 + 0.75 * pulse),
                ));
                let point = Vector2 {
                    X: at.X + k as f32 * 12.0,
                    Y: at.Y,
                };
                let dot = D2D1_ELLIPSE {
                    point,
                    radiusX: 3.2,
                    radiusY: 3.2,
                };
                rt.FillEllipse(&dot, &gfx.brush);
            }
            rt.PopAxisAlignedClip();
        }
        lightbox::draw(gfx, &pal, &self.key);
        rt.EndDraw(None, None).is_ok()
    }

    /// The pane's header row, printed on the plate as its name is.
    unsafe fn draw_bar(&self, gfx: &Gfx, chat: &mut Chat) -> bool {
        let rt = &gfx.rt;
        let size = rt.GetSize();
        let (w, h) = (size.width, size.height);
        let p = theme::palette();
        let pal = Palette {
            bg: p.plate_top,
            fg: p.text,
            dim: p.text_dim,
            faint: p.text_dim,
            accent: gfx.accent,
        };
        let mut hits: Vec<(Rect, Hit)> = Vec::new();
        rt.BeginDraw();
        rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        rt.Clear(Some(&color(p.plate_top)));
        let (offset, stage_h) = chat.plate;
        let stops = [
            D2D1_GRADIENT_STOP {
                position: 0.0,
                color: color(p.plate_top),
            },
            D2D1_GRADIENT_STOP {
                position: 1.0,
                color: color(p.plate_bottom),
            },
        ];
        if let Ok(b) = rt
            .CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)
            .and_then(|c| {
                rt.CreateLinearGradientBrush(
                    &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                        startPoint: Vector2 { X: 0.0, Y: -offset },
                        endPoint: Vector2 {
                            X: 0.0,
                            Y: stage_h - offset,
                        },
                    },
                    None,
                    &c,
                )
            })
        {
            rt.FillRectangle(&d2d(&Rect::new(0.0, 0.0, w, h)), &b);
        }
        let y = ((h - STRIP_H) / 2.0).round();
        self.strip(gfx, chat, &pal, w, y, &mut hits);
        *self.hits.borrow_mut() = hits;
        rt.EndDraw(None, None).is_ok()
    }

    /// A slim thumb at the glass's right edge: how far along the
    /// conversation the view is, and a way to drag there.
    unsafe fn scroll_bar(&self, gfx: &Gfx, pal: &Palette, chat: &Chat) {
        let area = chat.tr.state.area;
        let track = Rect::new(
            area.right() - BAR_W,
            area.y + BAR_INSET,
            BAR_W,
            area.h - 2.0 * BAR_INSET,
        );
        let thumb = look::thumb(chat.scroll, chat.content_h, chat.view_h, track.h);
        self.track.set(thumb.map(|_| track));
        let Some((at, len)) = thumb else {
            return;
        };
        let lit = self.grab.get().is_some() || self.bar_lit.get();
        let w = if lit { 6.0 } else { 4.0 };
        let r = Rect::new(track.right() - 4.0 - w, track.y + at, w, len);
        let alpha = if lit { 0.42 } else { 0.2 };
        gfx.fill_round(&r, w / 2.0, pal.fg.with_alpha(alpha));
    }

    fn over_bar(&self, x: f32, y: f32) -> bool {
        self.track.get().is_some_and(|t| t.contains(x, y))
    }

    /// A press on the scroll bar takes the thumb; beside it, brings the
    /// thumb's middle there first. False when the press missed the bar.
    fn grab_bar(&self, x: f32, y: f32) -> bool {
        let Some(track) = self.track.get().filter(|t| t.contains(x, y)) else {
            return false;
        };
        let into = with(&self.key, |c| {
            let (at, len) = look::thumb(c.scroll, c.content_h, c.view_h, track.h)?;
            let into = y - track.y - at;
            Some(if (0.0..=len).contains(&into) { into } else { len / 2.0 })
        });
        let Some(Some(into)) = into else {
            return false;
        };
        self.grab.set(Some(into));
        unsafe { SetCapture(self.hwnd()) };
        self.drag_bar(y);
        true
    }

    fn drag_bar(&self, y: f32) {
        let (Some(track), Some(into)) = (self.track.get(), self.grab.get()) else {
            return;
        };
        with(&self.key, |c| {
            c.scroll = look::scroll_at(y - track.y - into, c.content_h, c.view_h, track.h);
            c.follow = look::at_bottom(c.scroll, c.content_h, c.view_h);
        });
        redraw(&self.key);
    }

    /// Asks whether to trust the folder, as Claude Code does before its
    /// first session in one, in the middle of the glass.
    #[allow(clippy::too_many_arguments)]
    unsafe fn trust_card(
        &self,
        gfx: &Gfx,
        pal: &Palette,
        chat: &Chat,
        cx: f32,
        cw: f32,
        area: &Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let w = cw.min(480.0);
        let h = 150.0;
        let x = cx + (cw - w) / 2.0;
        let y = area.y + ((area.h - h) / 2.0).max(16.0);
        let card = Rect::new(x, y, w, h);
        gfx.fill_round(&card, 12.0, pal.bg.mix(pal.fg, 0.06));
        gfx.stroke_round(&card, 12.0, pal.accent.with_alpha(0.45), 1.0);
        let inner = x + 20.0;
        let iw = w - 40.0;
        gfx.text_in(
            "Trust this folder?",
            &gfx.title,
            &Rect::new(inner, y + 16.0, iw, 22.0),
            pal.fg,
        );
        gfx.text_in(
            "Its hooks and MCP servers will run.",
            &gfx.small,
            &Rect::new(inner, y + 42.0, iw, 18.0),
            pal.dim,
        );
        gfx.text_in(
            &chat.dir.to_string_lossy(),
            &gfx.small,
            &Rect::new(inner, y + 62.0, iw, 18.0),
            pal.faint,
        );
        let by = y + h - 50.0;
        let trust = Rect::new(inner, by, 92.0, 30.0);
        let lit = |r: &Rect| {
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            let _ = ScreenToClient(self.hwnd(), &mut p);
            let s = self.scale();
            r.contains(p.x as f32 / s, p.y as f32 / s)
        };
        let fill = if lit(&trust) {
            pal.accent.mix(pal.fg, 0.2)
        } else {
            pal.accent
        };
        gfx.fill_round(&trust, 15.0, fill);
        gfx.text_in("Trust", &gfx.button, &trust, pal.bg);
        hits.push((trust, Hit::Trust(true)));
        let cancel = Rect::new(trust.right() + 10.0, by, 92.0, 30.0);
        if lit(&cancel) {
            gfx.fill_round(&cancel, 15.0, pal.fg.with_alpha(0.08));
        }
        gfx.stroke_round(&cancel, 15.0, pal.fg.with_alpha(0.2), 1.0);
        gfx.text_in("Cancel", &gfx.button, &cancel, pal.dim);
        hits.push((cancel, Hit::Trust(false)));
    }

    /// Whether the keyboard is in the pane this conversation is laid in.
    pub(super) fn has_keyboard(&self) -> bool {
        unsafe {
            let focus = windows::Win32::UI::Input::KeyboardAndMouse::GetFocus();
            GetParent(self.hwnd()).is_ok_and(|p| p == focus)
        }
    }

    pub(super) fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        // Buttons are pushed after what they sit on, so the last wins.
        (self.hits.borrow().iter().rev())
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, h)| h.clone())
    }

    /// The button under a point, for lighting it.
    pub(super) fn hit_rect(&self, x: f32, y: f32) -> Option<Rect> {
        (self.hits.borrow().iter().rev())
            .find(|(r, _)| r.contains(x, y))
            .map(|(r, _)| *r)
    }

    pub(super) fn on_press(&self, lparam: LPARAM, double: bool) {
        let hwnd = self.hwnd();
        unsafe {
            // The pane keeps the keyboard, so typing goes on into the
            // composer wherever the click was.
            if let Ok(pane) = GetParent(hwnd) {
                let _ = SetFocus(Some(pane));
            }
        }
        let (x, y) = self.dip(lparam);
        if !self.bar && lightbox::press(self, x, y) {
            return;
        }
        if !self.bar && self.grab_bar(x, y) {
            return;
        }
        let key = self.key.clone();
        let hit = self.hit_at(x, y);
        // The bar has nothing to select.
        if self.bar && hit.is_none() {
            return;
        }
        // A double click's first press already did what a button does; its
        // second only selects a word in text.
        if double && !matches!(hit, None | Some(Hit::Input(Act::Text))) {
            return;
        }
        select::unfocus(&key, unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0);
        match hit {
            Some(Hit::Answer(id, allow)) => answer(&key, &id, allow),
            Some(Hit::Tr(a)) => select::act(&key, a),
            Some(Hit::Fold(i)) => {
                with(&key, |c| {
                    if !c.open.remove(&i) {
                        c.open.insert(i);
                    }
                    c.follow = false;
                });
            }
            Some(Hit::Input(Act::Stop)) => {
                with(&key, composer::hold);
                interrupt(&key);
            }
            Some(Hit::Trust(true)) => super::trust_folder(&key),
            Some(Hit::Trust(false)) => app::push(Input::CloseFolio(key.clone())),
            Some(Hit::Input(act)) => {
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                self.press_input(act, x, y, double, shift);
            }
            None => {
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                select::press(self, x, y, double, shift);
            }
        }
        redraw(&key);
    }

    pub(super) fn on_drag(&self, lparam: LPARAM) {
        let (x, y) = self.dip(lparam);
        if !self.bar && lightbox::drag(self, x, y) {
            return;
        }
        if self.grab.get().is_some() {
            self.drag_bar(y);
            return;
        }
        let over = self.over_bar(x, y);
        if self.bar_lit.replace(over) != over {
            redraw(&self.key);
        }
        if !self.bar && select::moved(self, x, y) {
            return;
        }
        self.hover_input(x, y);
    }

    pub(super) fn cursor(&self) -> bool {
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
            let _ = ScreenToClient(self.hwnd(), &mut p);
        }
        let s = self.scale();
        let boxed = (!self.bar)
            .then(|| lightbox::cursor(self, p.x as f32 / s, p.y as f32 / s))
            .flatten();
        let id = match self.hit_at(p.x as f32 / s, p.y as f32 / s) {
            _ if boxed.is_some() => boxed.unwrap_or(IDC_ARROW),
            _ if self.over_bar(p.x as f32 / s, p.y as f32 / s) => IDC_ARROW,
            Some(Hit::Input(Act::Text)) => IDC_IBEAM,
            Some(_) => IDC_HAND,
            None if !self.bar && select::over_text(&self.key, p.x as f32 / s, p.y as f32 / s) => {
                IDC_IBEAM
            }
            None => IDC_ARROW,
        };
        unsafe {
            if let Ok(c) = LoadCursorW(None, id) {
                SetCursor(Some(c));
            }
        }
        true
    }

    pub(super) fn handle(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
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
                    let _ = render::resize_target(&g.win, w, h);
                }
                unsafe {
                    let _ = InvalidateRect(Some(self.hwnd()), None, false);
                }
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == BLINK || wparam.0 == SETTLE => {
                let id = wparam.0;
                unsafe {
                    let _ = KillTimer(Some(self.hwnd()), id);
                    let _ = InvalidateRect(Some(self.hwnd()), None, false);
                }
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TICK => {
                if self.seen() {
                    if !self.tick() {
                        unsafe {
                            let _ = InvalidateRect(Some(self.hwnd()), None, false);
                        }
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
                let (x, y) = self.dip(lparam);
                if !self.bar && lightbox::release(self, x, y) {
                    return Some(LRESULT(0));
                }
                if self.grab.take().is_some() {
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    redraw(&self.key);
                }
                select::release(&self.key);
                if self.selecting.replace(false) {
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                if !self.bar {
                    select::left(&self.key);
                }
                if self.bar_lit.replace(false) {
                    redraw(&self.key);
                }
                self.leave_input();
                Some(LRESULT(0))
            }
            WM_DROPFILES => {
                let drop = HDROP(wparam.0 as *mut c_void);
                let paths = clipboard::drop_paths(drop);
                unsafe { DragFinish(drop) };
                composer::drop_files(&self.key, paths);
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                lightbox::lost_capture(&self.key);
                self.selecting.set(false);
                self.grab.set(None);
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
                    if c.bar == Some(hwnd) {
                        c.bar = None;
                    }
                });
                None
            }
            _ => None,
        }
    }
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
