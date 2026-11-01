//! The cluster's moving light: what gives off light each frame, drawn
//! sharp into the window's light layer (`crate::light`), which blooms it.

use windows::core::Result;

use std::time::SystemTime;

use super::{Gpu, Painter, Scene, Target, WARRIV_BREATH};
use crate::layout::{Metrics, Rect};
use crate::light::Bloom;
use crate::motion::{self, ORBIT};
use crate::theme::{self, Color};
use horadric_core::session::Phase;

impl Target {
    /// Lays the frame's light over what is drawn. Nothing when nothing
    /// shines, so a quiet window costs no blur.
    pub(super) unsafe fn bloom(&self, gpu: &Gpu, m: &Metrics, scene: &Scene) -> Result<()> {
        if !scene.ambient || !shines(scene) {
            return Ok(());
        }
        let layer = self.light.begin(&self.rt)?;
        let lit = self.painter(&layer).shine(gpu, m, scene);
        for r in lit {
            self.light.lit(r);
        }
        self.light.present(&self.rt, &Bloom::tile())
    }
}

/// Whether anything in the frame may give off light. Generous: a frame it
/// lets through with nothing lit clears the layer and blurs nothing.
fn shines(scene: &Scene) -> bool {
    let astir = scene.tasks.as_ref().is_some_and(|t| t.astir);
    astir
        || super::tiles(scene).enumerate().any(|(i, (_, s, _))| {
            scene.beckons(i, s) || matches!(s.phase, Phase::Working | Phase::Waiting(_))
        })
}

impl Painter<'_> {
    /// What gives off light this frame, sharp, on black, and where.
    unsafe fn shine(&self, gpu: &Gpu, m: &Metrics, scene: &Scene) -> Vec<Rect> {
        let mut lit = Vec::new();
        let radius = m.tile_radius;
        if let Some(r) = self.astir_rim(m, scene) {
            lit.push(r.inset(-2.0));
        }
        for (i, (r, s, look)) in super::tiles(scene).enumerate() {
            let c = theme::phase_color(&s.phase);
            if scene.beckons(i, s) {
                let on = motion::beacon(look.phase_age) * look.enter;
                self.rim(&r, radius, c, 0.7 * on);
                lit.push(r.inset(-2.0));
                continue;
            }
            match s.phase {
                Phase::Working => {
                    let t = motion::cycle(look.phase_age, ORBIT);
                    let lamp = super::lamp_rect(&r);
                    self.surge(gpu, &lamp, c, t, look.enter);
                    lit.push(lamp);
                }
                Phase::Waiting(_) => {
                    let breath = motion::waiting_breath(look.phase_age);
                    self.rim(&r, radius, c, (0.3 + 0.4 * breath) * look.enter);
                    let spread = self.arrival_ring(&r, radius, c, look.arrival);
                    lit.push(r.inset(-2.0 - spread));
                }
                _ => {}
            }
        }
        lit
    }

    /// A key's rim lit in `c`: the line round its edge, which the bloom
    /// spills onto the plate round it. `strength` from 0 to 1.
    unsafe fn rim(&self, r: &Rect, radius: f32, c: Color, strength: f32) {
        if strength <= 0.0 {
            return;
        }
        let line = c.with_alpha((strength * 0.75).min(1.0));
        self.stroke_rounded(&r.inset(0.75), radius - 0.75, line, 1.5);
    }

    /// A ring of light leaving the rim the moment a tile starts waiting,
    /// `arrival` running from 1 down to 0. How far out it has gone.
    unsafe fn arrival_ring(&self, r: &Rect, radius: f32, c: Color, arrival: f32) -> f32 {
        if arrival <= 0.0 {
            return 0.0;
        }
        let spread = (1.0 - arrival) * 5.0;
        let ring = r.inset(-spread);
        self.stroke_rounded(&ring, radius + spread, c.with_alpha(arrival * 0.6), 1.2);
        spread
    }

    /// Warriv's gold round the quests tile while the camp works, breathing,
    /// as light. Where it is, if lit.
    unsafe fn astir_rim(&self, m: &Metrics, scene: &Scene) -> Option<Rect> {
        let (Some(l), Some(t)) = (&scene.layout.tasks, &scene.tasks) else {
            return None;
        };
        if !t.astir {
            return None;
        }
        let clock = scene
            .now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        let breath = motion::breathe(clock, WARRIV_BREATH);
        self.rim(&l.rect, m.tile_radius, theme::warriv(), 0.4 + 0.6 * breath);
        Some(l.rect)
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
