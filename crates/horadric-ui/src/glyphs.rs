//! Draws a terminal [`Frame`] with Direct2D glyph runs.
//!
//! One monospace font, four faces. Every glyph in a run is given the cell
//! width as its advance, so text lands exactly on the grid no matter what the
//! font's own advances say. Characters the font lacks go through DirectWrite
//! font fallback one at a time, each pinned to its own cell.

use std::cell::{Cell, RefCell};
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
use crate::render::{self, hwnd_target, Gpu};
use crate::theme::{self, Color};

/// Cascadia ships with Windows 11. Consolas is on every Windows since Vista.
const FAMILIES: [PCWSTR; 2] = [w!("Cascadia Mono"), w!("Consolas")];

/// Space between the grid and the window edge, in DIPs.
pub const PAD: f32 = 6.0;

/// Height of a pane's header, in DIPs.
pub const HEADER_H: f32 = 26.0;

/// The strip above a pane's grid when the stage shows more than one: which
/// session it is, and the handle it is dragged by.
pub struct Header<'a> {
    pub name: &'a str,
    /// What the agent says it is doing, dimmer, after the name.
    pub detail: &'a str,
    /// The session's phase, as the colour of its dot and of the line along
    /// the top. None for a session doing nothing, which gets neither.
    pub phase: Option<Color>,
    /// The project's colour, under the header of the pane with the keyboard.
    pub accent: Color,
    /// Has the keyboard.
    pub active: bool,
    /// Being dragged to another place in the grid.
    pub lifted: bool,
    /// Ends in a cross that closes it, a square [`HEADER_H`] wide.
    pub close: bool,
    /// Has the button that zooms it, a square left of the cross, and
    /// whether it is zoomed now.
    pub zoom: Option<bool>,
}

/// Where a header's buttons start, from its left, in a pane `width` DIPs
/// wide: the zoom button's, then the cross's.
pub fn header_buttons(width: f32, zoom: bool, close: bool) -> (Option<f32>, Option<f32>) {
    let close_at = close.then_some(width - HEADER_H);
    let zoom_at = zoom.then_some(close_at.unwrap_or(width) - HEADER_H);
    (zoom_at, close_at)
}

/// The search bar over a pane's top right corner, while it is open.
pub struct FindBar<'a> {
    pub query: &'a str,
    /// After the query, dimmer: "No match", or a hint while it is empty.
    pub status: &'a str,
}

/// The search bar's size in DIPs, and its distance from the edges.
const FIND_W: f32 = 320.0;
const FIND_H: f32 = 30.0;
const FIND_INSET: f32 = 8.0;

/// Behind a pane's header: the clay of a tile, so the header reads as the
/// label on the dark well under it.
const HEADER_BG: Color = theme::SURFACE;

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
    /// For the characters drawn one at a time. They carry the size, so a
    /// new size makes new ones.
    formats: RefCell<[IDWriteTextFormat; 4]>,
    family: PCWSTR,
    dw: IDWriteFactory,
    metrics: DWRITE_FONT_METRICS,
    /// Advance of `0` in design units. Monospace, so every glyph's.
    advance: u32,
    /// In DIPs, the same for every pane.
    size: Cell<f32>,
    cache: RefCell<HashMap<(char, u8), u16>>,
}

const VARIANTS: [(DWRITE_FONT_WEIGHT, DWRITE_FONT_STYLE); 4] = [
    (DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL),
    (DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_STYLE_NORMAL),
    (DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_ITALIC),
    (DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_STYLE_ITALIC),
];

fn formats(dw: &IDWriteFactory, family: PCWSTR, size: f32) -> Result<[IDWriteTextFormat; 4]> {
    let format = |(weight, style): (DWRITE_FONT_WEIGHT, DWRITE_FONT_STYLE)| unsafe {
        let f = dw.CreateTextFormat(
            family,
            None,
            weight,
            style,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("en-us"),
        )?;
        f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        Ok::<_, windows::core::Error>(f)
    };
    Ok([
        format(VARIANTS[0])?,
        format(VARIANTS[1])?,
        format(VARIANTS[2])?,
        format(VARIANTS[3])?,
    ])
}

impl Font {
    pub fn new(dw: &IDWriteFactory, size: f32) -> Result<Font> {
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

            let face = |(weight, style): (DWRITE_FONT_WEIGHT, DWRITE_FONT_STYLE)| {
                family
                    .GetFirstMatchingFont(weight, DWRITE_FONT_STRETCH_NORMAL, style)?
                    .CreateFontFace()
            };
            let faces = [
                face(VARIANTS[0])?,
                face(VARIANTS[1])?,
                face(VARIANTS[2])?,
                face(VARIANTS[3])?,
            ];

            let mut metrics = DWRITE_FONT_METRICS::default();
            faces[0].GetMetrics(&mut metrics);
            let zero = glyph_index(&faces[0], '0');
            let mut gm = DWRITE_GLYPH_METRICS::default();
            faces[0].GetDesignGlyphMetrics(&zero, 1, &mut gm, false)?;

            Ok(Font {
                faces,
                formats: RefCell::new(formats(dw, family_name, size)?),
                family: family_name,
                dw: dw.clone(),
                metrics,
                advance: gm.advanceWidth.max(1),
                size: Cell::new(size),
                cache: RefCell::new(HashMap::new()),
            })
        }
    }

    pub fn size(&self) -> f32 {
        self.size.get()
    }

    /// Changes the size for every pane. They have to fit their grids again.
    pub fn set_size(&self, size: f32) -> Result<()> {
        *self.formats.borrow_mut() = formats(&self.dw, self.family, size)?;
        self.size.set(size);
        Ok(())
    }

    /// Cell geometry at a DPI.
    pub fn cell(&self, dpi: u32) -> CellSize {
        let scale = dpi.max(96) as f32 / 96.0;
        let m = &self.metrics;
        let k = self.size.get() / m.designUnitsPerEm as f32;
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

    /// Draws a frame, below `header` when there is one, and the search bar
    /// over it when open. With `drop`, the pane is where a dragged one
    /// would land. `veil` from 0 to 1 lays the background over everything:
    /// a pane without the keyboard steps back, a pane just shown fades in.
    /// `Err` means the target must be recreated.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        gpu: &Gpu,
        font: &Font,
        cell: &CellSize,
        frame: &Frame,
        header: Option<&Header>,
        find: Option<&FindBar>,
        drop: bool,
        veil: f32,
    ) -> Result<()> {
        let top = if header.is_some() { HEADER_H } else { 0.0 };
        let x = |col: usize| PAD + col as f32 * cell.w;
        let y = |row: usize| top + PAD + row as f32 * cell.h;
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
                    fontEmSize: font.size.get(),
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

            let formats = font.formats.borrow();
            for l in &frame.loose {
                let wide: Vec<u16> = l.text.encode_utf16().collect();
                let fmt = &formats[l.style as usize & 3];
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

            if let Some(h) = header {
                self.header(gpu, h);
            }
            if let Some(f) = find {
                self.find_bar(gpu, f, top);
            }
            if veil > 0.0 {
                let size = self.rt.GetSize();
                let bg = frame.background;
                self.brush.SetColor(&D2D1_COLOR_F {
                    a: veil.min(1.0),
                    ..color(bg)
                });
                // The header stays clay: a dark veil over it would read as
                // dirt, and its dimmer name already says it is not in use.
                let top = if header.is_some() { HEADER_H } else { 0.0 };
                self.rt.FillRectangle(
                    &D2D_RECT_F {
                        left: 0.0,
                        top,
                        right: size.width,
                        bottom: size.height,
                    },
                    &self.brush,
                );
            }
            if drop {
                let size = self.rt.GetSize();
                let all = D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: size.width,
                    bottom: size.height,
                };
                self.brush
                    .SetColor(&render::color(theme::WORKING.with_alpha(0.12)));
                self.rt.FillRectangle(&all, &self.brush);
                let inset = D2D_RECT_F {
                    left: 1.0,
                    top: 1.0,
                    right: size.width - 1.0,
                    bottom: size.height - 1.0,
                };
                self.brush.SetColor(&render::color(theme::WORKING));
                self.rt.DrawRectangle(&inset, &self.brush, 2.0, None);
            }

            self.rt.EndDraw(None, None)
        }
    }

    unsafe fn header(&self, gpu: &Gpu, h: &Header) {
        let width = self.rt.GetSize().width;
        let bar = |c: Color, top: f32, bottom: f32| {
            self.brush.SetColor(&render::color(c));
            self.rt.FillRectangle(
                &D2D_RECT_F {
                    left: 0.0,
                    top,
                    right: width,
                    bottom,
                },
                &self.brush,
            );
        };
        let bg = if h.lifted {
            HEADER_BG.mix(theme::WORKING, 0.35)
        } else {
            HEADER_BG
        };
        bar(bg, 0.0, HEADER_H);
        // The phase, as a line of light along the top, as on a tile's edge.
        // It is the only mark of the phase here, so it is strong enough to
        // read from across the room.
        if let (Some(c), false) = (h.phase, h.lifted) {
            bar(c.with_alpha(0.1), 0.0, HEADER_H);
            bar(c.with_alpha(0.85), 0.0, 2.0);
        }
        // The project's colour under the pane that has the keyboard: the
        // same colour the stage's edge and the cluster's wash have.
        if h.active && !h.lifted {
            bar(h.accent.with_alpha(0.8), HEADER_H - 2.0, HEADER_H);
        } else {
            bar(theme::RIM_SHADE, HEADER_H - 1.0, HEADER_H);
        }

        let left = 10.0;
        let (zoom_at, close_at) = header_buttons(width, h.zoom.is_some(), h.close);
        let right = zoom_at.or(close_at).unwrap_or(width - 8.0);
        let button = |glyph: &str, at: f32| {
            let glyph: Vec<u16> = glyph.encode_utf16().collect();
            self.brush.SetColor(&render::color(theme::TEXT_DIM));
            self.rt.DrawText(
                &glyph,
                &gpu.icon_small,
                &D2D_RECT_F {
                    left: at,
                    top: 0.0,
                    right: at + HEADER_H,
                    bottom: HEADER_H,
                },
                &self.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        };
        if let Some(at) = close_at {
            button("\u{E711}", at);
        }
        // Full screen to zoom in, back to window to zoom out.
        if let (Some(at), Some(zoomed)) = (zoom_at, h.zoom) {
            button(if zoomed { "\u{E73F}" } else { "\u{E740}" }, at);
        }
        let name: Vec<u16> = h.name.encode_utf16().collect();
        let name_w = gpu
            .dw
            .CreateTextLayout(&name, &gpu.title, (right - left).max(0.0), HEADER_H)
            .and_then(|l| {
                let mut m = Default::default();
                l.GetMetrics(&mut m)
                    .map(|_| m.widthIncludingTrailingWhitespace)
            })
            .unwrap_or(right - left);
        let text = if h.active {
            theme::TEXT
        } else {
            theme::TEXT_DIM
        };
        self.brush.SetColor(&render::color(text));
        self.rt.DrawText(
            &name,
            &gpu.title,
            &D2D_RECT_F {
                left,
                top: 0.0,
                right,
                bottom: HEADER_H,
            },
            &self.brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
        let detail_left = left + name_w + 10.0;
        if !h.detail.is_empty() && detail_left < right {
            let detail: Vec<u16> = h.detail.encode_utf16().collect();
            self.brush.SetColor(&render::color(theme::TEXT_DIM));
            self.rt.DrawText(
                &detail,
                &gpu.small,
                &D2D_RECT_F {
                    left: detail_left,
                    top: 0.0,
                    right,
                    bottom: HEADER_H,
                },
                &self.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    /// The search bar, in the top right corner below the header at `top`:
    /// a magnifier, the query with a caret after it, and what was found.
    unsafe fn find_bar(&self, gpu: &Gpu, f: &FindBar, top: f32) {
        let width = self.rt.GetSize().width;
        let bar = D2D_RECT_F {
            left: (width - FIND_INSET - FIND_W).max(FIND_INSET),
            top: top + FIND_INSET,
            right: width - FIND_INSET,
            bottom: top + FIND_INSET + FIND_H,
        };
        self.brush
            .SetColor(&render::color(HEADER_BG.mix(theme::WORKING, 0.12)));
        self.rt.FillRectangle(&bar, &self.brush);
        self.brush
            .SetColor(&render::color(theme::WORKING.with_alpha(0.6)));
        let edge = D2D_RECT_F {
            left: bar.left + 0.5,
            top: bar.top + 0.5,
            right: bar.right - 0.5,
            bottom: bar.bottom - 0.5,
        };
        self.rt.DrawRectangle(&edge, &self.brush, 1.0, None);

        let text = |s: &str, format: &IDWriteTextFormat, color: Color, left: f32, right: f32| {
            let wide: Vec<u16> = s.encode_utf16().collect();
            self.brush.SetColor(&render::color(color));
            self.rt.DrawText(
                &wide,
                format,
                &D2D_RECT_F {
                    left,
                    top: bar.top,
                    right: right.max(left),
                    bottom: bar.bottom,
                },
                &self.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        };
        text(
            "\u{E721}",
            &gpu.icon_small,
            theme::TEXT_DIM,
            bar.left,
            bar.left + FIND_H,
        );
        let left = bar.left + FIND_H;
        let right = bar.right - 10.0;
        text(f.status, &gpu.small_right, theme::TEXT_DIM, left, right);
        text(f.query, &gpu.body, theme::TEXT, left, right);
        // The caret after the query, where the next character goes.
        let wide: Vec<u16> = f.query.encode_utf16().collect();
        let query_w = gpu
            .dw
            .CreateTextLayout(&wide, &gpu.body, (right - left).max(0.0), FIND_H)
            .and_then(|l| {
                let mut m = Default::default();
                l.GetMetrics(&mut m)
                    .map(|_| m.widthIncludingTrailingWhitespace)
            })
            .unwrap_or(0.0);
        let x = (left + query_w + 1.0).min(right);
        self.brush.SetColor(&render::color(theme::TEXT));
        self.rt.FillRectangle(
            &D2D_RECT_F {
                left: x,
                top: bar.top + 7.0,
                right: x + 1.5,
                bottom: bar.bottom - 7.0,
            },
            &self.brush,
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_buttons_sit_at_the_end_zoom_first() {
        assert_eq!(header_buttons(400.0, false, false), (None, None));
        assert_eq!(
            header_buttons(400.0, false, true),
            (None, Some(400.0 - HEADER_H))
        );
        assert_eq!(
            header_buttons(400.0, true, false),
            (Some(400.0 - HEADER_H), None)
        );
        assert_eq!(
            header_buttons(400.0, true, true),
            (Some(400.0 - 2.0 * HEADER_H), Some(400.0 - HEADER_H))
        );
    }
}
