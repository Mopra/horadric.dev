//! A window drawn with its own alpha through a DIB, the way layered
//! windows take per pixel alpha, and the pen that draws on it.
//!
//! Three ways to change what is on screen, each as cheap as the change:
//! paint and present all of it, paint and present one part of it, or
//! move and fade it without sending a pixel. The overlay is up for as
//! long as a session has the desktop, so what it costs while nothing
//! changes is what it costs.

use windows::core::{w, Result};
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DCRenderTarget, ID2D1RenderTarget, ID2D1SolidColorBrush, ID2D1StrokeStyle,
    D2D1_CAP_STYLE_ROUND, D2D1_DASH_STYLE_SOLID, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_LINE_JOIN_ROUND, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
    D2D1_STROKE_STYLE_PROPERTIES, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{IDWriteTextFormat, DWRITE_MEASURING_MODE_NATURAL};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, AC_SRC_ALPHA,
    AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC,
    HGDIOBJ,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetCursorPos, GetWindowLongPtrW, SetCursorPos,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, UpdateLayeredWindow, UpdateLayeredWindowIndirect,
    GWL_EXSTYLE, HWND_TOPMOST, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, UPDATELAYEREDWINDOWINFO, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows_numerics::{Matrix3x2, Vector2};

use super::CLASS;
use crate::render::{self, color};
use crate::sparks::RUNES;
use crate::theme::{self, Color};

/// A rectangle in DIPs: x, y, width, height.
pub(super) type Area = (f32, f32, f32, f32);

pub(super) struct Layer {
    pub hwnd: HWND,
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    rt: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    round: Option<ID2D1StrokeStyle>,
    pub pos: (i32, i32),
    pub size: (i32, i32),
    pub scale: f32,
    pub shown: bool,
    /// The alpha it was last put on screen with.
    alpha: u8,
}

fn alpha_byte(alpha: f32) -> u8 {
    (alpha.clamp(0.0, 1.0) * 255.0).round() as u8
}

impl Layer {
    pub fn new(gpu: &render::Gpu, pos: (i32, i32), size: (i32, i32), scale: f32) -> Result<Layer> {
        unsafe {
            // Not left out of screen captures: a recording or a remote
            // tool that works by capture must show the human what takes
            // their clicks. A session's own screenshot is the program's
            // window drawn by itself, which holds none of this.
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
            // Drawn by the processor: what is drawn here is a few lines and
            // a few words, and it ends in a GDI bitmap, which a paint by
            // the graphics card would have to be read back into.
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
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
            // A rune's cuts end round, as the Folio draws them.
            let round = gpu
                .d2d
                .CreateStrokeStyle(
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
                )
                .ok();
            let dc = CreateCompatibleDC(None);
            let mut layer = Layer {
                hwnd,
                dc,
                bitmap: HBITMAP::default(),
                old: HGDIOBJ::default(),
                rt,
                brush,
                round,
                pos,
                size: (0, 0),
                scale,
                shown: false,
                alpha: 0,
            };
            layer.resize(size)?;
            Ok(layer)
        }
    }

    pub fn resize(&mut self, size: (i32, i32)) -> Result<()> {
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

    /// `area` in the window's pixels, grown to whole pixels and kept
    /// inside it.
    fn pixels(&self, area: Area) -> RECT {
        let s = self.scale;
        RECT {
            left: ((area.0 * s).floor() as i32).clamp(0, self.size.0),
            top: ((area.1 * s).floor() as i32).clamp(0, self.size.1),
            right: (((area.0 + area.2) * s).ceil() as i32).clamp(0, self.size.0),
            bottom: (((area.1 + area.3) * s).ceil() as i32).clamp(0, self.size.1),
        }
    }

    /// Draws the whole window afresh, in DIPs.
    pub fn paint(&self, draw: impl FnOnce(&Pen)) -> Result<()> {
        let all = RECT {
            left: 0,
            top: 0,
            right: self.size.0,
            bottom: self.size.1,
        };
        self.paint_in(all, draw)
    }

    /// Draws `area` of the window afresh and leaves the rest as it was.
    /// `draw` draws in the window's own DIPs, and only what lies in the
    /// area is kept. Only the area is bound, so only it is rendered and
    /// copied: a rune changing costs a rune, not the plate it is on.
    pub fn paint_part(&self, area: Area, draw: impl FnOnce(&Pen)) -> Result<()> {
        let r = self.pixels(area);
        if r.right <= r.left || r.bottom <= r.top {
            return Ok(());
        }
        self.paint_in(r, draw)
    }

    fn paint_in(&self, r: RECT, draw: impl FnOnce(&Pen)) -> Result<()> {
        unsafe {
            self.rt.BindDC(self.dc, &r)?;
            let dpi = 96.0 * self.scale;
            self.rt.SetDpi(dpi, dpi);
            self.rt.BeginDraw();
            // The bound part's corner is the target's origin.
            self.rt.SetTransform(&Matrix3x2::translation(
                -(r.left as f32) / self.scale,
                -(r.top as f32) / self.scale,
            ));
            self.rt.Clear(Some(&D2D1_COLOR_F::default()));
            draw(&Pen {
                rt: &self.rt,
                brush: &self.brush,
                round: self.round.as_ref(),
            });
            self.rt.SetTransform(&Matrix3x2::identity());
            self.rt.EndDraw(None, None)
        }
    }

    fn blend(alpha: u8) -> BLENDFUNCTION {
        BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: alpha,
            AlphaFormat: AC_SRC_ALPHA as u8,
        }
    }

    fn show(&mut self) {
        if !self.shown {
            self.shown = true;
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }

    /// Puts all that was painted on screen at `pos`, `alpha` over it.
    pub fn present(&mut self, pos: (i32, i32), alpha: f32) {
        self.pos = pos;
        self.alpha = alpha_byte(alpha);
        let blend = Self::blend(self.alpha);
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
        }
        self.show();
    }

    /// Puts `area` of what was painted on screen, where the window is and
    /// with the alpha it has: only that part is sent.
    pub fn present_part(&mut self, area: Area) {
        let dirty = self.pixels(area);
        if dirty.right <= dirty.left || dirty.bottom <= dirty.top {
            return;
        }
        let blend = Self::blend(self.alpha);
        let at = POINT {
            x: self.pos.0,
            y: self.pos.1,
        };
        let size = SIZE {
            cx: self.size.0,
            cy: self.size.1,
        };
        let from = POINT { x: 0, y: 0 };
        let info = UPDATELAYEREDWINDOWINFO {
            cbSize: std::mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
            hdcDst: HDC::default(),
            pptDst: &at,
            psize: &size,
            hdcSrc: self.dc,
            pptSrc: &from,
            crKey: COLORREF(0),
            pblend: &blend,
            dwFlags: ULW_ALPHA,
            prcDirty: &dirty,
        };
        unsafe {
            let _ = UpdateLayeredWindowIndirect(self.hwnd, &info);
        }
        self.show();
    }

    /// Moves and fades what is on screen without sending a pixel, and
    /// does nothing when neither changed. For a window never presented
    /// this shows nothing: there is nothing of it to move.
    pub fn place(&mut self, pos: (i32, i32), alpha: f32) {
        let alpha = alpha_byte(alpha);
        if !self.shown || (pos == self.pos && alpha == self.alpha) {
            return;
        }
        self.pos = pos;
        self.alpha = alpha;
        let blend = Self::blend(alpha);
        let at = POINT { x: pos.0, y: pos.1 };
        unsafe {
            let _ = UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&at),
                None,
                None,
                None,
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }

    /// Puts the window above the other topmost ones, so the mark is never
    /// under the plate, which is as topmost as it is and made later.
    pub fn raise(&self) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    /// Lets the mouse reach the window, or go through it. Toggled as the
    /// cursor comes over a control, since a click on another process's
    /// window behind does not come through `HTTRANSPARENT`.
    ///
    /// Windows sends a click to the window it found under the pointer at
    /// the pointer's last move, so a style changed under a pointer at rest
    /// would not count until it moved: a click without a move went through
    /// the plate. `SWP_FRAMECHANGED` makes Windows take the new style in
    /// at once, and putting the pointer back where it is makes it look
    /// again what is under it, without the pointer moving.
    pub fn set_clickable(&self, on: bool, at: POINT) {
        unsafe {
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            let through = WS_EX_TRANSPARENT.0 as isize;
            let want = if on { ex & !through } else { ex | through };
            if want == ex {
                return;
            }
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, want);
            let _ = SetWindowPos(
                self.hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            // Only where the human left it: a pointer that moved since has
            // been looked at by Windows already, and is not pulled back.
            let mut now = POINT::default();
            if GetCursorPos(&mut now).is_ok() && now.x == at.x && now.y == at.y {
                let _ = SetCursorPos(at.x, at.y);
            }
        }
    }

    pub fn hide(&mut self) {
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
pub(super) struct Pen<'a> {
    pub rt: &'a ID2D1RenderTarget,
    pub brush: &'a ID2D1SolidColorBrush,
    round: Option<&'a ID2D1StrokeStyle>,
}

pub(super) fn d2rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}

impl Pen<'_> {
    pub unsafe fn fill(&self, r: D2D_RECT_F, c: Color) {
        self.brush.SetColor(&color(c));
        self.rt.FillRectangle(&r, self.brush);
    }

    pub unsafe fn fill_rounded(&self, r: D2D_RECT_F, radius: f32, c: Color) {
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

    pub unsafe fn stroke_rounded(&self, r: D2D_RECT_F, radius: f32, c: Color, width: f32) {
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

    pub unsafe fn circle(&self, at: (f32, f32), r: f32, c: Color, stroke: Option<f32>) {
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

    pub unsafe fn text(&self, fmt: &IDWriteTextFormat, c: Color, s: &str, r: D2D_RECT_F) {
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

    /// One of the twelve runes of [`crate::sparks::RUNES`], `h` tall and
    /// centred on `at`, its head toward `up` (a unit vector, (0, -1) for
    /// one standing upright), `drawn` of its strokes cut so far in the
    /// order a hand would cut them, `width` wide in `ink`.
    pub unsafe fn rune(
        &self,
        which: usize,
        at: (f32, f32),
        up: (f32, f32),
        h: f32,
        ink: Color,
        width: f32,
        drawn: f32,
    ) {
        if ink.a <= 0.0 {
            return;
        }
        let strokes = RUNES[which % RUNES.len()];
        // Across is a quarter turn from up, so an upright rune reads as
        // the table draws it.
        let (ax, ay) = (-up.1, up.0);
        let place = |(u, v): (f32, f32)| Vector2 {
            X: at.0 + up.0 * (0.5 - v) * h + ax * u * h,
            Y: at.1 + up.1 * (0.5 - v) * h + ay * u * h,
        };
        let len = |a: (f32, f32), b: (f32, f32)| (b.0 - a.0).hypot(b.1 - a.1);
        let total: f32 = strokes
            .iter()
            .flat_map(|s| s.windows(2))
            .map(|w| len(w[0], w[1]))
            .sum();
        let mut left = total * drawn.clamp(0.0, 1.0);
        self.brush.SetColor(&color(ink));
        for seg in strokes.iter().flat_map(|s| s.windows(2)) {
            if left <= 0.0 {
                break;
            }
            let l = len(seg[0], seg[1]);
            let f = (left / l).min(1.0);
            let end = (
                seg[0].0 + (seg[1].0 - seg[0].0) * f,
                seg[0].1 + (seg[1].1 - seg[0].1) * f,
            );
            self.rt
                .DrawLine(place(seg[0]), place(end), self.brush, width, self.round);
            left -= l;
        }
    }

    /// A rune that reads on anything: the screen behind the overlay is
    /// whatever the human has open, white paper or a dark editor, so its
    /// cut gets a dark bed a little wider than itself. `level` is how
    /// strongly it shows, both together.
    pub unsafe fn rune_on_anything(
        &self,
        which: usize,
        at: (f32, f32),
        up: (f32, f32),
        h: f32,
        ink: Color,
        width: f32,
        level: f32,
    ) {
        let bed = Color::rgb(0x000000).with_alpha(0.55 * level);
        self.rune(which, at, up, h, bed, width + 2.0, 1.0);
        self.rune(which, at, up, h, ink.with_alpha(level), width, 1.0);
    }
}
