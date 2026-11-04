//! The rune stone where an agent's tile had its icon: the icon's own baked
//! stone (`crate::turning`), its runes lit in the phase's colour, turning a
//! quarter for each new tool, with a little stone circling it for each
//! subagent. It moves every frame, so it is drawn over the still layer
//! rather than into it.

use std::time::{Duration, SystemTime};

use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_OPACITY_MASK_CONTENT_GRAPHICS,
};

use super::{color, rect, Painter, Scene, Stones, HIRELING_ORBIT, MAX_SPARKS, RUNE_SETTLE};
use crate::layout::Rect;
use crate::motion;
use crate::theme::{self, Color};
use crate::tilecube;
use horadric_core::session::{Phase, Session};

/// The square the big stone's frame is drawn into, in DIPs. The baked
/// frame leaves a margin round the stone, so it stands a little smaller
/// and clear of the lamp.
const STONE: f32 = 28.0;
/// How far above the tile's middle the stone stands, so the context
/// meter under it has room.
const LIFT: f32 = 2.0;
/// The square a little stone's frame is drawn into.
const MINI: f32 = 12.0;
/// How far out the little stones circle, across and up and down.
const REACH: (f32, f32) = (12.0, 4.5);
/// The ring's middle sits this far right of the stone's, so a little
/// stone at its left end stays off the lamp.
const RING_RIGHT: f32 = 1.5;
/// A little stone turning once about itself.
const MINI_SPIN: Duration = Duration::from_millis(3000);

/// Whether a tile carries a rune stone: one an agent works in. A plain or
/// SSH terminal keeps its flat glyph.
pub(super) fn has_stone(s: &Session) -> bool {
    !s.shell && s.ssh.is_none()
}

/// A little stone this frame: its runes' colour, where on its ring, and
/// how far it has fallen in.
struct Mini {
    ink: Color,
    turn: f32,
    gather: f32,
    spin: f32,
}

impl Painter<'_> {
    /// Every agent tile's stone and its little ones, over the still layer.
    pub(super) unsafe fn key_stones(&self, scene: &Scene, stones: &Stones) {
        for (r, s, look) in super::tiles(scene) {
            if !has_stone(s) {
                continue;
            }
            // Where the tile itself stands this frame: a finished turn's
            // key jumps, and the stone with it.
            let landing = if scene.ambient && s.phase == Phase::Done {
                motion::land(look.phase_age)
            } else {
                0.0
            };
            let (cx, cy) = super::icon_centre(&r);
            let cy = cy - LIFT - super::LAND_RISE * landing;
            let lit = if s.phase == Phase::Done && !s.unread() {
                &Phase::Idle
            } else {
                &s.phase
            };
            let reach = if s.brother.as_ref().is_some_and(|b| !b.reach) {
                0.45
            } else {
                1.0
            };
            let presence = theme::presence(lit) * look.enter * reach;
            let ink = match lit {
                Phase::Working => theme::phase_color(lit),
                Phase::Waiting(_) => {
                    let breath = if scene.ambient {
                        motion::waiting_breath(look.phase_age)
                    } else {
                        1.0
                    };
                    theme::phase_color(lit).fade(0.4 + 0.6 * breath)
                }
                Phase::Done => theme::phase_color(lit),
                _ => theme::text_dim().fade(0.45),
            };
            let turn = if scene.ambient { look.turn } else { 0.0 };
            let deg = tilecube::stone_deg(look.turns, turn);
            let minis = minis(s, scene.now);
            let small = |m: &Mini, behind: bool| {
                let o = tilecube::orbit(m.turn, REACH, m.gather);
                if o.behind != behind {
                    return;
                }
                let size = MINI * o.scale * (1.0 - 0.5 * m.gather);
                let at = Rect::new(
                    cx + RING_RIGHT + o.dx - size / 2.0,
                    cy + o.dy - size / 2.0,
                    size,
                    size,
                );
                let fade = if behind { 0.8 } else { 1.0 };
                self.rune_stone(stones, m.spin, &at, m.ink, presence.max(0.6) * fade);
            };
            for m in &minis {
                small(m, true);
            }
            let at = Rect::new(cx - STONE / 2.0, cy - STONE / 2.0, STONE, STONE);
            self.rune_stone(stones, deg, &at, ink, presence);
            for m in &minis {
                small(m, false);
            }
        }
    }

    /// The baked stone turned `deg` past the icon's angle, filling `at`,
    /// its runes lit in `ink`.
    unsafe fn rune_stone(&self, stones: &Stones, deg: f32, at: &Rect, ink: Color, opacity: f32) {
        let Some((stone, light)) = stones.get(&crate::turning::frame_at(deg)) else {
            return;
        };
        let dest = rect(at);
        self.rt.DrawBitmap(
            stone,
            Some(&dest),
            opacity,
            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
            None,
        );
        // An opacity mask draws only without antialiasing.
        self.rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
        self.brush.SetColor(&color(ink.fade(opacity)));
        self.rt.FillOpacityMask(
            light,
            self.brush,
            D2D1_OPACITY_MASK_CONTENT_GRAPHICS,
            Some(&dest),
            None,
        );
        self.rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    }
}

/// The little stones this frame: one per hireling the band knows of, its
/// runes in its kind's colour or in how it stands, or one per subagent at
/// work where the band knows nothing (a mod too old, a headless run).
fn minis(s: &Session, now: SystemTime) -> Vec<Mini> {
    use horadric_core::hireling::Lot;
    let clock = now.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    let turn = motion::cycle(clock, HIRELING_ORBIT);
    let spin = |seed: u64| 360.0 * motion::cycle(clock, MINI_SPIN) + (seed % 360) as f32;
    if s.band.hirelings.is_empty() {
        let n = if s.phase == Phase::Working {
            s.subagents(now)
        } else {
            s.agents_alone(now)
        }
        .min(MAX_SPARKS);
        return (0..n)
            .map(|k| Mini {
                ink: theme::working(),
                turn: turn + k as f32 / n as f32,
                gather: 0.0,
                spin: spin(k as u64 * 97),
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
            let seed = crate::particles::seed_of(&h.id);
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
            Some(Mini { ink, turn, gather, spin: spin(seed) })
        })
        .collect()
}
