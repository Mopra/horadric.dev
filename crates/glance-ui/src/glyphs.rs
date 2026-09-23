//! Draws a terminal [`Frame`] with Direct2D glyph runs.
//!
//! One monospace font, four faces. Every glyph in a run is given the cell
//! width as its advance, so text lands exactly on the grid no matter what the
//! font's own advances say. Characters the font lacks go through DirectWrite
//! font fallback one at a time, each pinned to its own cell.

use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::ManuallyDrop;

use alacritty_terminal::vte::ansi::{CursorShape, Rgb};
use windows::core::{w, Result, BOOL, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{
    ID2D1HwndRenderTarget, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_ALIASED,
    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_DRAW_TEXT_OPTIONS_NONE,
};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteFactory, IDWriteFontCollection, IDWriteFontFace, IDWriteTextFormat, DWRITE_FONT_METRICS,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE, DWRITE_FONT_STYLE_ITALIC,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_BOLD,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_GLYPH_METRICS, DWRITE_GLYPH_RUN,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows_numerics::Vector2;

use crate::frame::{Decoration, Frame, BOLD, ITALIC};
use crate::render::{hwnd_target, Gpu};

/// Cascadia ships with Windows 11. Consolas is on every Windows since Vista.
const FAMILIES: [PCWSTR; 2] = [w!("Cascadia Mono"), w!("Consolas")];

/// Font size in DIPs. 14 DIPs is 10.5 points at 100 % scaling.
pub const FONT_SIZE: f32 = 14.0;

/// Space between the grid and the window edge, in DIPs.
pub const PAD: f32 = 6.0;

/// Cell geometry in DIPs, snapped so every cell edge is a whole device pixel.
/// Without the snap, backgrounds of neighbouring cells leave hairline seams.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellSize {
    pub w: f32,
    pub h: f32,
    /// From the top of the cell to the baseline.
    pub baseline: f32,
    pub underline: f32,
    pub strike: f32,
    pub stroke: f32,
}

pub struct Font {
    /// Regular, bold, italic, bold italic: indexed by the frame's style bits.
    faces: [IDWriteFontFace; 4],
    formats: [IDWriteTextFormat; 4],
    metrics: DWRITE_FONT_METRICS,
    /// Advance of `0` in design units. Monospace, so every glyph's.
    advance: u32,
    size: f32,
    cache: RefCell<HashMap<(char, u8), u16>>,
}

impl Font {
    pub fn new(dw: &IDWriteFactory) -> Result<Font> {
        unsafe {
            let mut collection: Option<IDWriteFontCollection> = None;
            dw.GetSystemFontCollection(&mut collection, false)?;
            let collection = collection.ok_or_else(windows::core::Error::empty)?;

            let mut family_index = 0u32;
            let mut family_name = FAMILIES[0];
            for name in FAMILIES {
                let mut exists = BOOL(0);
                collection.FindFamilyName(name, &mut family_index, &mut exists)?;
                if exists.as_bool() {
                    family_name = name;
                    break;
                }
            }
            let family = collection.GetFontFamily(family_index)?;

            let variants: [(DWRITE_FONT_WEIGHT, DWRITE_FONT_STYLE); 4] = [
                (DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL),
                (DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_STYLE_NORMAL),
                (DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_ITALIC),
                (DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_STYLE_ITALIC),
            ];
            let face = |(weight, style): (DWRITE_FONT_WEIGHT, DWRITE_FONT_STYLE)| {
                family
                    .GetFirstMatchingFont(weight, DWRITE_FONT_STRETCH_NORMAL, style)?
                    .CreateFontFace()
            };
            let format = |(weight, style): (DWRITE_FONT_WEIGHT, DWRITE_FONT_STYLE)| {
                let f = dw.CreateTextFormat(
                    family_name,
                    None,
                    weight,
                    style,
                    DWRITE_FONT_STRETCH_NORMAL,
                    FONT_SIZE,
                    w!("en-us"),
                )?;
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                Ok::<_, windows::core::Error>(f)
            };
            let faces = [
                face(variants[0])?,
                face(variants[1])?,
                face(variants[2])?,
                face(variants[3])?,
            ];
            let formats = [
                format(variants[0])?,
                format(variants[1])?,
                format(variants[2])?,
                format(variants[3])?,
            ];

            let mut metrics = DWRITE_FONT_METRICS::default();
            faces[0].GetMetrics(&mut metrics);
            let zero = glyph_index(&faces[0], '0');
            let mut gm = DWRITE_GLYPH_METRICS::default();
            faces[0].GetDesignGlyphMetrics(&zero, 1, &mut gm, false)?;

            Ok(Font {
                faces,
                formats,
                metrics,
                advance: gm.advanceWidth.max(1),
                size: FONT_SIZE,
                cache: RefCell::new(HashMap::new()),
            })
        }
    }

    /// Cell geometry at a DPI.
    pub fn cell(&self, dpi: u32) -> CellSize {
        let scale = dpi.max(96) as f32 / 96.0;
        let m = &self.metrics;
        let k = self.size / m.designUnitsPerEm as f32;
        let snap_round = |dip: f32| (dip * scale).round().max(1.0) / scale;
        let snap_up = |dip: f32| (dip * scale).ceil().max(1.0) / scale;

        let ascent = m.ascent as f32 * k;
        let descent = m.descent as f32 * k;
        let gap = (m.lineGap.max(0)) as f32 * k;
        let w = snap_round(self.advance as f32 * k);
        let h = snap_up(ascent + descent + gap);
        let baseline = snap_round(ascent + (h - ascent - descent) / 2.0);
        let stroke = snap_round((m.underlineThickness as f32 * k).max(1.0 / scale));
        CellSize {
            w,
            h,
            baseline,
            underline: snap_round(baseline - m.underlinePosition as f32 * k),
            strike: snap_round(baseline - m.strikethroughPosition as f32 * k),
            stroke,
        }
    }

    /// Glyph index for a character in a style, zero when the face lacks it.
    pub fn glyph(&self, c: char, style: u8) -> u16 {
        *self
            .cache
            .borrow_mut()
            .entry((c, style))
            .or_insert_with(|| glyph_index(&self.faces[style as usize & 3], c))
    }
}

fn glyph_index(face: &IDWriteFontFace, c: char) -> u16 {
    let code = c as u32;
    let mut index = 0u16;
    unsafe {
        let _ = face.GetGlyphIndices(&code, 1, &mut index);
    }
    index
}

/// One terminal window's render target.
pub struct GridTarget {
    rt: ID2D1HwndRenderTarget,
    brush: ID2D1SolidColorBrush,
}

impl GridTarget {
    pub fn new(gpu: &Gpu, hwnd: HWND, width_px: u32, height_px: u32, dpi: u32) -> Result<Self> {
        let rt = hwnd_target(gpu, hwnd, width_px, height_px, dpi)?;
        unsafe {
            // Cell backgrounds meet edge to edge. Antialiased, their shared
            // edges would blend into a visible line.
            rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
            let brush = rt.CreateSolidColorBrush(&color(Rgb { r: 0, g: 0, b: 0 }), None)?;
            Ok(GridTarget { rt, brush })
        }
    }

    pub fn resize(&self, width_px: u32, height_px: u32) -> Result<()> {
        crate::render::resize_target(&self.rt, width_px, height_px)
    }

    pub fn set_dpi(&self, dpi: u32) {
        unsafe { self.rt.SetDpi(dpi as f32, dpi as f32) }
    }

    /// Draws a frame. `Err` means the target must be recreated.
    pub fn draw(&self, font: &Font, cell: &CellSize, frame: &Frame) -> Result<()> {
        let x = |col: usize| PAD + col as f32 * cell.w;
        let y = |row: usize| PAD + row as f32 * cell.h;
        let rect = |row: usize, col: usize, cells: usize| D2D_RECT_F {
            left: x(col),
            top: y(row),
            right: x(col + cells),
            bottom: y(row) + cell.h,
        };
        unsafe {
            self.rt.BeginDraw();
            self.rt.Clear(Some(&color(frame.background)));

            for f in &frame.fills {
                self.brush.SetColor(&color(f.color));
                self.rt
                    .FillRectangle(&rect(f.row, f.col, f.cells), &self.brush);
            }

            let mut advances: Vec<f32> = Vec::new();
            for r in &frame.runs {
                advances.clear();
                advances.resize(r.glyphs.len(), cell.w);
                let run = DWRITE_GLYPH_RUN {
                    fontFace: ManuallyDrop::new(Some(font.faces[r.style as usize & 3].clone())),
                    fontEmSize: font.size,
                    glyphCount: r.glyphs.len() as u32,
                    glyphIndices: r.glyphs.as_ptr(),
                    glyphAdvances: advances.as_ptr(),
                    glyphOffsets: std::ptr::null(),
                    isSideways: false.into(),
                    bidiLevel: 0,
                };
                self.brush.SetColor(&color(r.color));
                self.rt.DrawGlyphRun(
                    Vector2 {
                        X: x(r.col),
                        Y: y(r.row) + cell.baseline,
                    },
                    &run,
                    &self.brush,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
                let mut run = run;
                ManuallyDrop::drop(&mut run.fontFace);
            }

            for l in &frame.loose {
                let wide: Vec<u16> = l.text.encode_utf16().collect();
                let fmt = &font.formats[l.style as usize & 3];
                // Colour glyphs only for wide characters, which is where
                // emoji presentation lives. A one cell symbol such as Claude
                // Code's bullet has to keep the colour the program gave it.
                let options = if l.cells == 2 {
                    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT
                } else {
                    D2D1_DRAW_TEXT_OPTIONS_NONE
                };
                self.brush.SetColor(&color(l.color));
                self.rt.DrawText(
                    &wide,
                    fmt,
                    &rect(l.row, l.col, l.cells),
                    &self.brush,
                    options,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }

            for s in &frame.strokes {
                let top = y(s.row)
                    + match s.kind {
                        Decoration::Underline => cell.underline,
                        Decoration::Strike => cell.strike,
                    };
                self.brush.SetColor(&color(s.color));
                self.rt.FillRectangle(
                    &D2D_RECT_F {
                        left: x(s.col),
                        top,
                        right: x(s.col + s.cells),
                        bottom: top + cell.stroke,
                    },
                    &self.brush,
                );
            }

            if let Some(c) = &frame.caret {
                let r = rect(c.row, c.col, c.cells);
                let bar = cell.stroke.max(cell.w / 8.0);
                self.brush.SetColor(&color(c.color));
                match c.shape {
                    CursorShape::Beam => self.rt.FillRectangle(
                        &D2D_RECT_F {
                            right: r.left + bar,
                            ..r
                        },
                        &self.brush,
                    ),
                    CursorShape::Underline => self.rt.FillRectangle(
                        &D2D_RECT_F {
                            top: r.bottom - bar,
                            ..r
                        },
                        &self.brush,
                    ),
                    CursorShape::Hidden => {}
                    CursorShape::Block | CursorShape::HollowBlock => {
                        let half = cell.stroke / 2.0;
                        let inner = D2D_RECT_F {
                            left: r.left + half,
                            top: r.top + half,
                            right: r.right - half,
                            bottom: r.bottom - half,
                        };
                        self.rt
                            .DrawRectangle(&inner, &self.brush, cell.stroke, None);
                    }
                }
            }

            self.rt.EndDraw(None, None)
        }
    }
}

// Styles index the face arrays directly.
const _: () = assert!(BOLD | ITALIC == 3);

fn color(c: Rgb) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: c.r as f32 / 255.0,
        g: c.g as f32 / 255.0,
        b: c.b as f32 / 255.0,
        a: 1.0,
    }
}
