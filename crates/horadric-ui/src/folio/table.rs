//! A markdown table in a reply.

use horadric_core::markdown::{Align, Inline};
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_SEMI_BOLD;

use super::flow;
use super::rich::{range, Ctx, Scene};
use crate::layout::Rect;
use crate::render;

/// A table cell's padding.
const CELL_X: f32 = 10.0;
const CELL_Y: f32 = 6.0;
/// The narrowest a table column gets.
const CELL_LEAST: f32 = 64.0;

impl Ctx<'_> {
    /// A table: columns as wide as their widest cell, the wide ones
    /// wrapping when the table does not fit.
    pub(super) unsafe fn table(
        &self,
        header: &[Inline],
        align: &[Align],
        rows: &[Vec<Inline>],
        w: f32,
    ) -> Scene {
        let mut s = Scene::default();
        let cols = header.len();
        if cols == 0 {
            return s;
        }
        let all = || std::iter::once(header).chain(rows.iter().map(Vec::as_slice));
        let mut natural = vec![0.0f32; cols];
        for row in all() {
            for (c, cell) in row.iter().enumerate().take(cols) {
                if let Some(l) = self.one_line(&self.fonts.body, &cell.text) {
                    let bold = row.as_ptr() == header.as_ptr();
                    if bold {
                        let _ =
                            l.SetFontWeight(DWRITE_FONT_WEIGHT_SEMI_BOLD, range(0..u32::MAX / 2));
                    }
                    natural[c] = natural[c].max(render::text_size(&l).0 + 2.0 * CELL_X + 2.0);
                }
            }
        }
        let widths = flow::table_widths(&natural, CELL_LEAST, w);
        let mut y = 0.0;
        let total_w: f32 = widths.iter().sum();
        for (k, row) in all().enumerate() {
            let head = k == 0;
            let mut cell_scene = Scene::default();
            let mut rh: f32 = 0.0;
            let mut x = 0.0;
            for (c, cell) in row.iter().enumerate().take(cols) {
                let cw = widths[c];
                let ink = if head {
                    self.pal.fg
                } else {
                    self.pal.fg.with_alpha(0.88)
                };
                let a = align.get(c).copied().unwrap_or(Align::None);
                let mark = cell_scene.units.len();
                let (h, _) = self.inline(
                    &mut cell_scene,
                    cell,
                    &self.fonts.body,
                    x + CELL_X,
                    y + CELL_Y,
                    cw - 2.0 * CELL_X,
                    ink,
                    a,
                );
                if head {
                    if let Some(u) = cell_scene.units.get(mark) {
                        let _ = u.layout.SetFontWeight(
                            DWRITE_FONT_WEIGHT_SEMI_BOLD,
                            range(0..u.text.encode_utf16().count() as u32),
                        );
                    }
                }
                rh = rh.max(h + 2.0 * CELL_Y);
                x += cw;
            }
            if head {
                s.fill(
                    Rect::new(0.0, y, total_w, rh),
                    0.0,
                    self.pal.fg.with_alpha(0.06),
                );
            } else if k % 2 == 0 {
                s.fill(
                    Rect::new(0.0, y, total_w, rh),
                    0.0,
                    self.pal.fg.with_alpha(0.025),
                );
            }
            s.append(&cell_scene, 0.0, 0.0);
            y += rh;
            s.fill(
                Rect::new(0.0, y - 1.0, total_w, 1.0),
                0.0,
                self.pal.fg.with_alpha(0.09),
            );
        }
        s.stroke(
            Rect::new(0.0, 0.0, total_w, y),
            6.0,
            self.pal.fg.with_alpha(0.14),
            1.0,
        );
        s.h = y + 4.0;
        s
    }
}
