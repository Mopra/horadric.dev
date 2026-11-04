//! Where a session tile's rune stone and its subagents' little stones
//! stand: for each new tool the stone rolls away or turns aside a
//! quarter, and a little stone circles it for each subagent. Pure, so every frame is a function of
//! its inputs alone; the drawing is in `render::keycube`.

use std::f32::consts::TAU;

/// How fast a working session's stone spins, in degrees a second.
pub const SPIN: f32 = 24.0;

/// Of a new tool's roll, the share spent squaring the stone up to the
/// nearest quarter of its spin before it rolls away.
const SQUARE_UP: f32 = 0.3;

/// A pose of the cube, exact: from the stone's own space to the world's,
/// row by row.
type Rot = [[i8; 3]; 3];

const UPRIGHT: Rot = [[1, 0, 0], [0, 1, 0], [0, 0, 1]];
/// A quarter about the upright, the way the stone spins.
const ASIDE: Rot = [[0, 0, 1], [0, 1, 0], [-1, 0, 0]];
/// A quarter about the level axis nearest the view's right, top away.
const AWAY: Rot = [[0, -1, 0], [1, 0, 0], [0, 0, 1]];

/// The cube's 24 poses as each of these turned about the upright, one per
/// face that can be up. Must match SPUN in bake.py.
const SPUN: [Rot; 6] = [
    UPRIGHT,
    [[1, 0, 0], [0, 0, -1], [0, 1, 0]],
    [[1, 0, 0], [0, -1, 0], [0, 0, -1]],
    [[1, 0, 0], [0, 0, 1], [0, -1, 0]],
    [[0, -1, 0], [1, 0, 0], [0, 0, 1]],
    [[0, 1, 0], [-1, 0, 0], [0, 0, 1]],
];

/// And as each of these rolled away, one per face on the roll's axis.
/// Must match ROLLED in bake.py.
const ROLLED: [Rot; 6] = [
    UPRIGHT,
    [[0, 0, 1], [0, 1, 0], [-1, 0, 0]],
    [[-1, 0, 0], [0, 1, 0], [0, 0, -1]],
    [[0, 0, -1], [0, 1, 0], [1, 0, 0]],
    [[1, 0, 0], [0, 0, -1], [0, 1, 0]],
    [[1, 0, 0], [0, 0, 1], [0, -1, 0]],
];

fn mul(a: Rot, b: Rot) -> Rot {
    let mut m = [[0; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    m
}

fn times(r: Rot, n: usize, m: Rot) -> Rot {
    (0..n % 4).fold(m, |m, _| mul(r, m))
}

/// Which of `reps` turned `n` quarters by `r` makes `m`, and that `n`.
fn find(reps: &[Rot; 6], r: Rot, m: Rot) -> (usize, usize) {
    for (i, &rep) in reps.iter().enumerate() {
        for n in 0..4 {
            if times(r, n, rep) == m {
                return (i, n);
            }
        }
    }
    unreachable!("every pose of a cube is a quarter turn of one of six")
}

/// How the stone stands between moves: which face is up, as an index
/// into the baked groups, and how far it has spun about the upright, in
/// degrees past the icon's angle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stand {
    pub up: usize,
    pub yaw: f32,
}

/// The stone as the icon stands it.
pub const STANDING: Stand = Stand { up: 0, yaw: 0.0 };

/// What to draw: the stone spun, or part way through a roll, `deg` round
/// the roll of group `group`, whose frames are baked a whole turn round.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pose {
    Spun(Stand),
    Rolling { group: usize, deg: f32 },
}

/// A new tool's move: a quarter turn aside about the upright, or a
/// quarter roll away. One axis at a time, never both.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Move {
    Aside,
    Away,
}

fn quarters(yaw: f32) -> usize {
    ((yaw / 90.0).round() as i64).rem_euclid(4) as usize
}

/// Where `mv` leaves a stone that stood `at`. Aside is always onward, a
/// quarter past the square one behind, never back against the spin; a
/// roll starts from the nearest square quarter.
pub fn after(at: Stand, mv: Move) -> Stand {
    match mv {
        Move::Aside => Stand {
            up: at.up,
            yaw: (at.yaw / 90.0).floor() * 90.0 + 90.0,
        },
        Move::Away => {
            let rolled = mul(AWAY, times(ASIDE, quarters(at.yaw), SPUN[at.up]));
            let (up, n) = find(&SPUN, ASIDE, rolled);
            Stand {
                up,
                yaw: 90.0 * n as f32,
            }
        }
    }
}

/// How the stone stands `p` of the way through `mv` from `from` (0 at the
/// change, 1 and on once over). A roll squares up first. Eased, so it
/// sets off and settles like something heavy.
pub fn pose(from: Stand, mv: Move, p: f32) -> Pose {
    let to = after(from, mv);
    if p >= 1.0 {
        return Pose::Spun(to);
    }
    let p = p.max(0.0);
    match mv {
        Move::Aside => Pose::Spun(Stand {
            up: from.up,
            yaw: from.yaw + (to.yaw - from.yaw) * ease_in_out(p),
        }),
        Move::Away if p < SQUARE_UP => {
            let square = (from.yaw / 90.0).round() * 90.0;
            Pose::Spun(Stand {
                up: from.up,
                yaw: from.yaw + (square - from.yaw) * ease_in_out(p / SQUARE_UP),
            })
        }
        Move::Away => {
            let standing = times(ASIDE, quarters(from.yaw), SPUN[from.up]);
            let (group, n) = find(&ROLLED, AWAY, standing);
            let t = ease_in_out((p - SQUARE_UP) / (1.0 - SQUARE_UP));
            Pose::Rolling {
                group,
                deg: 90.0 * (n as f32 + t),
            }
        }
    }
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

    fn all_poses() -> Vec<Rot> {
        let mut all = vec![UPRIGHT];
        while let Some(m) = all
            .iter()
            .flat_map(|&m| [mul(ASIDE, m), mul(AWAY, m)])
            .find(|m| !all.contains(m))
        {
            all.push(m);
        }
        all
    }

    fn standing(at: Stand) -> Rot {
        times(ASIDE, quarters(at.yaw), SPUN[at.up])
    }

    #[test]
    fn the_baked_groups_hold_every_pose_once() {
        assert_eq!(all_poses().len(), 24);
        for (reps, r) in [(&SPUN, ASIDE), (&ROLLED, AWAY)] {
            let mut seen: Vec<Rot> = (0..6)
                .flat_map(|i| (0..4).map(move |n| times(r, n, reps[i])))
                .collect();
            seen.sort();
            seen.dedup();
            assert_eq!(seen.len(), 24);
        }
    }

    #[test]
    fn a_roll_squares_up_then_rolls_a_quarter_into_its_next_pose() {
        let from = Stand { up: 0, yaw: 100.0 };
        let Pose::Spun(s) = pose(from, Move::Away, 0.15) else {
            panic!("squaring up first");
        };
        assert!(s.up == 0 && s.yaw < 100.0 && s.yaw > 90.0);
        let Pose::Rolling { group, deg } = pose(from, Move::Away, 0.31) else {
            panic!("rolling by then");
        };
        let (g, n) = find(&ROLLED, AWAY, standing(from));
        assert_eq!(group, g);
        assert!(deg >= 90.0 * n as f32 && deg < 90.0 * n as f32 + 1.0);
        let to = after(from, Move::Away);
        assert_eq!(standing(to), mul(AWAY, standing(from)));
        assert_eq!(pose(from, Move::Away, 1.0), Pose::Spun(to));
    }

    #[test]
    fn a_turn_aside_goes_onward_and_never_rolls() {
        let from = Stand { up: 3, yaw: 130.0 };
        assert_eq!(after(from, Move::Aside), Stand { up: 3, yaw: 180.0 });
        let Pose::Spun(s) = pose(from, Move::Aside, 0.5) else {
            panic!("a turn aside is a spin");
        };
        assert!(s.up == 3 && s.yaw > 130.0 && s.yaw < 180.0);
    }

    /// How far apart two baked frames are, 0 to 255 a channel on average.
    fn apart(a: usize, b: usize) -> f32 {
        let ((sa, _), (sb, _)) = (crate::turning::frame(a), crate::turning::frame(b));
        let sum: u64 = sa.iter().zip(&sb).map(|(x, y)| x.abs_diff(*y) as u64).sum();
        sum as f32 / sa.len() as f32
    }

    #[test]
    fn every_baked_roll_starts_and_ends_on_its_standing_frames() {
        use crate::turning::{rolled_at, spun_at};
        for up in 0..6 {
            for q in 0..4 {
                let from = Stand { up, yaw: 90.0 * q as f32 };
                let to = after(from, Move::Away);
                let (group, n) = find(&ROLLED, AWAY, standing(from));
                let start = rolled_at(group, 90.0 * n as f32);
                let end = rolled_at(group, 90.0 * (n + 1) as f32);
                let (a, b) = (apart(start, spun_at(up, from.yaw)), apart(end, spun_at(to.up, to.yaw)));
                assert!(a < 2.0 && b < 2.0, "pose {up}.{q}: {a} {b}");
                // And a different pose is far from it, or the check proves nothing.
                assert!(apart(end, spun_at(up, from.yaw)) > 4.0, "pose {up}.{q} rolled looks unrolled");
            }
        }
    }

    #[test]
    fn four_rolls_away_come_back() {
        let start = Stand { up: 2, yaw: 270.0 };
        let mut at = start;
        for _ in 0..4 {
            at = after(at, Move::Away);
        }
        assert_eq!(standing(at), standing(start));
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
