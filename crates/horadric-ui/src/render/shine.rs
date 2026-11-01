//! The cluster's moving light: what gives off light each frame, drawn
//! sharp into the window's light layer (`crate::light`), which blooms it.

use windows::core::Result;

use super::{Gpu, Painter, Scene, Target};
use crate::layout::Rect;
use crate::light::Bloom;
use crate::motion::{self, ORBIT};
use crate::theme::{self, Color};
use horadric_core::session::Phase;

impl Target {
    /// Lays the frame's light over what is drawn. Nothing when nothing
    /// shines, so a quiet window costs no blur.
    pub(super) unsafe fn bloom(&self, gpu: &Gpu, scene: &Scene) -> Result<()> {
        if !scene.ambient || !shines(scene) {
            return Ok(());
        }
        let layer = self.light.begin(&self.rt)?;
        let lit = self.painter(&layer).shine(gpu, scene);
        for r in lit {
            self.light.lit(r);
        }
        self.light.present(&self.rt, &Bloom::tile())
    }
}

/// Whether anything in the frame gives off light.
fn shines(scene: &Scene) -> bool {
    super::tiles(scene)
        .enumerate()
        .any(|(i, (_, s, _))| s.phase == Phase::Working && !scene.beckons(i, s))
}

impl Painter<'_> {
    /// What gives off light this frame, sharp, on black, and where.
    unsafe fn shine(&self, gpu: &Gpu, scene: &Scene) -> Vec<Rect> {
        let mut lit = Vec::new();
        for (i, (r, s, look)) in super::tiles(scene).enumerate() {
            if s.phase != Phase::Working || scene.beckons(i, s) {
                continue;
            }
            let t = motion::cycle(look.phase_age, ORBIT);
            let lamp = super::lamp_rect(&r);
            self.surge(gpu, &lamp, theme::phase_color(&s.phase), t, look.enter);
            lit.push(lamp);
        }
        lit
    }

    /// Light running up a working session's lamp, `t` of the way through
    /// one climb: two pulses, each a white hot head with a tail of the
    /// phase's colour behind it, kept inside the glass. The bloom does the
    /// rest, so the glow stays round the lamp where the light is.
    unsafe fn surge(&self, gpu: &Gpu, r: &Rect, c: Color, t: f32, strength: f32) {
        let strength = strength.clamp(0.0, 1.0);
        if strength <= 0.0 {
            return;
        }
        let white = Color::rgb(0xFFFFFF);
        let hot = c.mix(white, 0.55);
        let tail = r.h * 0.55;
        let travel = r.h + tail;
        self.masked(gpu, r, r.w / 2.0, || {
            for k in [0.0, 0.5] {
                let p = (t + k).rem_euclid(1.0);
                // Fast through the middle, easing at the ends, so it
                // reads as a surge and not as a conveyor.
                let p = motion::ease_in_out(p);
                let head = r.bottom() - travel * p;
                let streak = Rect::new(r.x, head, r.w, tail);
                let stops = [(0.0, hot), (0.12, c), (1.0, c.with_alpha(0.0))];
                self.fill_rounded_gradient(&streak, r.w / 2.0, &stops, strength);
            }
        });
    }
}
