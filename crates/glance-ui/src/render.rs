//! Direct2D and DirectWrite drawing for a cluster window.
//!
//! [`Gpu`] holds the process wide factories and text formats. [`Target`] is
//! one window's render target and brushes. Drawing happens in DIPs; Direct2D
//! applies the DPI.

use std::time::SystemTime;

use glance_core::{format_age, Phase, Session};
use windows::core::{w, Result};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1SolidColorBrush,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows_numerics::Vector2;

use crate::layout::{ClusterLayout, Metrics, Rect};
use crate::theme::{self, Color};

const FONT: windows::core::PCWSTR = w!("Segoe UI Variable Text");

/// Process wide Direct2D and DirectWrite objects.
pub struct Gpu {
    pub d2d: ID2D1Factory,
    pub dw: IDWriteFactory,
    pub title: IDWriteTextFormat,
    pub body: IDWriteTextFormat,
    pub small: IDWriteTextFormat,
    pub small_right: IDWriteTextFormat,
    pub title_center: IDWriteTextFormat,
}

impl Gpu {
    pub fn new() -> Result<Self> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;

            let title = format(&dw, 13.0, true, false)?;
            let body = format(&dw, 13.0, false, false)?;
            let small = format(&dw, 11.5, false, false)?;
            let small_right = format(&dw, 11.5, false, true)?;
            let title_center = format(&dw, 16.0, false, false)?;
            title_center.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            Ok(Gpu {
                d2d,
                dw,
                title,
                body,
                small,
                small_right,
                title_center,
            })
        }
    }
}

/// One line, vertically centred, trimmed with an ellipsis, no wrapping.
unsafe fn format(
    dw: &IDWriteFactory,
    size: f32,
    bold: bool,
    right: bool,
) -> Result<IDWriteTextFormat> {
    let weight = if bold {
        DWRITE_FONT_WEIGHT_SEMI_BOLD
    } else {
        DWRITE_FONT_WEIGHT_NORMAL
    };
    let f = dw.CreateTextFormat(
        FONT,
        None,
        weight,
        DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_STRETCH_NORMAL,
        size,
        w!("en-us"),
    )?;
    f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
    f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
    if right {
        f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
    }
    let trimming = DWRITE_TRIMMING {
        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
        delimiter: 0,
        delimiterCount: 0,
    };
    let sign = dw.CreateEllipsisTrimmingSign(&f)?;
    f.SetTrimming(&trimming, &sign)?;
    Ok(f)
}

/// Everything one frame of a cluster needs.
pub struct Scene<'a> {
    pub layout: &'a ClusterLayout,
    pub name: &'a str,
    pub collapsed: bool,
    pub sessions: &'a [&'a Session],
    pub now: SystemTime,
}

/// A window's render target. Recreated when Direct2D asks for it.
pub struct Target {
    rt: ID2D1HwndRenderTarget,
    brush: ID2D1SolidColorBrush,
}

/// A render target for a window, at its DPI, drawing in DIPs.
pub fn hwnd_target(
    gpu: &Gpu,
    hwnd: HWND,
    width_px: u32,
    height_px: u32,
    dpi: u32,
) -> Result<ID2D1HwndRenderTarget> {
    unsafe {
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: 0.0,
            dpiY: 0.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd,
            pixelSize: D2D_SIZE_U {
                width: width_px.max(1),
                height: height_px.max(1),
            },
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };
        let rt = gpu.d2d.CreateHwndRenderTarget(&props, &hwnd_props)?;
        rt.SetDpi(dpi as f32, dpi as f32);
        Ok(rt)
    }
}

pub fn resize_target(rt: &ID2D1HwndRenderTarget, width_px: u32, height_px: u32) -> Result<()> {
    unsafe {
        rt.Resize(&D2D_SIZE_U {
            width: width_px.max(1),
            height: height_px.max(1),
        })
    }
}

impl Target {
    pub fn new(gpu: &Gpu, hwnd: HWND, width_px: u32, height_px: u32, dpi: u32) -> Result<Self> {
        let rt = hwnd_target(gpu, hwnd, width_px, height_px, dpi)?;
        let brush = unsafe { rt.CreateSolidColorBrush(&color(theme::TEXT), None)? };
        Ok(Target { rt, brush })
    }

    pub fn resize(&self, width_px: u32, height_px: u32) -> Result<()> {
        resize_target(&self.rt, width_px, height_px)
    }

    pub fn set_dpi(&self, dpi: u32) {
        unsafe { self.rt.SetDpi(dpi as f32, dpi as f32) }
    }

    /// Draws a whole cluster. `Err` means the target must be recreated.
    pub fn draw(&self, gpu: &Gpu, m: &Metrics, scene: &Scene) -> Result<()> {
        unsafe {
            self.rt.BeginDraw();
            self.rt.Clear(Some(&color(theme::WINDOW_BG)));

            self.header(
                gpu,
                scene.layout,
                scene.name,
                scene.collapsed,
                scene.sessions,
            );
            for (rect, s) in scene.layout.tiles.iter().zip(scene.sessions) {
                self.tile(gpu, m, rect, s, scene.now);
            }

            self.rt.EndDraw(None, None)
        }
    }

    unsafe fn header(
        &self,
        gpu: &Gpu,
        layout: &ClusterLayout,
        name: &str,
        collapsed: bool,
        sessions: &[&Session],
    ) {
        let h = layout.header;
        let chevron = if collapsed { "\u{25B8}" } else { "\u{25BE}" };
        self.text(
            &gpu.title,
            theme::TEXT_DIM,
            chevron,
            Rect::new(h.x, h.y, 14.0, h.h),
        );
        self.text(
            &gpu.title,
            theme::TEXT,
            name,
            Rect::new(h.x + 16.0, h.y, h.w * 0.6, h.h),
        );

        let waiting = sessions.iter().filter(|s| s.phase.is_waiting()).count();
        let working = sessions
            .iter()
            .filter(|s| s.phase == Phase::Working)
            .count();
        let summary = match (working, waiting) {
            (0, 0) => format!("{}", sessions.len()),
            (w, 0) => format!("{w} working"),
            (0, a) => format!("{a} waiting"),
            (w, a) => format!("{w} working \u{00B7} {a} waiting"),
        };
        let c = if waiting > 0 {
            theme::WAITING
        } else {
            theme::TEXT_DIM
        };
        let summary_rect = Rect::new(h.x, h.y, layout.new.x - h.x - 2.0, h.h);
        self.text(&gpu.small_right, c, &summary, summary_rect);
        self.text(&gpu.title_center, theme::TEXT_DIM, "+", layout.new);
    }

    unsafe fn tile(&self, gpu: &Gpu, m: &Metrics, r: &Rect, s: &Session, now: SystemTime) {
        let waiting = s.phase.is_waiting();
        let bg = if waiting {
            theme::TILE_BG_WAITING
        } else {
            theme::TILE_BG
        };
        let rr = D2D1_ROUNDED_RECT {
            rect: rect(r),
            radiusX: m.tile_radius,
            radiusY: m.tile_radius,
        };
        self.brush.SetColor(&color(bg));
        self.rt.FillRoundedRectangle(&rr, &self.brush);
        if waiting {
            let inner = D2D1_ROUNDED_RECT {
                rect: rect(&r.inset(0.75)),
                radiusX: m.tile_radius - 0.75,
                radiusY: m.tile_radius - 0.75,
            };
            self.brush.SetColor(&color(theme::phase_color(&s.phase)));
            self.rt.DrawRoundedRectangle(&inner, &self.brush, 1.5, None);
        }

        let pad = 10.0;
        let row_h = r.h / 2.0;
        let top = Rect::new(r.x + pad, r.y + 4.0, r.w - 2.0 * pad, row_h - 2.0);
        let bottom = Rect::new(r.x + pad, r.y + row_h - 2.0, r.w - 2.0 * pad, row_h - 4.0);

        // State dot.
        let dot = D2D1_ELLIPSE {
            point: Vector2 {
                X: top.x + m.dot / 2.0,
                Y: top.y + top.h / 2.0,
            },
            radiusX: m.dot / 2.0,
            radiusY: m.dot / 2.0,
        };
        self.brush.SetColor(&color(theme::phase_color(&s.phase)));
        self.rt.FillEllipse(&dot, &self.brush);

        // Name, then the age line right aligned on the same row.
        let age = format!(
            "{} {}",
            theme::phase_verb(&s.phase),
            format_age(now.duration_since(s.since).unwrap_or_default())
        );
        let age_w = (age.len() as f32 * 6.5).min(top.w * 0.55);
        let name_rect = Rect::new(
            top.x + m.dot + 8.0,
            top.y,
            top.w - m.dot - 8.0 - age_w,
            top.h,
        );
        self.text(&gpu.body, theme::TEXT, &s.name, name_rect);
        let age_c = if waiting {
            theme::phase_color(&s.phase)
        } else {
            theme::TEXT_DIM
        };
        self.text(&gpu.small_right, age_c, &age, top);

        // Last line.
        let last = if s.last_line.is_empty() {
            &s.cwd
        } else {
            &s.last_line
        };
        self.text(&gpu.small, theme::TEXT_DIM, last, bottom);
    }

    unsafe fn text(&self, fmt: &IDWriteTextFormat, c: Color, s: &str, r: Rect) {
        if r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        let wide: Vec<u16> = s.encode_utf16().collect();
        self.brush.SetColor(&color(c));
        self.rt.DrawText(
            &wide,
            fmt,
            &rect(&r),
            &self.brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
}

fn color(c: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
}

fn rect(r: &Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.x,
        top: r.y,
        right: r.right(),
        bottom: r.bottom(),
    }
}
