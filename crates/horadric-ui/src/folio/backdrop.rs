//! Behind the conversation: runes drifting up in the project's colour.

use super::view::{Gfx, Palette, View};
use crate::folio_look as look;
use crate::sparks::RUNES;
impl View {
    /// Runes drifting up behind the conversation, more of them, brighter
    /// and in more of the project's colour while Claude works. No glow
    /// under them: a faint wash that wide bands into rings on a dark
    /// background, and the runes say it better.
    pub(super) unsafe fn background(
        &self,
        gfx: &Gfx,
        pal: &Palette,
        w: f32,
        h: f32,
        t: f32,
        heat: f32,
    ) {
        let ink = pal.fg.mix(pal.accent, 0.35 + 0.5 * heat);
        for i in 0..look::EMBERS + look::BUSY_EMBERS {
            let e = look::ember(i, t, w, h, heat, RUNES.len());
            if e.alpha <= 0.004 {
                continue;
            }
            gfx.rune(e.rune, e.x, e.y, e.size, ink.with_alpha(e.alpha), 1.3);
        }
    }
}
