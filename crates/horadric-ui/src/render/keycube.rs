//! The Horadric cube where a session tile's icon was: the tool glyph on
//! its front face, a quarter roll whenever the glyph changes, and a little
//! cube circling it for each subagent. Solid stone, not light, so it is
//! drawn with the key and never blooms.

use std::time::{Duration, SystemTime};

use windows_numerics::{Matrix3x2, Vector2};

use super::{color, Gpu, Painter, HIRELING_ORBIT, MAX_SPARKS, RUNE_SETTLE};
use crate::anim::Look;
use crate::motion;
use crate::theme::{self, Color};
use crate::tilecube::{self, Face, Side};
use horadric_core::session::{Phase, Session};

/// Half the big cube's side, in DIPs.
const HALF: f32 = 8.0;
/// Half a subagent cube's side, in DIPs.
const MINI: f32 = 2.6;
/// How far out from the big cube's middle the little ones circle.
const REACH: f32 = 15.5;
/// Half the glyph's face in the icon font's own units: the icon format
/// draws its glyph about 14 wide, so this leaves a rim of stone round it.
const GLYPH_FACE: f32 = 11.5;

/// A little cube this frame: its colour, where on its ring, and how far
/// it has fallen in.
struct Mini {
    ink: Color,
    turn: f32,
    gather: f32,
    seed: u32,
}

impl Painter<'_> {
    /// The cube round `(cx, cy)`, its glyph in `ink`, `c` the phase's own
    /// colour for a face that waits on you.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn key_cube(
        &self,
        gpu: &Gpu,
        s: &Session,
        look: &Look,
        (cx, cy): (f32, f32),
        ink: Color,
        c: Color,
        presence: f32,
        now: SystemTime,
        ambient: bool,
    ) {
        let roll = if ambient && look.flip > 0.0 {
            motion::ease_in_out(look.flip)
        } else {
            0.0
        };
        let wait = if s.phase.is_waiting() {
            0.12 + if ambient { 0.2 * motion::waiting_breath(look.phase_age) } else { 0.1 }
        } else {
            0.0
        };
        let glyph = theme::tile_icon(s);
        let minis = self.minis(s, now);
        let clock = now.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
        let secs = (clock.as_secs() % 3600) as f32 + clock.subsec_millis() as f32 / 1000.0;
        let small = |m: &Mini, behind: bool| {
            let o = tilecube::orbit(0, 1, m.turn, REACH, m.gather);
            if o.behind != behind {
                return;
            }
            let half = MINI * o.scale * (1.0 - 0.6 * m.gather);
            let spin = tilecube::tumble(secs, m.seed);
            for f in tilecube::faces(&spin, (cx + o.dx, cy + o.dy), half) {
                self.cube_face(gpu, &f, theme::well().mix(m.ink, 0.6), m.ink, presence);
            }
        };
        for m in &minis {
            small(m, true);
        }
        for f in tilecube::faces(&tilecube::pose(roll), (cx, cy), HALF) {
            let stone = if f.side == Side::Front && wait > 0.0 {
                theme::well().mix(c, wait)
            } else {
                theme::well()
            };
            self.cube_face(gpu, &f, stone, theme::text_dim(), presence);
            let shown = match f.side {
                Side::Front => Some(glyph),
                Side::Leaving => Some(look.was_icon),
                Side::Blank => None,
            };
            if let Some(g) = shown.filter(|g| *g != ' ') {
                let [rx, ry, dx, dy, ox, oy] = tilecube::glyph_frame(&f);
                self.rt.SetTransform(&Matrix3x2 {
                    M11: rx / GLYPH_FACE,
                    M12: ry / GLYPH_FACE,
                    M21: dx / GLYPH_FACE,
                    M22: dy / GLYPH_FACE,
                    M31: ox,
                    M32: oy,
                });
                let seen = (f.facing * 2.5).min(1.0);
                let at = crate::layout::Rect::new(-14.0, -14.0, 28.0, 28.0);
                self.icon(&gpu.icon, ink.fade(seen * presence), g, at);
                self.rt.SetTransform(&Matrix3x2::identity());
            }
        }
        for m in &minis {
            small(m, false);
        }
    }

    /// One face: stone lit by how squarely it meets the light, with its
    /// edges cut in `edge`.
    unsafe fn cube_face(&self, gpu: &Gpu, f: &Face, stone: Color, edge: Color, presence: f32) {
        let fill = stone.mix(theme::text(), 0.06 + 0.26 * f.light).fade(presence);
        let points = f.corners.map(|(x, y)| Vector2 { X: x, Y: y });
        self.polygon(gpu, &points, fill);
        self.brush.SetColor(&color(edge.with_alpha(0.5).fade(presence)));
        for k in 0..4 {
            let (a, b) = (points[k], points[(k + 1) % 4]);
            self.rt.DrawLine(a, b, self.brush, 0.7, None);
        }
    }

    /// The little cubes this frame: one per hireling the band knows of,
    /// coloured by its kind and by how it stands, or one per subagent at
    /// work where the band knows nothing (a mod too old, a headless run).
    fn minis(&self, s: &Session, now: SystemTime) -> Vec<Mini> {
        use horadric_core::hireling::Lot;
        let clock = now.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
        let turn = motion::cycle(clock, HIRELING_ORBIT);
        if s.band.hirelings.is_empty() {
            let n = if s.phase == Phase::Working {
                s.subagents(now)
            } else {
                s.agents_alone(now)
            }
            .min(MAX_SPARKS);
            let ink = theme::working();
            return (0..n)
                .map(|k| Mini {
                    ink,
                    turn: turn + k as f32 / n as f32,
                    gather: 0.0,
                    seed: k as u32,
                })
                .collect();
        }
        let shown = s.band.shown(now, false);
        let shown = &shown[..shown.len().min(MAX_SPARKS)];
        let n = shown.len().max(1) as f32;
        let since = |t: SystemTime| now.duration_since(t).unwrap_or(Duration::ZERO);
        shown
            .iter()
            .enumerate()
            .filter_map(|(k, h)| {
                let spot = k as f32 / n;
                let seed = crate::particles::seed_of(&h.id) as u32;
                let (ink, turn, gather) = match h.lot_at(now) {
                    Lot::Working => (theme::accent(&h.kind), turn + spot, 0.0),
                    // Stopped in front, low, where it is seen first.
                    Lot::Waiting => (theme::waiting(), 0.25, 0.0),
                    Lot::Done => {
                        let t = since(h.ended.unwrap_or(h.heard)).as_secs_f32() / RUNE_SETTLE;
                        if t >= 1.0 {
                            return None;
                        }
                        (theme::done(), turn + spot, motion::ease_in_out(t))
                    }
                    Lot::Failed => (theme::error(), spot, 0.0),
                };
                Some(Mini { ink, turn, gather, seed })
            })
            .collect()
    }
}
