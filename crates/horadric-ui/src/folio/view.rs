//! The Folio's window over a pane's glass: its graphics, the paint loop,
//! input from the pane and the mouse, and what a click hits.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::time::Instant;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_GRADIENT_STOP, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{
    ID2D1HwndRenderTarget, ID2D1LinearGradientBrush, ID2D1RadialGradientBrush,
    ID2D1SolidColorBrush, ID2D1StrokeStyle, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_CAP_STYLE_ROUND, D2D1_DASH_STYLE_SOLID, D2D1_DRAW_TEXT_OPTIONS_NONE,
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
    GetKeyState, ReleaseCapture, SetFocus, VK_NEXT, VK_PRIOR, VK_SHIFT,
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

use super::composer::{self, Act};
use super::{answer, interrupt, redraw, select, send_prompt, transcript, with, Chat, CHATS};
use crate::app::{self, Input};
use crate::clipboard;
use crate::folio_look as look;
use crate::keys::Mods;
use crate::layout::Rect;
use crate::render::{self, color, Gpu};
use crate::sparks::RUNES;
use crate::theme::{self, Color};
use crate::window::Shared;

const CLASS: PCWSTR = w!("HoradricFolio");
const TICK: usize = 1;
/// Between frames while the background moves. It drifts slowly, so this is
/// smooth enough and costs a third of the display's rate.
const FRAME_MS: u32 = 40;
/// The glass's corner, less the line of its rim the conversation leaves.
const CORNER: f32 = 7.0;

pub(super) const FONT: PCWSTR = w!("Segoe UI Variable Text");
const MONO: PCWSTR = w!("Cascadia Mono");
const ICONS: PCWSTR = w!("Segoe Fluent Icons");

/// The width of the button that opens the session in a terminal.
const TERMINAL_W: f32 = 88.0;

/// Not in the windows crate's WindowsAndMessaging.
const WM_MOUSELEAVE: u32 = 0x02A3;

/// A wheel notch, in DIPs.
const NOTCH: f32 = 54.0;
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
        // Files dropped on the conversation go into the composer.
        unsafe { DragAcceptFiles(hwnd, true) };
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

pub(super) fn dpi(hwnd: HWND) -> u32 {
    unsafe { GetDpiForWindow(hwnd) }.max(96)
}

/// A key that makes a character, from the pane: the composer's.
pub fn char(key: &str, c: char, mods: Mods) {
    if select::char(key, c, mods) {
        return;
    }
    composer::on_char(key, c, mods);
}

/// A key that makes no character, from the pane. False leaves it to the
/// pane.
pub fn key(key: &str, vk: u16, mods: Mods) -> bool {
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
        _ => return composer::on_key(key, vk, mods),
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
    /// Opens the session in a terminal.
    Terminal,
    /// The transcript's own buttons and links.
    Tr(transcript::Act),
}

pub(super) struct Gfx {
    pub(super) rt: ID2D1HwndRenderTarget,
    /// The window's DPI the target draws at. Moving to another monitor
    /// changes the window's, and the target is told.
    pub(super) dpi: u32,
    pub(super) brush: ID2D1SolidColorBrush,
    pub(super) glow: ID2D1RadialGradientBrush,
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
    pub(super) key: String,
    pub(super) shared: Rc<Shared>,
    pub(super) gfx: RefCell<Option<Gfx>>,
    pub(super) hits: RefCell<Vec<(Rect, Hit)>>,
    /// The composer's text laid out and where it was drawn, in DIPs, for
    /// placing the caret by a click or an arrow.
    pub(super) composer: RefCell<Option<(IDWriteTextLayout, f32, f32)>>,
    pub(super) ticking: Cell<bool>,
    pub(super) last: Cell<Option<Instant>>,
    /// The background's own time, in seconds. It runs only while it moves,
    /// so warming again picks up where it stopped.
    pub(super) clock: Cell<f32>,
    pub(super) selecting: Cell<bool>,
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

    /// Runs the frame clock while the background moves and it is seen,
    /// and stops it otherwise.
    pub(super) fn clock(&self, moving: bool) {
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
    pub(super) unsafe fn draw(&self, gfx: &Gfx, chat: &mut Chat) -> bool {
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
        self.transcript(gfx, &pal, chat, cx, cw, &area, t, &mut hits);
        // What scrolls up is hidden under the top edge, where the Terminal
        // button sits, and fades in below it, so the two never overlap.
        // At the top nothing is under there: the first block starts lower.
        if chat.scroll > 0.0 {
            const SOLID: f32 = 22.0;
            const FADE: f32 = 18.0;
            gfx.fill(&Rect::new(0.0, 0.0, w, SOLID), pal.bg);
            for i in 0..6 {
                let band = Rect::new(0.0, SOLID + FADE * i as f32 / 6.0, w, FADE / 6.0);
                gfx.fill(&band, pal.bg.with_alpha(1.0 - (i as f32 + 0.5) / 6.0));
            }
        }

        // The glass is sunk into the plate, so its top edge is in shade.
        gfx.shade.SetStartPoint(Vector2 { X: 0.0, Y: 0.0 });
        gfx.shade.SetEndPoint(Vector2 { X: 0.0, Y: 9.0 });
        rt.FillRectangle(&d2d(&Rect::new(0.0, 0.0, w, 9.0)), &gfx.shade);
        self.terminal_button(gfx, &pal, chat, w, &mut composer_hits);
        self.composer_menu(gfx, chat, &pal, cx, cw, &mut composer_hits);

        hits.extend(composer_hits);
        *self.hits.borrow_mut() = hits;
        rt.EndDraw(None, None).is_ok()
    }

    /// "Terminal" at the top right: the session goes on in a terminal pane,
    /// for what only the CLI does. Only between turns, since claude.exe
    /// ends first, and only once there is a conversation to resume.
    unsafe fn terminal_button(
        &self,
        gfx: &Gfx,
        pal: &Palette,
        chat: &Chat,
        w: f32,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let ready = !chat.folio.busy
            && !chat.ended
            && !chat.to_terminal
            && chat.folio.session_id.is_some()
            && chat.folio.pending_asks().is_empty();
        if !ready {
            return;
        }
        // Above where the first block starts, so at the top it covers nothing.
        let r = Rect::new(w - TERMINAL_W - 8.0, 2.0, TERMINAL_W, 18.0);
        gfx.fill_round(&r, 9.0, pal.bg.mix(pal.fg, 0.07).with_alpha(0.9));
        gfx.stroke_round(&r, 9.0, pal.fg.with_alpha(0.14), 1.0);
        let icon = Rect::new(r.x + 8.0, r.y, 16.0, r.h);
        gfx.text_in("\u{E756}", &gfx.icon, &icon, pal.dim);
        let label = Rect::new(r.x + 28.0, r.y, r.w - 34.0, r.h);
        gfx.text_in("Terminal", &gfx.small, &label, pal.dim);
        hits.push((r, Hit::Terminal));
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
        let key = self.key.clone();
        select::unfocus(&key, unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0);
        match self.hit_at(x, y) {
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
            Some(Hit::Input(Act::Send)) => send_prompt(&key),
            Some(Hit::Input(Act::Stop)) => {
                with(&key, composer::hold);
                interrupt(&key);
            }
            Some(Hit::Terminal) => app::push(Input::FolioToTerminal(key.clone())),
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
        if select::moved(self, x, y) {
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
        let id = match self.hit_at(p.x as f32 / s, p.y as f32 / s) {
            Some(Hit::Input(Act::Text)) => IDC_IBEAM,
            Some(_) => IDC_HAND,
            None if select::over_text(&self.key, p.x as f32 / s, p.y as f32 / s) => IDC_IBEAM,
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
                select::release(&self.key);
                if self.selecting.replace(false) {
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                select::left(&self.key);
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
