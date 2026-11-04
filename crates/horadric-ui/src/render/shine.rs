//! The cluster's moving light: what gives off light each frame, drawn
//! sharp into the window's light layer (`crate::light`), which blooms it.

use windows::core::Result;

use std::time::{Duration, SystemTime};

use windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE;
use windows_numerics::Vector2;

use super::{color, Gpu, Painter, Scene, Target, BEAM_H, SHIMMER, WARRIV_BREATH};
use crate::layout::{Metrics, Rect};
use crate::light::Bloom;
use crate::motion::{self, ORBIT};
use crate::particles::{self, heat_color, Burst, Kind};
use crate::theme::{self, Color};
use horadric_core::session::{Phase, Session};

/// How far round a lamp its sparks, runes and embers reach.
const LAMP_REACH: f32 = 24.0;

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
        || !scene.flights.is_empty()
        || super::tiles(scene).enumerate().any(|(i, (_, s, look))| {
            scene.beckons(i, s)
                || matches!(s.phase, Phase::Working | Phase::Waiting(_))
                || s.phase == Phase::Done && look.phase_age < motion::BEAM
                || s.phase == Phase::Ended && look.settle > 0.0
                || !s.band.hirelings.is_empty()
                || s.agents_alone(scene.now) > 0
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
        lit.extend(self.flights(scene));
        for (i, (r, s, look)) in super::tiles(scene).enumerate() {
            let c = theme::phase_color(&s.phase);
            if scene.beckons(i, s) {
                let on = motion::beacon(look.phase_age) * look.enter;
                self.rim(&r, radius, c, 0.7 * on);
                lit.push(r.inset(-2.0));
                continue;
            }
            let lamp = super::lamp_rect(&r);
            let stone = super::keycube::has_stone(s);
            match s.phase {
                // A rune stone carries its own light; it has no lamp.
                Phase::Working | Phase::Ended if stone => {}
                Phase::Working => {
                    let t = motion::cycle(look.phase_age, ORBIT);
                    self.surge(gpu, &lamp, c, t, look.enter);
                    lit.push(lamp);
                    let context = s.status.as_ref().and_then(|st| st.context);
                    if let Some(full) = context.filter(|&f| f >= 75.0) {
                        let (ix, iy) = super::icon_centre(&r);
                        let track = Rect::new(ix - 10.0, iy + 14.0, 20.0, 3.0);
                        let t = motion::cycle(look.phase_age, SHIMMER);
                        self.glint(&track, look.context, t, theme::fullness_color(full));
                        lit.push(track);
                    }
                }
                Phase::Waiting(_) => {
                    let breath = motion::waiting_breath(look.phase_age);
                    self.rim(&r, radius, c, (0.3 + 0.4 * breath) * look.enter);
                    let spread = self.arrival_ring(&r, radius, c, look.arrival);
                    lit.push(r.inset(-2.0 - spread));
                }
                Phase::Done if look.phase_age < motion::BEAM => {
                    if let Some(shaft) = self.beam(&r, s, look.phase_age, look.enter) {
                        lit.push(shaft);
                    }
                }
                Phase::Ended if look.settle > 0.0 => {
                    self.power_down(&lamp, s, 1.0 - look.settle);
                    lit.push(lamp.inset(-LAMP_REACH));
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

    /// A hot glint running along the lit part of a context meter that is
    /// nearly full, `t` of the way through its run.
    unsafe fn glint(&self, r: &Rect, fraction: f32, t: f32, c: Color) {
        let lit = r.w * fraction.clamp(0.0, 1.0);
        if lit <= 0.0 {
            return;
        }
        let x = r.x + lit * motion::ease_in_out(t);
        let fade = (std::f32::consts::PI * t).sin();
        let glint = Rect::new(x - 2.0, r.y, 4.0, r.h);
        self.fill_rounded(&glint, r.h / 2.0, heat_color(c, 1.2).fade(fade));
    }

    /// The live particles of `b`, `age` seconds old, round `(x, y)`, in the
    /// one fire's colours from `c`.
    unsafe fn burst(&self, b: &Burst, (x, y): (f32, f32), age: f32, c: Color, strength: f32) {
        if !b.alive(age) {
            return;
        }
        for i in 0..b.count {
            if let Some(p) = b.particle(i, age) {
                let ink = heat_color(c, p.heat).fade(p.alpha * strength);
                self.dot(x + p.x, y + p.y, p.size, ink);
            }
        }
    }

    /// A sharp round point of light; the bloom gives it its glow.
    unsafe fn dot(&self, x: f32, y: f32, radius: f32, c: Color) {
        if c.a <= 0.0 {
            return;
        }
        let e = D2D1_ELLIPSE {
            point: Vector2 { X: x, Y: y },
            radiusX: radius,
            radiusY: radius,
        };
        self.brush.SetColor(&color(c));
        self.rt.FillEllipse(&e, self.brush);
    }

    /// A finished turn is loot dropping: a shaft of its lamp's green shoots
    /// up off the key with motes rising in it, and fades. Where it reaches.
    unsafe fn beam(&self, r: &Rect, s: &Session, age: Duration, enter: f32) -> Option<Rect> {
        let (grown, bright) = motion::beam(age);
        let strength = bright * enter;
        if strength <= 0.0 {
            return None;
        }
        let c = theme::phase_color(&Phase::Done);
        let (x, base) = if super::keycube::has_stone(s) {
            super::keycube::centre(r)
        } else {
            super::icon_centre(r)
        };
        let tall = 12.0 + BEAM_H * grown;
        let stops = [(0.0, c.with_alpha(0.0)), (1.0, heat_color(c, 0.8))];
        for (w, a) in [(5.0, 0.3), (1.6, 0.9)] {
            let shaft = Rect::new(x - w / 2.0, base - tall, w, tall);
            self.fill_rounded_gradient(&shaft, w / 2.0, &stops, a * strength);
        }
        self.dot(x, base, 2.0, heat_color(c, 1.3).fade(strength));
        let motes = Burst::new(particles::seed_of(&s.id), 8, Kind::Mote);
        let secs = age.as_secs_f32();
        for i in 0..motes.count {
            // Spread up the shaft, so it is full of them at once.
            let from = base - tall * 0.8 * i as f32 / motes.count as f32;
            if let Some(p) = motes.particle(i, secs) {
                let ink = heat_color(c, p.heat).fade(p.alpha * strength);
                self.dot(x + p.x, from + p.y, p.size, ink);
            }
        }
        let rise = 40.0;
        Some(Rect::new(
            x - 8.0,
            base - tall - rise,
            16.0,
            tall + rise + 8.0,
        ))
    }

    /// A light leaving a task's row for the lamp of the session that took
    /// it: a streak with a trail, flaring as it lands. Where each went.
    unsafe fn flights(&self, scene: &Scene) -> Vec<Rect> {
        let c = theme::working();
        let mut lit = Vec::new();
        for (n, f) in scene.flights.iter().enumerate() {
            let ((fx, fy), (tx, ty), p) = (f.from, f.to, f.done);
            // A little arc out to the right, so it reads as thrown.
            let bow = 24.0;
            let at = |p: f32| {
                let k = motion::ease_in_out(p.clamp(0.0, 1.0));
                let out = (std::f32::consts::PI * k).sin() * bow;
                (fx + (tx - fx) * k + out, fy + (ty - fy) * k)
            };
            const TRAIL: usize = 5;
            for tail in (1..=TRAIL).rev() {
                let (x, y) = at(p - tail as f32 * 0.035);
                let fade = 1.0 - tail as f32 / (TRAIL + 1) as f32;
                self.dot(x, y, 0.5 + fade, heat_color(c, 0.9 * fade).fade(0.8 * fade));
            }
            let (x, y) = at(p);
            self.dot(x, y, 1.8, heat_color(c, 1.5));
            if p > 0.8 {
                let k = (p - 0.8) / 0.2;
                self.dot(
                    tx,
                    ty,
                    1.0 + 2.0 * (1.0 - k),
                    heat_color(c, 1.2).fade(1.0 - k),
                );
                let sparks = Burst::new(n as u64, 6, Kind::Spark);
                self.burst(&sparks, (tx, ty), k * sparks.span(), c, 1.0);
            }
            let reach = 16.0;
            let (x0, x1) = (fx.min(tx) - reach, fx.max(tx) + bow + reach);
            let (y0, y1) = (fy.min(ty) - reach, fy.max(ty) + reach);
            lit.push(Rect::new(x0, y0, x1 - x0, y1 - y0));
        }
        lit
    }

    /// A lamp going out, `t` of the way through: its light collapses to
    /// its middle and leaves it as a few embers.
    unsafe fn power_down(&self, r: &Rect, s: &Session, t: f32) {
        let white = Color::rgb(0xFFFFFF);
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        if t < 0.5 {
            let q = motion::ease_in_out(t / 0.5);
            let h = (r.h * (1.0 - q)).max(r.w * 0.6);
            let w = r.w * (1.0 - 0.4 * q);
            let core = Rect::new(cx - w / 2.0, cy - h / 2.0, w, h);
            self.fill_rounded(&core, w / 2.0, white.with_alpha(0.5 + 0.4 * q));
        }
        let embers = Burst::new(particles::seed_of(&s.id), 4, Kind::Ember);
        let k = (t - 0.35) / 0.65;
        self.burst(&embers, (cx, cy), k * embers.span(), theme::working(), 1.0);
    }
}
