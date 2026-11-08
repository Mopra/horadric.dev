//! The cluster's light: what gives off light, drawn sharp into a light
//! layer (`crate::light`), which blooms it. Light that moves is drawn each
//! frame over the window. Light that holds still (a lamp lit at rest, the
//! header's count lights) is collected as [`Emitter`]s while the kept layer
//! is drawn and bloomed into it, and bloomed again only when it changes.

use windows::core::{Interface, Result};

use std::time::{Duration, SystemTime};

use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_COMPOSITE_MODE_PLUS, D2D1_PIXEL_FORMAT,
    D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1BitmapRenderTarget, ID2D1DeviceContext, ID2D1Image,
    D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_PRIMITIVE_BLEND_COPY, D2D1_PRIMITIVE_BLEND_SOURCE_OVER,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
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

/// Something lit that holds still, found while the kept layer is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Emitter {
    /// A lamp's glass, lit `level` of full in `c`.
    Glass { r: Rect, c: Color, level: f32 },
    /// A key's rim, lit `strength` of full in `c`.
    Rim {
        r: Rect,
        radius: f32,
        c: Color,
        strength: f32,
    },
    /// A small round light.
    Dot {
        x: f32,
        y: f32,
        radius: f32,
        c: Color,
    },
    /// A line of light from `a` to `b`, `width` wide: a lit rune's cut, a
    /// stone's rim.
    Line {
        a: (f32, f32),
        b: (f32, f32),
        width: f32,
        c: Color,
    },
    /// Not light: a key drawn over what came before it, which hides
    /// their light where it lies. What follows is over it.
    Cover { r: Rect, radius: f32 },
}

impl Emitter {
    fn bounds(&self) -> Rect {
        match *self {
            Emitter::Glass { r, .. } => r.inset(-1.0),
            Emitter::Rim { r, .. } => r.inset(-2.0),
            Emitter::Dot { x, y, radius, .. } => {
                Rect::new(x - radius, y - radius, 2.0 * radius, 2.0 * radius)
            }
            Emitter::Line { a, b, width, .. } => {
                let (x, y) = (a.0.min(b.0), a.1.min(b.1));
                let w = (a.0 - b.0).abs();
                let h = (a.1 - b.1).abs();
                Rect::new(x, y, w, h).inset(-width)
            }
            Emitter::Cover { r, .. } => r,
        }
    }
}

/// The still light, bloomed, and what it was bloomed from.
pub(super) struct Kept {
    target: ID2D1BitmapRenderTarget,
    size: D2D_SIZE_U,
    signature: Vec<u32>,
}

/// What a set of still lights looks like once bloomed, to a quarter DIP
/// and an eight bit colour: the same signature is the same light, so the
/// kept bloom is reused. Tiles sliding change it; a context bar filling or
/// a cursor over a key does not.
fn signature(emitters: &[Emitter], look: &Bloom) -> Vec<u32> {
    let q = |v: f32| (v * 4.0).round() as i32 as u32;
    let ink = |c: Color| {
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        byte(c.r) << 24 | byte(c.g) << 16 | byte(c.b) << 8 | byte(c.a)
    };
    let level = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    let mut sig = vec![
        look.near.to_bits(),
        look.far.to_bits(),
        look.far_passes as u32,
        look.gain.to_bits(),
    ];
    for e in emitters {
        match *e {
            Emitter::Glass { r, c, level: l } => {
                sig.extend([0, q(r.x), q(r.y), q(r.w), q(r.h), ink(c), level(l)])
            }
            Emitter::Rim {
                r,
                radius,
                c,
                strength,
            } => sig.extend([
                1,
                q(r.x),
                q(r.y),
                q(r.w),
                q(r.h),
                q(radius),
                ink(c),
                level(strength),
            ]),
            Emitter::Dot { x, y, radius, c } => sig.extend([2, q(x), q(y), q(radius), ink(c)]),
            Emitter::Line { a, b, width, c } => {
                sig.extend([4, q(a.0), q(a.1), q(b.0), q(b.1), q(width), ink(c)])
            }
            Emitter::Cover { r, radius } => {
                sig.extend([3, q(r.x), q(r.y), q(r.w), q(r.h), q(radius)])
            }
        }
    }
    sig
}

impl Target {
    /// Blooms `emitters` into `target`, which is drawing, in the order
    /// they were drawn: each [`Emitter::Cover`] wipes the light so far
    /// from under its key before what lies over it is added. Without a
    /// cover that is one bloom, as it is all but always.
    unsafe fn bloom_still(
        &self,
        target: &ID2D1BitmapRenderTarget,
        emitters: &[Emitter],
        look: &Bloom,
    ) -> Result<()> {
        for (n, group) in emitters
            .split(|e| matches!(e, Emitter::Cover { .. }))
            .enumerate()
        {
            if n > 0 {
                // The cover that ended the group before this one.
                let at = emitters
                    .iter()
                    .filter(|e| matches!(e, Emitter::Cover { .. }))
                    .nth(n - 1);
                if let Some(&Emitter::Cover { r, radius }) = at {
                    let dc = target.cast::<ID2D1DeviceContext>()?;
                    dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_COPY);
                    self.painter(target)
                        .fill_rounded(&r, radius, Color::rgb(0).with_alpha(0.0));
                    dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_SOURCE_OVER);
                }
            }
            if group.is_empty() {
                continue;
            }
            let layer = self.still_light.begin(&self.rt)?;
            let p = self.painter(&layer);
            for e in group {
                p.emitter(e);
                self.still_light.lit(e.bounds());
            }
            self.still_light.present(target, look)?;
            // The next group begins by clearing the light layer, which the
            // blurs just queued still read from.
            target.Flush(None, None)?;
        }
        Ok(())
    }

    /// Adds the still light to the kept layer `on`, `size` pixels, while it
    /// is drawn. It is bloomed again only when it differs from last time:
    /// the layer is redrawn every frame a tile slides or a bar fills, and a
    /// blur each of those frames is what the kept layer is there to save.
    pub(super) unsafe fn still_bloom(
        &self,
        on: &ID2D1BitmapRenderTarget,
        emitters: &[Emitter],
        size: D2D_SIZE_U,
    ) -> Result<()> {
        if emitters.iter().all(|e| matches!(e, Emitter::Cover { .. })) {
            return Ok(());
        }
        let look = Bloom::tile();
        let signature = signature(emitters, &look);
        let mut kept = self.kept_light.borrow_mut();
        let fresh = kept
            .as_ref()
            .is_some_and(|k| k.size == size && k.signature == signature);
        if !fresh {
            let target = match kept.take() {
                Some(k) if k.size == size => k.target,
                _ => {
                    let format = D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    };
                    self.rt.CreateCompatibleRenderTarget(
                        None,
                        None,
                        Some(&format),
                        D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE,
                    )?
                }
            };
            target.BeginDraw();
            target.Clear(Some(&D2D1_COLOR_F::default()));
            let bloomed = self.bloom_still(&target, emitters, &look);
            target.EndDraw(None, None)?;
            bloomed?;
            *kept = Some(Kept {
                target,
                size,
                signature,
            });
        }
        let Some(k) = kept.as_ref() else {
            return Ok(());
        };
        let light: ID2D1Image = k.target.GetBitmap()?.cast()?;
        on.cast::<ID2D1DeviceContext>()?.DrawImage(
            &light,
            None,
            None,
            D2D1_INTERPOLATION_MODE_LINEAR,
            D2D1_COMPOSITE_MODE_PLUS,
        );
        Ok(())
    }

    /// Adds `emitters`, bloomed, over a window drawn whole each time, as the
    /// small ones are. `rt` must be drawing. Nothing when nothing is lit.
    pub(super) unsafe fn bloom_over(&self, emitters: &[Emitter]) -> Result<()> {
        let lit: Vec<&Emitter> = emitters
            .iter()
            .filter(|e| !matches!(e, Emitter::Cover { .. }))
            .collect();
        if lit.is_empty() {
            return Ok(());
        }
        let layer = self.light.begin(&self.rt)?;
        let p = self.painter(&layer);
        for e in lit {
            p.emitter(e);
            self.light.lit(e.bounds());
        }
        self.light.present(&self.rt, &Bloom::tile())
    }

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
            if super::under_held(scene, i, &r) {
                continue;
            }
            let c = theme::phase_color(&s.phase);
            let lamp = super::lamp_rect(&r);
            let stone = super::keycube::has_stone(s);
            // The breathing and beckoning lamps are drawn each frame by
            // `Painter::light`; their glass is light, so it is drawn here.
            if scene.beckons(i, s) {
                let on = motion::beacon(look.phase_age) * look.enter;
                self.rim(&r, radius, c, 0.7 * on);
                if !stone {
                    self.glass(&lamp, c, on);
                }
                lit.push(r.inset(-2.0));
                continue;
            }
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
                    if !stone {
                        self.glass(&lamp, c, (0.55 + 0.45 * breath) * look.enter);
                    }
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

    /// One still light, sharp, into the light layer.
    unsafe fn emitter(&self, e: &Emitter) {
        match *e {
            Emitter::Glass { r, c, level } => self.glass(&r, c, level),
            Emitter::Rim {
                r,
                radius,
                c,
                strength,
            } => self.rim(&r, radius, c, strength),
            Emitter::Dot { x, y, radius, c } => self.dot(x, y, radius, c),
            Emitter::Line { a, b, width, c } => {
                let at = |(x, y): (f32, f32)| Vector2 { X: x, Y: y };
                self.brush.SetColor(&color(c));
                self.rt.DrawLine(at(a), at(b), self.brush, width, None);
            }
            Emitter::Cover { .. } => {}
        }
    }

    /// A lamp's glass lit `level` of full in `c`: its colour, and a hot
    /// core down its middle that the bloom spills onto the key round it.
    unsafe fn glass(&self, r: &Rect, c: Color, level: f32) {
        let level = level.clamp(0.0, 1.0);
        if level <= 0.0 {
            return;
        }
        let round = r.w / 2.0;
        self.fill_rounded(r, round, c.fade(0.45 * level));
        let hot = c.mix(Color::rgb(0xFFFFFF), 0.45);
        self.fill_rounded(&r.inset(1.0), (round - 1.0).max(0.5), hot.fade(0.5 * level));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn look() -> Bloom {
        Bloom {
            near: 1.8,
            far: 7.0,
            far_passes: 2,
            gain: 1.0,
        }
    }

    fn lamp(y: f32, level: f32) -> Emitter {
        Emitter::Glass {
            r: Rect::new(12.0, y, 4.0, 30.0),
            c: Color::rgb(0x3FA2FF),
            level,
        }
    }

    #[test]
    fn the_same_still_light_is_bloomed_once() {
        let a = [lamp(14.0, 1.0)];
        // Closer than a quarter DIP and an eight bit level is the same.
        let b = [lamp(14.05, 0.999)];
        assert_eq!(signature(&a, &look()), signature(&b, &look()));
    }

    #[test]
    fn still_light_that_moves_dims_or_changes_is_bloomed_again() {
        let a = [lamp(14.0, 1.0)];
        assert_ne!(
            signature(&a, &look()),
            signature(&[lamp(15.0, 1.0)], &look())
        );
        assert_ne!(
            signature(&a, &look()),
            signature(&[lamp(14.0, 0.5)], &look())
        );
        assert_ne!(signature(&a, &look()), signature(&[], &look()));
        let wider = Bloom { far: 9.0, ..look() };
        assert_ne!(signature(&a, &look()), signature(&a, &wider));
        // A dot where a lamp was is not the same light.
        let dot = [Emitter::Dot {
            x: 12.0,
            y: 14.0,
            radius: 4.0,
            c: Color::rgb(0x3FA2FF),
        }];
        assert_ne!(signature(&a, &look()), signature(&dot, &look()));
    }
}
