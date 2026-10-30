//! A browser tab with no page: a field to search or type an address in,
//! and the pages its project went to lately, drawn by Horadric on the
//! pane's glass while the tab's WebView stays hidden. The one web view
//! shows web pages, never Horadric, so this is not a page.
//!
//! The field is the pane's address field, moved: the same keys, the same
//! suggestions while something is typed. The arrow keys walk the list
//! while nothing is.

use std::rc::Rc;

use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{
    ID2D1HwndRenderTarget, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_ALIASED,
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteTextLayout, DWRITE_MEASURING_MODE_NATURAL, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows_numerics::Vector2;

use crate::favicon::{self, Favicon};
use crate::glyphs::{bar_scroll, BarEdit};
use crate::render::{self, Gpu};
use crate::theme::{self, Color};

/// The most pages listed.
pub const ROWS: usize = 8;

/// Shown in the empty field.
const PLACEHOLDER: &str = "Search Google or type an address";

const FIELD_W: f32 = 560.0;
const FIELD_H: f32 = 38.0;
/// Room left of the text for the magnifier, and right of it.
const TEXT_LEFT: f32 = 38.0;
const TEXT_RIGHT: f32 = 16.0;
const ROW_H: f32 = 34.0;
const ICON: f32 = 16.0;
/// Between the field and the first page.
const LIST_GAP: f32 = 26.0;

/// Where the start page puts its parts, in DIPs.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub field: [f32; 4],
    pub rows: Vec<[f32; 4]>,
}

/// The field a little above the middle of `glass`, as wide as it allows,
/// and under it as many of `count` pages as there is room for.
pub fn layout(glass: [f32; 4], count: usize) -> Layout {
    let [left, top, right, bottom] = glass;
    let w = FIELD_W.min(right - left - 48.0).max(0.0);
    let mid = (left + right) / 2.0;
    let field_top = top + ((bottom - top) * 0.26).max(56.0);
    let field = [mid - w / 2.0, field_top, mid + w / 2.0, field_top + FIELD_H];
    let first = field[3] + LIST_GAP;
    let room = ((bottom - 16.0 - first) / ROW_H).floor().max(0.0) as usize;
    let rows = (0..count.min(room).min(ROWS))
        .map(|i| {
            let t = first + i as f32 * ROW_H;
            [field[0], t, field[2], t + ROW_H]
        })
        .collect();
    Layout { field, rows }
}

/// The page listed under `(x, y)`.
pub fn row_at(l: &Layout, x: f32, y: f32) -> Option<usize> {
    l.rows.iter().position(|r| inside(r, x, y))
}

pub fn in_field(l: &Layout, x: f32, y: f32) -> bool {
    inside(&l.field, x, y)
}

fn inside(r: &[f32; 4], x: f32, y: f32) -> bool {
    x >= r[0] && x < r[2] && y >= r[1] && y < r[3]
}

/// How wide the field's text is laid out, for drawing and for clicks.
pub fn text_view(l: &Layout) -> f32 {
    (l.field[2] - l.field[0] - TEXT_LEFT - TEXT_RIGHT).max(0.0)
}

/// Where the field's text starts, from the pane's left.
pub fn text_left(l: &Layout) -> f32 {
    l.field[0] + TEXT_LEFT
}

/// The field's text laid out on one line.
pub fn text_layout(gpu: &Gpu, text: &str) -> windows::core::Result<IDWriteTextLayout> {
    let wide: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        let l = gpu
            .dw
            .CreateTextLayout(&wide, &gpu.body, 100_000.0, FIELD_H)?;
        l.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        Ok(l)
    }
}

/// The field's text offset under `x`, DIPs from the pane's left, with
/// the text scrolled to show `caret`.
pub fn offset_at(gpu: &Gpu, l: &Layout, text: &str, caret: u32, x: f32) -> u32 {
    let Ok(t) = text_layout(gpu, text) else {
        return 0;
    };
    let scroll = bar_scroll(render::caret_at(&t, caret).x, text_view(l));
    render::offset_at(&t, x - text_left(l) + scroll, FIELD_H / 2.0)
}

/// A page in the list.
pub struct Row {
    pub title: String,
    pub host: String,
    pub icon: Option<Rc<Favicon>>,
}

impl Row {
    pub fn new(title: &str, url: &str) -> Row {
        let host = horadric_core::history::bare(url);
        let host = horadric_core::history::host(host).to_string();
        let title = match title.trim() {
            "" => horadric_core::history::bare(url)
                .trim_end_matches('/')
                .to_string(),
            t => t.to_string(),
        };
        Row {
            title,
            host,
            icon: favicon::of(url),
        }
    }
}

/// Everything the start page is drawn from.
pub struct Start<'a> {
    pub layout: Layout,
    pub text: &'a str,
    /// While the field has the keyboard.
    pub edit: Option<BarEdit>,
    pub rows: &'a [Row],
    /// The page under the mouse or the arrow keys.
    pub lit: Option<usize>,
    pub accent: Color,
}

fn rect(r: [f32; 4]) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r[0],
        top: r[1],
        right: r[2],
        bottom: r[3],
    }
}

fn rounded(r: D2D_RECT_F, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT {
        rect: r,
        radiusX: radius,
        radiusY: radius,
    }
}

/// Draws the start page on the glass.
///
/// # Safety
/// Between the target's BeginDraw and EndDraw.
pub unsafe fn draw(rt: &ID2D1HwndRenderTarget, brush: &ID2D1SolidColorBrush, gpu: &Gpu, s: &Start) {
    let l = &s.layout;
    if l.field[2] - l.field[0] < TEXT_LEFT + TEXT_RIGHT {
        return;
    }
    rune(
        rt,
        brush,
        (l.field[0] + l.field[2]) / 2.0,
        l.field[1] - 30.0,
        s.accent,
    );
    field(rt, brush, gpu, s);
    for (i, (row, at)) in s.rows.iter().zip(&l.rows).enumerate() {
        page(rt, brush, gpu, row, *at, s.lit == Some(i));
    }
}

/// A quiet mark over the field: a rune cut in the glass, a diamond with a
/// stroke through it and the project's colour at its heart.
unsafe fn rune(
    rt: &ID2D1HwndRenderTarget,
    brush: &ID2D1SolidColorBrush,
    x: f32,
    y: f32,
    accent: Color,
) {
    let ink = theme::text_dim().with_alpha(0.45);
    brush.SetColor(&render::color(ink));
    let (w, h) = (7.0, 11.0);
    let p = |dx: f32, dy: f32| Vector2 {
        X: x + dx,
        Y: y + dy,
    };
    let corners = [p(0.0, -h), p(w, 0.0), p(0.0, h), p(-w, 0.0)];
    for i in 0..4 {
        rt.DrawLine(corners[i], corners[(i + 1) % 4], brush, 1.0, None);
    }
    rt.DrawLine(p(0.0, -h - 5.0), p(0.0, h + 5.0), brush, 1.0, None);
    brush.SetColor(&render::color(accent.with_alpha(0.7)));
    rt.FillEllipse(
        &D2D1_ELLIPSE {
            point: p(0.0, 0.0),
            radiusX: 1.8,
            radiusY: 1.8,
        },
        brush,
    );
}

/// The field sunk in the glass, lit while it has the keyboard, the
/// magnifier at its left and the text with its caret and selection.
unsafe fn field(rt: &ID2D1HwndRenderTarget, brush: &ID2D1SolidColorBrush, gpu: &Gpu, s: &Start) {
    let l = &s.layout;
    let f = rect(l.field);
    let radius = FIELD_H / 2.0;
    brush.SetColor(&render::color(theme::well()));
    rt.FillRoundedRectangle(&rounded(f, radius), brush);
    let rim = if s.edit.is_some() {
        theme::working().with_alpha(0.55)
    } else {
        theme::engrave_dark()
    };
    let edge = D2D_RECT_F {
        left: f.left + 0.5,
        top: f.top + 0.5,
        right: f.right - 0.5,
        bottom: f.bottom - 0.5,
    };
    brush.SetColor(&render::color(rim));
    rt.DrawRoundedRectangle(&rounded(edge, radius - 0.5), brush, 1.0, None);

    let glyph: Vec<u16> = "\u{E721}".encode_utf16().collect();
    brush.SetColor(&render::color(theme::text_dim()));
    rt.DrawText(
        &glyph,
        &gpu.icon,
        &D2D_RECT_F {
            left: f.left + 6.0,
            top: f.top,
            right: f.left + TEXT_LEFT - 4.0,
            bottom: f.bottom,
        },
        brush,
        D2D1_DRAW_TEXT_OPTIONS_NONE,
        DWRITE_MEASURING_MODE_NATURAL,
    );

    let left = text_left(l);
    let view = text_view(l);
    rt.PushAxisAlignedClip(
        &D2D_RECT_F {
            left: left - 1.0,
            top: f.top,
            right: left + view + 1.0,
            bottom: f.bottom,
        },
        D2D1_ANTIALIAS_MODE_ALIASED,
    );
    let placeholder = s.text.is_empty();
    let shown = if placeholder { PLACEHOLDER } else { s.text };
    if let Ok(layout) = text_layout(gpu, shown) {
        let edit = s.edit.filter(|_| !placeholder);
        let caret = edit.map(|e| render::caret_at(&layout, e.caret));
        let x = left - caret.map_or(0.0, |c| bar_scroll(c.x, view));
        if let Some(e) = edit {
            let (a, b) = e.selection;
            brush.SetColor(&render::color(theme::working().with_alpha(0.4)));
            for r in render::range_rects(&layout, a, b - a) {
                rt.FillRectangle(
                    &D2D_RECT_F {
                        left: x + r.x,
                        top: f.top + r.y,
                        right: x + r.x + r.w.max(3.0),
                        bottom: f.top + r.y + r.h,
                    },
                    brush,
                );
            }
        }
        let ink = if placeholder {
            theme::text_dim().with_alpha(0.55)
        } else {
            theme::text()
        };
        brush.SetColor(&render::color(ink));
        rt.DrawTextLayout(
            Vector2 { X: x, Y: f.top },
            &layout,
            brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
        let caret = match (s.edit, caret) {
            (Some(_), Some(c)) => Some(x + c.x),
            (Some(_), None) => Some(left),
            _ => None,
        };
        if let Some(cx) = caret {
            brush.SetColor(&render::color(theme::text()));
            rt.FillRectangle(
                &D2D_RECT_F {
                    left: cx.round() - 0.5,
                    top: f.top + 10.0,
                    right: cx.round() + 1.0,
                    bottom: f.bottom - 10.0,
                },
                brush,
            );
        }
    }
    rt.PopAxisAlignedClip();
}

/// A page in the list: its site's picture, or a dot where none is kept,
/// its title, and its host dimmer at the right.
unsafe fn page(
    rt: &ID2D1HwndRenderTarget,
    brush: &ID2D1SolidColorBrush,
    gpu: &Gpu,
    row: &Row,
    at: [f32; 4],
    lit: bool,
) {
    let r = rect(at);
    if lit {
        brush.SetColor(&render::color(theme::well().with_alpha(0.8)));
        rt.FillRoundedRectangle(&rounded(r, 6.0), brush);
    }
    let mid = (r.top + r.bottom) / 2.0;
    let icon_left = r.left + 14.0;
    match &row.icon {
        Some(icon) => favicon::draw(
            rt,
            icon,
            D2D_RECT_F {
                left: icon_left,
                top: mid - ICON / 2.0,
                right: icon_left + ICON,
                bottom: mid + ICON / 2.0,
            },
        ),
        None => {
            brush.SetColor(&render::color(theme::text_dim().with_alpha(0.5)));
            rt.FillEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2 {
                        X: icon_left + ICON / 2.0,
                        Y: mid,
                    },
                    radiusX: 2.5,
                    radiusY: 2.5,
                },
                brush,
            );
        }
    }
    let text_left = icon_left + ICON + 12.0;
    let host_left = text_left + (r.right - text_left) * 0.62;
    let text = |s: &str, format, left: f32, right: f32, ink: Color| {
        let wide: Vec<u16> = s.encode_utf16().collect();
        brush.SetColor(&render::color(ink));
        rt.DrawText(
            &wide,
            format,
            &D2D_RECT_F {
                left,
                top: r.top,
                right: right.max(left),
                bottom: r.bottom,
            },
            brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    };
    let title_ink = if lit {
        theme::text()
    } else {
        theme::text().mix(theme::text_dim(), 0.3)
    };
    text(
        &row.title,
        &gpu.body,
        text_left,
        host_left - 12.0,
        title_ink,
    );
    text(
        &row.host,
        &gpu.small_right,
        host_left,
        r.right - 14.0,
        theme::text_dim(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_fits_under_the_field() {
        let l = layout([0.0, 0.0, 800.0, 600.0], 20);
        assert_eq!(l.field[2] - l.field[0], FIELD_W);
        assert_eq!(l.rows.len(), ROWS);
        let short = layout([0.0, 0.0, 800.0, 260.0], 20);
        assert!(short.rows.len() < ROWS);
        assert!(short.rows.iter().all(|r| r[3] <= 260.0));
        // A narrow pane narrows the field.
        let narrow = layout([0.0, 0.0, 300.0, 600.0], 2);
        assert_eq!(narrow.field[2] - narrow.field[0], 252.0);
        assert_eq!(narrow.rows.len(), 2);
    }

    #[test]
    fn a_click_finds_its_row() {
        let l = layout([0.0, 0.0, 800.0, 600.0], 3);
        let [x, y] = [l.rows[1][0] + 5.0, l.rows[1][1] + 1.0];
        assert_eq!(row_at(&l, x, y), Some(1));
        assert_eq!(row_at(&l, x, l.field[1] + 1.0), None);
        assert!(in_field(&l, x, l.field[1] + 1.0));
    }
}
