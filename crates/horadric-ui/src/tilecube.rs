//! Where a session tile's rune stone and its subagents' little stones
//! stand: for each new tool the stone rolls away or turns a quarter aside, and a little stone
//! circles it for each subagent. Pure, so every frame is a function of
//! its inputs alone; the drawing is in `render::keycube`.

use std::f32::consts::TAU;

/// How fast a working session's stone spins, in degrees a second.
pub const SPIN: f32 = 24.0;

/// Of a new tool's roll, the share spent squaring the stone up to the
/// nearest quarter of its spin before it rolls away.
const SQUARE_UP: f32 = 0.3;

/// The quarter of a turn nearest `yaw`, which the stone tumbles from: the
/// bake has the tumble from each quarter.
pub fn quarter(yaw: f32) -> f32 {
    (yaw / 90.0).round() * 90.0
}

/// How the stone stands `p` of the way through a new tool's move (0 at
/// the change, 1 and on once over), having spun to `from` and coming to
/// `to`: its spin in degrees, and how far round its roll away, 0 to 1.
/// A roll squares up first; a turn aside is the spin alone, so the stone
/// only ever turns on one axis at a time. Eased, so it sets off and
/// settles like something heavy.
pub fn stone_pose(from: f32, to: f32, p: f32, rolls: bool) -> (f32, f32) {
    if p >= 1.0 {
        return (to, 0.0);
    }
    let p = p.max(0.0);
    if !rolls {
        return (from + (to - from) * ease_in_out(p), 0.0);
    }
    if p < SQUARE_UP {
        return (from + (to - from) * ease_in_out(p / SQUARE_UP), 0.0);
    }
    (to, ease_in_out((p - SQUARE_UP) / (1.0 - SQUARE_UP)))
}

fn ease_in_out(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Where one little stone circles the big one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Orbiter {
    pub dx: f32,
    pub dy: f32,
    /// How large, from perspective: over 1 in front, under 1 behind.
    pub scale: f32,
    /// Behind the big stone, so drawn before it.
    pub behind: bool,
}

/// A little stone at `turn` of the way round its ring (0 to 1), `gather`
/// of the way in toward the middle (0 out on the ring, 1 fallen into the
/// stone). The ring loops over the top of the stone, down its front and
/// back up behind, so it changes sides above and below the stone, never
/// across its corners; `reach` is how far it swings across and up and
/// down. Leaning across as it comes forward, it reads as a loop in depth.
pub fn orbit(turn: f32, (rx, ry): (f32, f32), gather: f32) -> Orbiter {
    let a = TAU * turn;
    let k = 1.0 - gather.clamp(0.0, 1.0);
    let (s, c) = a.sin_cos();
    Orbiter {
        dx: s * rx * k,
        dy: -c * ry * k,
        scale: 1.0 + 0.15 * s,
        behind: s < 0.0,
    }
}

/// Where the `k`th little stone sits on the ring, from the order they
/// came in: spread by the golden angle, so each keeps its place as others
/// come and go and none ever jumps.
pub fn spot(k: usize) -> f32 {
    (k as f32 * 0.381_966).fract()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_tool_squares_the_stone_up_then_rolls_it_once() {
        assert_eq!(quarter(130.0), 90.0);
        assert_eq!(quarter(140.0), 180.0);
        assert_eq!(stone_pose(130.0, 90.0, 0.0, true), (130.0, 0.0));
        let (yaw, roll) = stone_pose(130.0, 90.0, 0.15, true);
        assert!(yaw < 130.0 && yaw > 90.0 && roll == 0.0, "squaring up first");
        let (yaw, roll) = stone_pose(130.0, 90.0, 0.65, true);
        assert!(yaw == 90.0 && roll > 0.3 && roll < 0.7, "then rolling");
        assert_eq!(stone_pose(130.0, 90.0, 1.0, true), (90.0, 0.0));
    }

    #[test]
    fn a_turn_aside_never_rolls() {
        let (yaw, roll) = stone_pose(130.0, 180.0, 0.5, false);
        assert!(yaw > 130.0 && yaw < 180.0 && roll == 0.0);
        assert_eq!(stone_pose(130.0, 180.0, 1.0, false), (180.0, 0.0));
    }

    #[test]
    fn little_stones_change_sides_above_and_below_and_fall_in() {
        let top = orbit(0.0, (6.0, 24.0), 0.0);
        assert!(top.dy < -23.0 && top.dx.abs() < 1e-4, "over the top");
        let front = orbit(0.25, (6.0, 24.0), 0.0);
        assert!(!front.behind && front.scale > 1.0);
        assert!(orbit(0.75, (6.0, 24.0), 0.0).behind);
        let fallen = orbit(0.1, (6.0, 24.0), 1.0);
        assert!(fallen.dx.abs() < 1e-4 && fallen.dy.abs() < 1e-4);
    }

    #[test]
    fn a_new_stone_leaves_the_others_where_they_were() {
        let before: Vec<f32> = (0..3).map(spot).collect();
        let after: Vec<f32> = (0..4).map(spot).collect();
        assert_eq!(before[..], after[..3]);
        assert!((after[3] - after[0]).abs() > 0.05);
    }
}
