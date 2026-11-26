//! What says, on every screen, that a session has the desktop: runes cut
//! along the screen's edges, faint, with a light going slowly round that
//! warms each rune it passes. The same ring a working terminal has round
//! its glass ([`crate::sparks::Ring`]), drawn round the screen, since the
//! screen is where this session works.
//!
//! No strip of colour and nothing that breathes: a faint wash round a
//! whole screen bands and pulls the eye, and the runes say it better.
//!
//! Each edge is one thin window the mouse goes through. Only the runes
//! whose light changed are drawn again, a few on each beat, and with
//! animations off none are: they stand still.

use std::time::Duration;

use super::layer::{Area, Layer};
use super::rules::edges;
use crate::particles;
use crate::render;
use crate::sparks::{self, Ring};
use crate::theme::Color;

/// How deep into the screen an edge's window reaches, in DIPs.
pub(super) const DEPTH: f32 = 16.0;
const RUNE_H: f32 = 9.0;
const RUNE_CUT: f32 = 1.1;
/// How far the light goes in a second, in DIPs: under two runes. Slow, so
/// it is seen from the corner of an eye without being watched.
const SPEED: f32 = 90.0;
/// Lights going round each screen, evenly apart, so one is in sight on
/// whichever side the human looks at.
const LIGHTS: usize = 2;
/// A rune cooling back to rest after the light passed over it.
const COOL: Duration = Duration::from_millis(2600);
/// How far ahead of the light a rune starts to warm, in DIPs.
const RISE: f32 = 70.0;
/// How strongly a rune shows at rest, and with the light on it.
const REST: f32 = 0.4;
const LIT: f32 = 0.95;
/// Where the runes stand with animations off.
const STILL: f32 = 0.5;
/// The steps a rune's light is drawn in. One that moved less than a step
/// is not drawn again.
const STEPS: f32 = 32.0;

/// A rune on an edge: where round the ring, which, its middle in its
/// window's DIPs, the way its head points, and the light it was last
/// drawn with.
struct Cell {
    d: f32,
    rune: usize,
    at: (f32, f32),
    up: (f32, f32),
    drawn: Option<u8>,
}

impl Cell {
    fn area(&self) -> Area {
        let half = RUNE_H / 2.0 + 3.0;
        (self.at.0 - half, self.at.1 - half, 2.0 * half, 2.0 * half)
    }
}

struct Strip {
    layer: Layer,
    cells: Vec<Cell>,
}

struct Screen {
    /// The ring's length in DIPs.
    len: f32,
    /// Where on the ring its lights start, so two screens are not in step.
    phase: f32,
    strips: Vec<Strip>,
}

pub(super) struct Frame {
    screens: Vec<Screen>,
    /// The colour it was last drawn in.
    tint: Option<Color>,
}

/// How warm the rune at `d` is with a light at `head` on a ring `len`
/// long: warming as the light comes, full as it passes, cooling behind.
pub(super) fn warmth(head: f32, d: f32, len: f32) -> f32 {
    let len = len.max(f32::EPSILON);
    let ahead = (d - head).rem_euclid(len);
    if ahead < RISE {
        let k = 1.0 - ahead / RISE;
        return k * k * (3.0 - 2.0 * k);
    }
    let behind = (head - d).rem_euclid(len);
    sparks::cool(behind / SPEED, COOL)
}

/// How strongly the rune at `d` shows `t` seconds into control.
pub(super) fn level(t: f32, phase: f32, d: f32, len: f32) -> f32 {
    let heat = (0..LIGHTS)
        .map(|i| {
            let head = t * SPEED + phase + len * i as f32 / LIGHTS as f32;
            warmth(head, d, len)
        })
        .fold(0.0, f32::max);
    REST + (LIT - REST) * heat
}

fn step_of(level: f32) -> u8 {
    (level.clamp(0.0, 1.0) * STEPS).round() as u8
}

impl Frame {
    /// The runes round every screen there is now, not yet shown.
    pub fn new(gpu: &render::Gpu) -> Frame {
        let mut screens = Vec::new();
        for monitor in super::monitors() {
            let Some((rect, _)) = super::monitor_rects(monitor) else {
                continue;
            };
            let scale = super::monitor_scale(monitor);
            let (w, h) = (
                (rect[2] - rect[0]) as f32 / scale,
                (rect[3] - rect[1]) as f32 / scale,
            );
            let mid = DEPTH / 2.0;
            let ring = Ring::new(mid, mid, w - mid, h - mid, 0.0);
            let seed = particles::seed_of(&format!("{rect:?}"));
            let depth = (DEPTH * scale).round() as i32;
            let mut strips: Vec<Option<Strip>> = edges(rect, depth)
                .into_iter()
                .map(|[x, y, sw, sh]| {
                    (sw > 0 && sh > 0)
                        .then(|| Layer::new(gpu, (x, y), (sw, sh), scale).ok())
                        .flatten()
                        .map(|layer| Strip {
                            layer,
                            cells: Vec::new(),
                        })
                })
                .collect();
            for cut in ring.runes(seed) {
                let ((x, y), (nx, ny)) = ring.at(cut.d);
                // Top, bottom, left, right, as `edges` gives them, and
                // the rune's place from its own window's corner.
                let (side, at) = if ny < -0.5 {
                    (0, (x, y))
                } else if ny > 0.5 {
                    (1, (x, y - (h - DEPTH)))
                } else if nx < -0.5 {
                    (2, (x, y - DEPTH))
                } else {
                    (3, (x - (w - DEPTH), y - DEPTH))
                };
                if let Some(strip) = strips[side].as_mut() {
                    strip.cells.push(Cell {
                        d: cut.d,
                        rune: cut.rune,
                        at,
                        // Its head to the rim, as round a terminal's glass.
                        up: (nx, ny),
                        drawn: None,
                    });
                }
            }
            screens.push(Screen {
                len: ring.length(),
                phase: (seed % 1000) as f32 / 1000.0 * ring.length(),
                strips: strips.into_iter().flatten().collect(),
            });
        }
        Frame {
            screens,
            tint: None,
        }
    }

    /// Draws the runes as they stand `t` seconds into control, in `tint`,
    /// `alpha` over all of it: every rune the first time and after the
    /// colour changed, then only those whose light moved a step. With
    /// `moving` false they stand still.
    pub fn step(&mut self, t: f32, tint: Color, alpha: f32, moving: bool) {
        let fresh = self.tint != Some(tint);
        self.tint = Some(tint);
        for screen in &mut self.screens {
            let (len, phase) = (screen.len, screen.phase);
            for strip in &mut screen.strips {
                let mut dirty: Option<Area> = None;
                let first = !strip.layer.shown || fresh;
                for cell in &mut strip.cells {
                    let now = step_of(if moving {
                        level(t, phase, cell.d, len)
                    } else {
                        STILL
                    });
                    if cell.drawn != Some(now) {
                        cell.drawn = Some(now);
                        dirty = Some(match dirty {
                            None => cell.area(),
                            Some(a) => union(a, cell.area()),
                        });
                    }
                }
                let cells = &strip.cells;
                let draw = |p: &super::layer::Pen, within: Option<Area>| unsafe {
                    for cell in cells {
                        if within.is_some_and(|a| !overlaps(a, cell.area())) {
                            continue;
                        }
                        let level = cell.drawn.unwrap_or(0) as f32 / STEPS;
                        let heat = ((level - REST) / (LIT - REST)).clamp(0.0, 1.0);
                        // The one ramp all of Horadric's light is coloured
                        // through, held under white: this is a rune seen
                        // sideways, not a spark.
                        let ink = particles::heat_color(tint, 0.7 * heat);
                        p.rune_on_anything(
                            cell.rune, cell.at, cell.up, RUNE_H, ink, RUNE_CUT, level,
                        );
                    }
                };
                if first {
                    if strip.layer.paint(|p| draw(p, None)).is_ok() {
                        let pos = strip.layer.pos;
                        strip.layer.present(pos, alpha);
                    }
                } else if let Some(area) = dirty {
                    if strip
                        .layer
                        .paint_part(area, |p| draw(p, Some(area)))
                        .is_ok()
                    {
                        strip.layer.present_part(area);
                    }
                }
            }
        }
    }

    /// Fades all of it, sending nothing but the alpha.
    pub fn fade(&mut self, alpha: f32) {
        for strip in self.screens.iter_mut().flat_map(|s| &mut s.strips) {
            let pos = strip.layer.pos;
            strip.layer.place(pos, alpha);
        }
    }
}

fn union(a: Area, b: Area) -> Area {
    let (l, t) = (a.0.min(b.0), a.1.min(b.1));
    let (r, bt) = ((a.0 + a.2).max(b.0 + b.2), (a.1 + a.3).max(b.1 + b.3));
    (l, t, r - l, bt - t)
}

fn overlaps(a: Area, b: Area) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}
