//! Behind the conversation: a glow in the project's colour and runes
//! drifting up through it.

use windows_numerics::Vector2;

use super::view::{d2d, Gfx, Palette, View};
use crate::folio_look as look;
use crate::layout::Rect;
use crate::sparks::RUNES;
impl View {
    /// The glow rising from under the composer in the project's colour,
    /// and runes drifting up through it, warmer while Claude works.
    pub(super) unsafe fn background(
        &self,
        gfx: &Gfx,
        pal: &Palette,
        w: f32,
        h: f32,
        t: f32,
        heat: f32,
    ) {
        let breath = (t * 1.6).sin() * 0.5 + 0.5;
        gfx.glow.SetCenter(Vector2 {
            X: w / 2.0,
            Y: h + h * 0.1,
        });
        let reach = w.max(h) * (0.62 + 0.06 * heat * breath);
        gfx.glow.SetRadiusX(reach);
        gfx.glow.SetRadiusY(reach * 0.7);
        gfx.glow.SetOpacity(0.09 + heat * (0.08 + 0.06 * breath));
        gfx.rt
            .FillRectangle(&d2d(&Rect::new(0.0, 0.0, w, h)), &gfx.glow);
        let ink = pal.fg.mix(pal.accent, 0.35 + 0.5 * heat);
        for i in 0..look::EMBERS {
            let e = look::ember(i, t, w, h, heat, RUNES.len());
            if e.alpha <= 0.004 {
                continue;
            }
            gfx.rune(e.rune, e.x, e.y, e.size, ink.with_alpha(e.alpha), 1.3);
        }
    }
}
