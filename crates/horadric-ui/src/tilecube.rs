//! The Horadric cube in place of a session tile's icon: a small stone cube
//! carrying the tool glyph on its front face, which rolls a quarter turn
//! to bring the next glyph forward when the tool changes, with a little
//! cube circling it for each subagent.
//!
//! Pure geometry, so the drawing code only fills what comes back. Every
//! shape is a function of its inputs alone; nothing is kept between frames.

use std::f32::consts::{FRAC_PI_2, TAU};

/// Row major, so a point turns as `m * p`.
pub type Mat = [[f32; 3]; 3];

/// How far the camera stands, in half sides: far enough that the near
/// corner swells only a little, near enough that it reads as solid.
const CAMERA: f32 = 6.0;

/// Where the light comes from: up, left and in front, as the cube in the
/// empty pane has it.
const LIGHT: [f32; 3] = [-0.5, 0.75, 0.6];

/// How the cube stands at rest: tipped toward you and turned a little, so
/// its top and right side show beside the face with the glyph.
const TIP: f32 = 0.38;
const TURN: f32 = -0.5;

/// Which face of the cube this is, for what is drawn on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The face that carries the glyph shown now.
    Front,
    /// The face that carried the glyph before, rolling away.
    Leaving,
    Blank,
}

/// One face as it shows on screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Face {
    pub side: Side,
    /// Bottom left, bottom right, top right, top left, as the face sees it.
    pub corners: [(f32, f32); 4],
    /// How squarely it meets the light, 0 to 1.
    pub light: f32,
    /// How squarely it faces you, 0 (edge on) to 1.
    pub facing: f32,
    depth: f32,
}

struct Plan {
    n: [f32; 3],
    u: [f32; 3],
    v: [f32; 3],
    side: Side,
}

/// Each face's normal and its own right and up. The glyph's face is the
/// front; the one that held the last glyph is the bottom, so a roll that
/// brings the top forward carries it down and away.
const PLANS: [Plan; 6] = [
    Plan { n: [0.0, 0.0, 1.0], u: [1.0, 0.0, 0.0], v: [0.0, 1.0, 0.0], side: Side::Front },
    Plan { n: [0.0, -1.0, 0.0], u: [1.0, 0.0, 0.0], v: [0.0, 0.0, 1.0], side: Side::Leaving },
    Plan { n: [1.0, 0.0, 0.0], u: [0.0, 0.0, -1.0], v: [0.0, 1.0, 0.0], side: Side::Blank },
    Plan { n: [-1.0, 0.0, 0.0], u: [0.0, 0.0, 1.0], v: [0.0, 1.0, 0.0], side: Side::Blank },
    Plan { n: [0.0, 1.0, 0.0], u: [1.0, 0.0, 0.0], v: [0.0, 0.0, -1.0], side: Side::Blank },
    Plan { n: [0.0, 0.0, -1.0], u: [-1.0, 0.0, 0.0], v: [0.0, 1.0, 0.0], side: Side::Blank },
];

fn mul(a: &Mat, b: &Mat) -> Mat {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

fn apply(m: &Mat, p: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| m[i][0] * p[0] + m[i][1] * p[1] + m[i][2] * p[2])
}

fn about_x(a: f32) -> Mat {
    let (s, c) = a.sin_cos();
    [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]]
}

fn about_y(a: f32) -> Mat {
    let (s, c) = a.sin_cos();
    [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]]
}

/// A turn of `a` about `axis` (any length).
fn about(axis: [f32; 3], a: f32) -> Mat {
    let l = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
    let [x, y, z] = axis.map(|k| k / l);
    let (s, c) = a.sin_cos();
    let t = 1.0 - c;
    [
        [t * x * x + c, t * x * y - s * z, t * x * z + s * y],
        [t * x * y + s * z, t * y * y + c, t * y * z - s * x],
        [t * x * z - s * y, t * y * z + s * x, t * z * z + c],
    ]
}

/// How the cube stands `roll` of the way back through a roll: 0 at rest
/// with the glyph's face forward, 1 with the last glyph's face forward and
/// the new one still on top.
pub fn pose(roll: f32) -> Mat {
    let rest = mul(&about_x(TIP), &about_y(TURN));
    mul(&rest, &about_x(-FRAC_PI_2 * roll.clamp(0.0, 1.0)))
}

/// How a little subagent cube tumbles `secs` into its life, each its own
/// way by `seed`.
pub fn tumble(secs: f32, seed: u32) -> Mat {
    let k = seed as f32;
    let axis = [1.0, 0.6 + 0.3 * (k * 1.7).sin(), 0.3 + 0.2 * (k * 2.3).cos()];
    mul(&pose(0.0), &about(axis, secs * 2.4 + k * 1.3))
}

/// The faces of a cube turned by `m`, `half` its half side in DIPs, round
/// `(cx, cy)`: only those that face you, farthest first.
pub fn faces(m: &Mat, (cx, cy): (f32, f32), half: f32) -> Vec<Face> {
    let light = {
        let l = (LIGHT[0] * LIGHT[0] + LIGHT[1] * LIGHT[1] + LIGHT[2] * LIGHT[2]).sqrt();
        LIGHT.map(|k| k / l)
    };
    let project = |p: [f32; 3]| {
        let q = apply(m, p);
        let k = CAMERA / (CAMERA - q[2]);
        (cx + q[0] * half * k, cy - q[1] * half * k)
    };
    let mut out: Vec<Face> = PLANS
        .iter()
        .filter_map(|f| {
            let n = apply(m, f.n);
            if n[2] <= 0.001 {
                return None;
            }
            let at = |a: f32, b: f32| {
                project(std::array::from_fn(|i| f.n[i] + f.u[i] * a + f.v[i] * b))
            };
            Some(Face {
                side: f.side,
                corners: [at(-1.0, -1.0), at(1.0, -1.0), at(1.0, 1.0), at(-1.0, 1.0)],
                light: (n[0] * light[0] + n[1] * light[1] + n[2] * light[2]).max(0.0),
                facing: n[2].min(1.0),
                depth: n[2],
            })
        })
        .collect();
    out.sort_by(|a, b| a.depth.total_cmp(&b.depth));
    out
}

/// What lays a glyph flat on `f`: a point `(x, y)` from -1 to 1 across the
/// face, `y` down, lands at `(ox + x * rx + y * dx, oy + x * ry + y * dy)`.
/// Returned as `[rx, ry, dx, dy, ox, oy]`, the order of a Direct2D matrix.
/// Exact at the middle and the edges' middles; the slight swell of
/// perspective across the face is left out, which a glyph this small hides.
pub fn glyph_frame(f: &Face) -> [f32; 6] {
    let [bl, br, tr, tl] = f.corners;
    let o = ((bl.0 + br.0 + tr.0 + tl.0) / 4.0, (bl.1 + br.1 + tr.1 + tl.1) / 4.0);
    let right = ((br.0 + tr.0 - bl.0 - tl.0) / 4.0, (br.1 + tr.1 - bl.1 - tl.1) / 4.0);
    let down = ((bl.0 + br.0 - tl.0 - tr.0) / 4.0, (bl.1 + br.1 - tl.1 - tr.1) / 4.0);
    [right.0, right.1, down.0, down.1, o.0, o.1]
}

/// Where one of the little cubes circles the big one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Orbiter {
    pub dx: f32,
    pub dy: f32,
    /// How large, from perspective: over 1 in front, under 1 behind.
    pub scale: f32,
    /// Behind the big cube, so drawn before it.
    pub behind: bool,
}

/// The `k`th of `n` little cubes `turn` of the way round its ring (0 to 1),
/// `reach` out from the middle, `gather` of the way in toward it (0 out
/// on the ring, 1 fallen into the cube). Spaced evenly so two read as two.
/// The ring is tipped as the cube is, so its near side passes low.
pub fn orbit(k: usize, n: usize, turn: f32, reach: f32, gather: f32) -> Orbiter {
    let a = TAU * (turn + k as f32 / n.max(1) as f32);
    let r = reach * (1.0 - gather.clamp(0.0, 1.0));
    let (s, c) = a.sin_cos();
    Orbiter {
        dx: c * r,
        dy: s * r * 0.3,
        scale: 1.0 + 0.12 * s,
        behind: s < 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn side_of(fs: &[Face], side: Side) -> Option<Face> {
        fs.iter().copied().find(|f| f.side == side)
    }

    #[test]
    fn at_rest_the_glyph_faces_you_with_the_top_and_a_side_beside_it() {
        let fs = faces(&pose(0.0), (0.0, 0.0), 10.0);
        assert_eq!(fs.len(), 3);
        let front = side_of(&fs, Side::Front).expect("front shows");
        assert!(side_of(&fs, Side::Leaving).is_none());
        assert!(fs.iter().all(|f| f.facing <= front.facing), "front is the most squarely seen");
        assert_eq!(fs.last().map(|f| f.side), Some(Side::Front), "drawn last");
    }

    #[test]
    fn a_roll_starts_on_the_last_glyph_with_the_new_one_on_top() {
        let fs = faces(&pose(1.0), (0.0, 0.0), 10.0);
        let leaving = side_of(&fs, Side::Leaving).expect("the last glyph faces you");
        let front = side_of(&fs, Side::Front).expect("the new glyph shows on top");
        assert!(leaving.facing > front.facing);
        let top = front.corners.iter().map(|c| c.1).sum::<f32>();
        let mid = leaving.corners.iter().map(|c| c.1).sum::<f32>();
        assert!(top < mid, "the new face sits above, y being down");
    }

    #[test]
    fn a_glyph_on_the_front_stands_upright() {
        let fs = faces(&pose(0.0), (50.0, 30.0), 10.0);
        let [rx, ry, dx, dy, ox, oy] = glyph_frame(&side_of(&fs, Side::Front).unwrap());
        assert!(rx > 5.0 && ry.abs() < rx, "right runs right");
        assert!(dy > 5.0 && dx.abs() < dy, "down runs down");
        assert!((ox - 50.0).abs() < 8.0 && (oy - 30.0).abs() < 8.0);
    }

    #[test]
    fn the_same_moment_draws_the_same_cube() {
        assert_eq!(faces(&tumble(1.5, 2), (0.0, 0.0), 3.0), faces(&tumble(1.5, 2), (0.0, 0.0), 3.0));
    }

    #[test]
    fn orbiters_spread_round_the_ring_and_fall_in() {
        let a = orbit(0, 2, 0.0, 10.0, 0.0);
        let b = orbit(1, 2, 0.0, 10.0, 0.0);
        assert!((a.dx + b.dx).abs() < 1e-4, "opposite sides");
        let fallen = orbit(0, 1, 0.3, 10.0, 1.0);
        assert!(fallen.dx.abs() < 1e-4 && fallen.dy.abs() < 1e-4);
    }
}
