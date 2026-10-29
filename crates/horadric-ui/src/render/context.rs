//! The Context window: its header on the plate, the totals under it, and
//! the list on a screen sunk into it, a project's lines with their
//! switches or every project in the overview. What each row says is
//! worked out by `crate::contextpanel`; this only draws it.

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE;

use super::{rect, Gpu, Painter, Target};
use crate::contextpanel::{Col, ContextHit, ContextLayout, ContextRow, OverviewRow, ROW_H};
use crate::layout::{self, Button, Metrics, Rect};
use crate::theme::{self, Color};

/// Everything one frame of the Context window needs.
pub struct ContextScene<'a> {
    pub layout: &'a ContextLayout,
    /// The project's name, or "All projects".
    pub title: &'a str,
    pub overview: bool,
    pub total: &'a str,
    /// What the totals were measured from, or why a switch failed.
    pub measured: &'a str,
    pub failed: bool,
    pub note: &'a str,
    pub rows: &'a [ContextRow],
    pub overview_rows: &'a [OverviewRow],
    /// What an empty list says.
    pub empty: &'a str,
    pub hot: ContextHit,
    pub pressed: Option<ContextHit>,
    pub scroll: f32,
    pub active: bool,
}

/// Layers, in Segoe Fluent Icons: what a turn is stacked from.
const LAYERS: char = '\u{E81E}';

impl Target {
    /// Draws the Context window. `Err` means the target must be recreated.
    pub fn draw_context(&self, gpu: &Gpu, m: &Metrics, scene: &ContextScene) -> Result<()> {
        unsafe {
            self.rt.BeginDraw();
            self.painter(&self.rt).context(gpu, m, scene);
            self.rt.EndDraw(None, None)
        }
    }
}

impl Painter<'_> {
    unsafe fn context(&self, gpu: &Gpu, m: &Metrics, scene: &ContextScene) {
        let l = scene.layout;
        self.plate(m, l.size);
        self.context_header(gpu, scene);

        self.text(&gpu.body, theme::text(), scene.total, l.total);
        let said = if scene.failed {
            theme::error()
        } else {
            theme::text_dim()
        };
        self.text(&gpu.small, said, scene.measured, l.measured);
        if !scene.overview {
            self.text(&gpu.small, theme::legend(), scene.note, l.note);
        }

        self.screen(gpu, &l.list, m.tile_radius);
        let n = if scene.overview {
            scene.overview_rows.len()
        } else {
            scene.rows.len()
        };
        self.context_head(gpu, scene);
        if n == 0 {
            let r = Rect::new(l.rows.x + 16.0, l.rows.y + 8.0, l.rows.w - 32.0, 40.0);
            self.text(&gpu.small, theme::legend(), scene.empty, r);
            return;
        }
        self.rt
            .PushAxisAlignedClip(&rect(&l.rows), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        for i in l.visible(n, scene.scroll) {
            let band = l.row(i, scene.scroll);
            let back = band.inset(2.0);
            if scene.hot == ContextHit::Row(i) && scene.overview {
                self.fill_rounded(&back, 6.0, theme::hover_fill());
            } else if i % 2 == 1 {
                self.fill_rounded(&back, 6.0, theme::hover_fill().fade(0.35));
            }
            if scene.overview {
                self.overview_line(gpu, l, band, &scene.overview_rows[i]);
            } else {
                self.context_line(gpu, scene, i, band, &scene.rows[i]);
            }
        }
        self.rt.PopAxisAlignedClip();

        let total = n as f32 * ROW_H;
        if total > l.rows.h {
            let track = l.rows.h - 8.0;
            let thumb_h = (track * l.rows.h / total).max(16.0);
            let most = l.max_scroll(n).max(1.0);
            let y = l.rows.y + 4.0 + (track - thumb_h) * scene.scroll / most;
            let bar = Rect::new(l.rows.right() - 6.0, y, 3.0, thumb_h);
            self.fill_rounded(&bar, 1.5, theme::text_dim().with_alpha(0.5));
        }
    }

    /// "CONTEXT" over the project's name, the way back to every project,
    /// and the close key.
    unsafe fn context_header(&self, gpu: &Gpu, scene: &ContextScene) {
        let l = scene.layout;
        let quiet = |c: Color| if scene.active { c } else { c.fade(0.6) };
        let ink = quiet(theme::working().mix(theme::text_dim(), 0.35));
        let mark = Rect::new(l.label.x - 2.0, l.label.y, 16.0, l.label.h);
        self.icon(&gpu.icon_small, ink, LAYERS, mark);
        let label = Rect::new(mark.right() + 4.0, l.label.y, l.label.w - 20.0, l.label.h);
        self.text_spaced(gpu, &gpu.chip, ink, "CONTEXT", 1.6, label);
        self.text(&gpu.display, quiet(theme::text()), scene.title, l.project);

        if !scene.overview {
            let b = layout::button(ContextHit::Back, scene.hot, scene.pressed);
            let depth = match b {
                Button::Idle => 0.5,
                Button::Hover => 0.8,
                Button::Pressed => 0.2,
            };
            self.key(gpu, &l.back, 8.0, theme::surface(), depth, 1.0);
            let ink = if b == Button::Hover {
                theme::text()
            } else {
                theme::text_dim().mix(theme::text(), 0.4)
            };
            self.text(&gpu.small_centre, ink, "All projects", l.back);
        }

        let b = layout::button(ContextHit::Close, scene.hot, scene.pressed);
        let black = Color::rgb(0);
        let white = Color::rgb(0xFFFFFF);
        let (fill, ink) = match b {
            Button::Idle => (None, quiet(theme::text_dim())),
            Button::Hover => (Some(theme::error().mix(black, 0.15)), white),
            Button::Pressed => (Some(theme::error().mix(black, 0.35)), white.fade(0.8)),
        };
        if let Some(f) = fill {
            self.fill_rounded(&l.close, 7.0, f);
        }
        self.icon(&gpu.icon_small, ink, '\u{E8BB}', l.close);
    }

    /// The band naming the columns.
    unsafe fn context_head(&self, gpu: &Gpu, scene: &ContextScene) {
        let l = scene.layout;
        let h = l.head;
        let at = |c: Col| Rect::new(c.x, h.y, c.w, h.h);
        let c = theme::legend();
        if scene.overview {
            self.text_spaced(gpu, &gpu.chip, c, "PROJECT", 1.2, at(l.o_name));
            self.head_right(gpu, c, "PER TURN", l.o_before, h);
            self.head_right(gpu, c, "SWITCHED", l.o_after, h);
            self.head_right(gpu, c, "UNUSED", l.o_idle, h);
            return;
        }
        self.head_right(gpu, c, "TOKENS", l.tokens, h);
        self.text_spaced(gpu, &gpu.chip, c, "LOADS", 1.2, at(l.name));
        if let Some(s) = l.source {
            self.text_spaced(gpu, &gpu.chip, c, "FROM", 1.2, at(s));
        }
        self.head_right(gpu, c, "USE", l.used, h);
    }

    /// A column's name over its right edge, as what it heads is set right.
    unsafe fn head_right(&self, gpu: &Gpu, c: Color, name: &str, col: Col, h: Rect) {
        // The spacing runs wider than the font's own measure.
        let w = self.measure(gpu, &gpu.chip, name) + 1.2 * name.len() as f32 + 2.0;
        let r = Rect::new(col.right() - w, h.y, w, h.h);
        self.text_spaced(gpu, &gpu.chip, c, name, 1.2, r);
    }

    unsafe fn context_line(
        &self,
        gpu: &Gpu,
        scene: &ContextScene,
        i: usize,
        band: Rect,
        row: &ContextRow,
    ) {
        let l = scene.layout;
        let cell = |c: Col| Rect::new(c.x, band.y + 6.0, c.w, band.h - 8.0);
        let off = row.switch == Some(false);
        let ink = if off {
            theme::text_dim().fade(0.7)
        } else {
            theme::text()
        };
        self.text_tabular(
            gpu,
            &gpu.small_right,
            if off { ink } else { theme::text_dim() },
            &row.tokens,
            cell(l.tokens),
        );

        let sw = l.switch_at(i, scene.scroll);
        let y = sw.y + sw.h / 2.0;
        match row.switch {
            Some(on) => {
                let b = layout::button(ContextHit::Switch(i), scene.hot, scene.pressed);
                if let (Some(fill), _) = theme::button_look(b) {
                    self.fill_rounded(&sw, 6.0, fill);
                }
                let x = sw.x + 10.0;
                if on {
                    self.led(x, y, theme::working());
                } else {
                    let dot = Rect::new(x - 2.5, y - 2.5, 5.0, 5.0);
                    self.fill_rounded(&dot, 2.5, theme::lamp_off().mix(theme::text_dim(), 0.3));
                }
                let word = Rect::new(x + 8.0, band.y + 6.0, sw.right() - x - 8.0, band.h - 8.0);
                let said = if on { "on" } else { "off" };
                self.text(&gpu.small, theme::text_dim(), said, word);
            }
            None => {
                let word = Rect::new(sw.x + 4.0, band.y + 6.0, sw.w - 4.0, band.h - 8.0);
                self.text(&gpu.small, theme::legend().fade(0.7), "fixed", word);
            }
        }

        self.text(&gpu.small, ink, &row.name, cell(l.name));
        if let Some(s) = l.source {
            self.text(&gpu.small, theme::legend(), row.source, cell(s));
        }
        let used = if row.warn {
            theme::waiting()
        } else {
            theme::text_dim()
        };
        self.text(&gpu.small_right, used, &row.used, cell(l.used));
    }

    unsafe fn overview_line(&self, gpu: &Gpu, l: &ContextLayout, band: Rect, row: &OverviewRow) {
        let cell = |c: Col| Rect::new(c.x, band.y + 6.0, c.w, band.h - 8.0);
        self.text(&gpu.small, theme::text(), &row.name, cell(l.o_name));
        self.text_tabular(
            gpu,
            &gpu.small_right,
            theme::text_dim(),
            &row.before,
            cell(l.o_before),
        );
        self.text_tabular(
            gpu,
            &gpu.small_right,
            theme::text_dim(),
            &row.after,
            cell(l.o_after),
        );
        let c = if row.warn {
            theme::waiting()
        } else {
            theme::text_dim()
        };
        self.text(&gpu.small_right, c, &row.idle, cell(l.o_idle));
    }
}
