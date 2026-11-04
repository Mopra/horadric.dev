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
/// frame leaves a margin round the stone, so the stone itself is a little
/// smaller. It fills the tile's left end, where the lamp was.
const STONE: f32 = 39.0;
/// The square a little stone's frame is drawn into.
const MINI: f32 = 11.0;
/// How far the little stones swing across, and over and under the stone:
/// clear of its top and bottom edge, inside the tile.
const REACH: (f32, f32) = (7.0, 24.0);
/// A new little stone rising out of the big one onto its ring.
const EMERGE: f32 = 0.6;
/// A working stone's runes brighten and dim this slowly, so a turn at
/// work is seen moving between tools.
const WORK_PULSE: Duration = Duration::from_millis(2400);
/// A little stone turning once about itself.
const MINI_SPIN: Duration = Duration::from_millis(3000);

/// Where a tile's stone stands: in the middle of its switch.
pub(super) fn centre(r: &Rect) -> (f32, f32) {
    (r.x + 29.0, r.y + r.h / 2.0)
}

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
            let (cx, cy) = centre(&r);
            let cy = cy - super::LAND_RISE * landing;
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
                Phase::Working => {
                    let pulse = if scene.ambient {
                        let clock = scene
                            .now
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .unwrap_or_default();
                        motion::breathe(clock, WORK_PULSE)
                    } else {
                        1.0
                    };
                    theme::phase_color(lit).fade(0.6 + 0.4 * pulse)
                }
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
            let tumble = if scene.ambient { look.tumble } else { 0.0 };
            let pose = frames(look.yaw, tumble);
            let minis = minis(s, scene.now);
            let small = |m: &Mini, behind: bool| {
                let o = tilecube::orbit(m.turn, REACH, m.gather);
                if o.behind != behind {
                    return;
                }
                let size = MINI * o.scale * (1.0 - 0.5 * m.gather);
                let at = Rect::new(
                    cx + o.dx - size / 2.0,
                    cy + o.dy - size / 2.0,
                    size,
                    size,
                );
                let fade = if behind { 0.8 } else { 1.0 };
                let pose = frames(m.spin, 0.0);
                self.rune_stone(stones, pose, &at, m.ink, presence.max(0.6) * fade);
            };
            for m in &minis {
                small(m, true);
            }
            let at = Rect::new(cx - STONE / 2.0, cy - STONE / 2.0, STONE, STONE);
            self.rune_stone(stones, pose, &at, ink, presence);
            for m in &minis {
                small(m, false);
            }
        }
    }

    /// The baked stone between frames `a` and `b`, `k` of the way to `b`,
    /// filling `at`, its runes lit in `ink`. Blending the two frames
    /// either side makes a slow spin glide rather than tick.
    unsafe fn rune_stone(
        &self,
        stones: &Stones,
        (a, b, k): (usize, usize, f32),
        at: &Rect,
        ink: Color,
        opacity: f32,
    ) {
        let (Some((stone_a, light_a)), Some((stone_b, light_b))) = (stones.get(&a), stones.get(&b))
        else {
            return;
        };
        let dest = rect(at);
        let draw = |stone, o: f32| {
            if o > 0.0 {
                self.rt.DrawBitmap(stone, Some(&dest), o, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
            }
        };
        draw(stone_a, opacity);
        draw(stone_b, opacity * k);
        // An opacity mask draws only without antialiasing.
        self.rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
        for (light, o) in [(light_a, 1.0 - k), (light_b, k)] {
            if o <= 0.0 {
                continue;
            }
            self.brush.SetColor(&color(ink.fade(opacity * o)));
            self.rt.FillOpacityMask(
                light,
                self.brush,
                D2D1_OPACITY_MASK_CONTENT_GRAPHICS,
                Some(&dest),
                None,
            );
        }
        self.rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    }
}

/// The two baked frames a stone spun `yaw` degrees and `tumble` of the way
/// round its roll away falls between, and how far to the second.
fn frames(yaw: f32, tumble: f32) -> (usize, usize, f32) {
    use crate::turning::{frame_at, tumble_at, STEP_DEG, TUMBLE_STEPS};
    if tumble > 0.0 {
        let quarter = ((yaw / 90.0).round() as i64).rem_euclid(4) as usize;
        let at = tumble.clamp(0.0, 1.0) * TUMBLE_STEPS as f32;
        let step = at.floor();
        let a = tumble_at(quarter, step as usize);
        return (a, tumble_at(quarter, step as usize + 1), at - step);
    }
    let at = yaw / STEP_DEG;
    let step = at.floor();
    (frame_at(step * STEP_DEG), frame_at((step + 1.0) * STEP_DEG), at - step)
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
                turn: turn + tilecube::spot(k),
                gather: 0.0,
                spin: spin(k as u64 * 97),
            })
            .collect();
    }
    let shown = s.band.shown(now, false);
    let shown = &shown[..shown.len().min(MAX_SPARKS)];
    let since = |t: SystemTime| now.duration_since(t).unwrap_or(Duration::ZERO);
    shown
        .iter()
        .enumerate()
        .filter_map(|(k, h)| {
            // Its place from the order it came in, not from how many
            // there are now, so none jumps as others come and go.
            let order = s.band.hirelings.iter().position(|x| x.id == h.id).unwrap_or(k);
            let spot = tilecube::spot(order);
            let young = since(h.started).as_secs_f32() / EMERGE;
            let rising = 1.0 - motion::ease_out(young.min(1.0));
            let seed = crate::particles::seed_of(&h.id);
            let (ink, turn, gather) = match h.lot_at(now) {
                Lot::Working => (theme::accent(&h.kind), turn + spot, rising),
                // Stopped in front, where it is seen first.
                Lot::Waiting => (theme::waiting(), 0.25, rising),
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
