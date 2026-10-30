//! Behind the conversation: runes drifting up in the project's colour, at
//! three depths.

use super::view::{Gfx, Palette, View};
use crate::folio_look as look;
use crate::sparks::RUNES;
impl View {
    /// Runes drifting up behind the conversation, farthest first so the
    /// near ones pass in front. While Claude works more join, they burn
    /// brighter and take more of the project's colour, and the near ones
    /// rush past while the far ones barely stir, so the turn feels like
    /// moving forward. No glow under them: a faint wash that wide bands
    /// into rings on a dark background, and the runes say it better.
    pub(super) unsafe fn background(
        &self,
        gfx: &Gfx,
        pal: &Palette,
        view: (f32, f32),
        clocks: (f32, f32),
        heat: f32,
    ) {
        let ink = pal.fg.mix(pal.accent, 0.35 + 0.5 * heat);
        let column = look::column(view.0);
        for (k, l) in look::LAYERS.iter().enumerate() {
            for i in 0..l.count + l.busy {
                let e = look::ember(k, i, clocks, view, heat, RUNES.len());
                let alpha = e.alpha * look::calm(e.x + e.size / 2.0, column);
                if alpha <= 0.004 {
                    continue;
                }
                let c = ink.mix(pal.bg, e.sunk).with_alpha(alpha);
                gfx.rune(e.rune, e.x, e.y, e.size, c, e.stroke);
            }
        }
    }
}
