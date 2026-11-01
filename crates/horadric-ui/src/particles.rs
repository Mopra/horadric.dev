//! Particles for anything that flies: sparks, embers, ash, motes and
//! streaks. A particle's place, size, heat and life at an age are a
//! function of its burst's seed and that age alone, as the Folio's embers
//! are (`folio_look::ember`), so nothing is stepped or stored per particle
//! and a window only draws what this says.
//!
//! Every emitter colours its particles through one ramp, [`heat_color`],
//! so all of Horadric's fire looks like one fire. They are drawn sharp into
//! a window's light layer, and the bloom makes them glow.

use std::f32::consts::TAU;

use crate::theme::Color;

/// What a burst throws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Thrown out every way, fast and hot, dragged to a stop.
    Spark,
    /// Rises slowly in a shaft, warm through the middle of its life.
    Mote,
    /// Lifts off and drifts up, swaying, cooling as it goes.
    Ember,
    /// Crumbles and falls, barely warm.
    Ash,
    /// One fast line along a heading, its trail a streak.
    Streak,
}

/// How a kind moves and burns, in DIPs and seconds.
struct Feel {
    life: (f32, f32),
    speed: (f32, f32),
    /// The widest a heading strays from straight up, in turns; one half is
    /// every way.
    spread: f32,
    /// How fast it is dragged to a stop, per second.
    drag: f32,
    /// Pull downward, DIPs per second squared; negative rises.
    gravity: f32,
    size: (f32, f32),
    /// The heat it is born with, past one burning white.
    heat: f32,
    /// How far it sways side to side.
    sway: f32,
}

fn feel(kind: Kind) -> Feel {
    match kind {
        Kind::Spark => Feel {
            life: (0.35, 0.6),
            speed: (40.0, 90.0),
            spread: 0.5,
            drag: 6.0,
            gravity: 20.0,
            size: (0.8, 1.4),
            heat: 1.5,
            sway: 0.0,
        },
        Kind::Mote => Feel {
            life: (0.8, 1.3),
            speed: (14.0, 30.0),
            spread: 0.02,
            drag: 0.0,
            gravity: 0.0,
            size: (0.7, 1.2),
            heat: 0.9,
            sway: 1.5,
        },
        Kind::Ember => Feel {
            life: (0.7, 1.2),
            speed: (6.0, 16.0),
            spread: 0.18,
            drag: 1.5,
            gravity: -10.0,
            size: (0.8, 1.3),
            heat: 1.2,
            sway: 2.0,
        },
        Kind::Ash => Feel {
            life: (0.6, 1.0),
            speed: (3.0, 8.0),
            spread: 0.5,
            drag: 2.0,
            gravity: 18.0,
            size: (0.6, 1.0),
            heat: 0.4,
            sway: 1.0,
        },
        Kind::Streak => Feel {
            life: (0.5, 0.5),
            speed: (0.0, 0.0),
            spread: 0.0,
            drag: 0.0,
            gravity: 0.0,
            size: (1.4, 1.4),
            heat: 1.3,
            sway: 0.0,
        },
    }
}

/// A burst of `count` particles of one kind from one point, all born at
/// once. The caller keeps its age; the burst keeps nothing else.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Burst {
    pub seed: u64,
    pub count: usize,
    pub kind: Kind,
}

/// One particle at one moment, placed relative to its burst's origin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Particle {
    pub x: f32,
    pub y: f32,
    /// Its radius, in DIPs.
    pub size: f32,
    /// From cold at 0 to white hot past 1, for [`heat_color`].
    pub heat: f32,
    /// How visible it is, 1 at its brightest and 0 gone.
    pub alpha: f32,
}

impl Burst {
    pub const fn new(seed: u64, count: usize, kind: Kind) -> Self {
        Burst { seed, count, kind }
    }

    /// The longest any of its particles lives, in seconds: past it the
    /// burst asks for no more frames.
    pub fn span(&self) -> f32 {
        feel(self.kind).life.1
    }

    pub fn alive(&self, age: f32) -> bool {
        age >= 0.0 && age < self.span()
    }

    /// Particle `i` at `age` seconds, or `None` once it has died or before
    /// it is born.
    pub fn particle(&self, i: usize, age: f32) -> Option<Particle> {
        let f = feel(self.kind);
        let u = |salt: u64| unit(hash(&[self.seed, i as u64, salt]));
        let span = |(lo, hi): (f32, f32), k: f32| lo + (hi - lo) * k;
        let life = span(f.life, u(1));
        if !(0.0..life).contains(&age) {
            return None;
        }
        let k = age / life;
        let speed = span(f.speed, u(2));
        // Straight up is a heading of zero; the spread fans it both ways.
        let heading = (u(3) * 2.0 - 1.0) * f.spread * TAU;
        let (vx, vy) = (heading.sin() * speed, -heading.cos() * speed);
        // Drag in closed form: speed falls off as e^(-drag t), so the
        // distance gone is v (1 - e^(-drag t)) / drag.
        let gone = if f.drag > 0.0 {
            (1.0 - (-f.drag * age).exp()) / f.drag
        } else {
            age
        };
        let sway = (age * (3.0 + 2.0 * u(4)) + u(5) * TAU).sin() * f.sway * k;
        let x = vx * gone + sway;
        let y = vy * gone + 0.5 * f.gravity * age * age;
        // Bright at once, fading over the last of its life.
        let alpha = (1.0 - k) * (k * 12.0).min(1.0);
        let heat = match self.kind {
            // Warm through the middle, cool at both ends.
            Kind::Mote => f.heat * (std::f32::consts::PI * k).sin(),
            _ => f.heat * (1.0 - k),
        };
        Some(Particle {
            x,
            y,
            size: span(f.size, u(6)) * (1.0 - 0.4 * k),
            heat,
            alpha,
        })
    }

    /// Particle `i` at `age` and `steps` earlier moments `gap` seconds
    /// apart, newest first, each fainter. In the light layer the bloom
    /// joins them into a streak.
    pub fn trail(&self, i: usize, age: f32, steps: usize, gap: f32) -> Vec<Particle> {
        (0..=steps)
            .filter_map(|k| {
                let p = self.particle(i, age - k as f32 * gap)?;
                let fade = 1.0 - k as f32 / (steps + 1) as f32;
                Some(Particle {
                    alpha: p.alpha * fade,
                    size: p.size * (0.6 + 0.4 * fade),
                    ..p
                })
            })
            .collect()
    }
}

/// Spark `i` of `n` circling a point at `t` seconds, `period` a lap, on an
/// ellipse `rx` by `ry`: its place and its heat, which flickers. The same
/// spark sampled a little earlier is its trail.
pub fn orbit(i: usize, n: usize, t: f32, period: f32, (rx, ry): (f32, f32)) -> Particle {
    let n = n.max(1);
    let u = |salt: u64| unit(hash(&[i as u64, salt]));
    let a = (t / period.max(0.01) + i as f32 / n as f32) * TAU;
    let wobble = 1.0 + 0.12 * (t * (5.0 + 3.0 * u(1)) + u(2) * TAU).sin();
    let flicker = 0.85 + 0.35 * (t * (9.0 + 4.0 * u(3)) + u(4) * TAU).sin().max(0.0);
    Particle {
        x: a.cos() * rx * wobble,
        y: a.sin() * ry * wobble,
        size: 1.0,
        heat: flicker,
        alpha: 1.0,
    }
}

/// The one ramp heat maps to colour through: `base` when cool, through its
/// hot mix, to white past one. Every emitter uses it, so all of Horadric's
/// fire is one fire.
pub fn heat_color(base: Color, heat: f32) -> Color {
    let white = Color::rgb(0xFFFFFF);
    let hot = base.mix(white, 0.45);
    let heat = heat.max(0.0);
    if heat <= 1.0 {
        base.mix(hot, heat)
    } else {
        hot.mix(white, (heat - 1.0).min(1.0))
    }
}

/// FNV-1a over the words, stable across runs and builds.
fn hash(words: &[u64]) -> u64 {
    words.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &w| {
        w.to_le_bytes()
            .iter()
            .fold(h, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
    })
}

fn unit(h: u64) -> f32 {
    (h % 10_000) as f32 / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_particle_is_the_same_at_the_same_age() {
        let b = Burst::new(7, 6, Kind::Spark);
        assert_eq!(b.particle(3, 0.2), b.particle(3, 0.2));
        assert_ne!(b.particle(3, 0.2), b.particle(4, 0.2));
    }

    #[test]
    fn a_particle_is_gone_before_birth_and_after_its_life() {
        let b = Burst::new(1, 4, Kind::Ember);
        assert!(b.particle(0, -0.01).is_none());
        assert!(b.particle(0, b.span()).is_none());
        assert!(!b.alive(b.span()));
        assert!(b.alive(0.0));
    }

    #[test]
    fn sparks_fly_out_and_embers_rise() {
        let sparks = Burst::new(2, 16, Kind::Spark);
        let out = (0..16)
            .filter_map(|i| sparks.particle(i, 0.15))
            .map(|p| (p.x * p.x + p.y * p.y).sqrt())
            .fold(f32::INFINITY, f32::min);
        assert!(out > 1.0, "every spark has left the middle, nearest {out}");

        let embers = Burst::new(3, 8, Kind::Ember);
        let early = embers.particle(0, 0.1).unwrap();
        let later = embers.particle(0, 0.5).unwrap();
        assert!(later.y < early.y, "an ember rises");
    }

    #[test]
    fn a_particle_cools_and_fades_as_it_dies() {
        let b = Burst::new(4, 1, Kind::Spark);
        let young = b.particle(0, 0.05).unwrap();
        let old = b.particle(0, 0.3).unwrap();
        assert!(old.heat < young.heat);
        assert!(old.alpha < young.alpha);
    }

    #[test]
    fn a_trail_fades_behind_its_head() {
        let b = Burst::new(5, 1, Kind::Mote);
        let trail = b.trail(0, 0.6, 3, 0.05);
        assert_eq!(trail.len(), 4);
        assert!(trail.windows(2).all(|w| w[1].alpha < w[0].alpha));
    }

    #[test]
    fn heat_runs_from_the_colour_to_white() {
        let c = Color::rgb(0x3060C0);
        assert_eq!(heat_color(c, 0.0), c);
        let white = heat_color(c, 2.0);
        assert!(white.r > 0.99 && white.g > 0.99 && white.b > 0.99);
        assert!(heat_color(c, 0.5).r > c.r);
    }

    #[test]
    fn an_orbit_goes_round() {
        let a = orbit(0, 1, 0.0, 1.0, (10.0, 5.0));
        let b = orbit(0, 1, 0.5, 1.0, (10.0, 5.0));
        assert!(a.x > 0.0 && b.x < 0.0);
    }
}
