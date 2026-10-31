//! A rune stone in 3D: a cube turned and tilted, seen straight on, its
//! visible faces made ready for a 2D brush. Drawn as a hireling's
//! portrait in the retinue, so the party reads as small stones of the
//! one in the icon.
//!
//! Seen straight on (no perspective) so each face lands as a
//! parallelogram, and whatever is drawn flat on a face (its rune) gets
//! there through one affine transform.

/// The icon stone's angle, in degrees: where a portrait rests.
pub const REST_TURN: f32 = 52.0;
pub const REST_TILT: f32 = 25.0;

/// One face the eye sees, in DIPs from the stone's centre, y down.
#[derive(Clone, Copy, Debug)]
pub struct Face {
    pub corners: [(f32, f32); 4],
    pub centre: (f32, f32),
    /// Where the face's own right and down go on the screen, per DIP of
    /// the face: what maps a flat rune onto it.
    pub right: (f32, f32),
    pub down: (f32, f32),
    /// How square on it faces the eye, 0 edge on to 1 full.
    pub facing: f32,
    /// How much of the light it catches, 0 to 1.
    pub light: f32,
    /// A side, which carries a rune; the top and bottom carry none.
    pub side: bool,
}

type V3 = (f32, f32, f32);

/// The faces of a stone `half` DIPs from centre to face, turned `turn`
/// degrees about its upright and then tilted `tilt` degrees toward the
/// eye, those the eye sees. x right, y up, z toward the eye.
pub fn faces(half: f32, turn: f32, tilt: f32) -> Vec<Face> {
    let (st, ct) = turn.to_radians().sin_cos();
    let (sp, cp) = tilt.to_radians().sin_cos();
    let spin = |(x, y, z): V3| -> V3 {
        let (x, z) = (x * ct + z * st, -x * st + z * ct);
        (x, y * cp - z * sp, y * sp + z * cp)
    };
    let screen = |(x, y, _): V3| (x, -y);
    // From above and to the left, a little toward the eye.
    let lamp = norm((-0.45, 0.75, 0.5));
    let sides = [(0.0, 0.0, 1.0), (1.0, 0.0, 0.0), (0.0, 0.0, -1.0), (-1.0, 0.0, 0.0)];
    let mut out = Vec::with_capacity(3);
    let mut add = |n: V3, u: V3, v: V3, side: bool| {
        let n2 = spin(n);
        if n2.2 <= 1e-3 {
            return;
        }
        let at = |a: f32, b: f32| {
            screen(spin((
                (n.0 + a * u.0 + b * v.0) * half,
                (n.1 + a * u.1 + b * v.1) * half,
                (n.2 + a * u.2 + b * v.2) * half,
            )))
        };
        out.push(Face {
            corners: [at(-1.0, -1.0), at(1.0, -1.0), at(1.0, 1.0), at(-1.0, 1.0)],
            centre: at(0.0, 0.0),
            right: screen(spin(u)),
            down: screen(spin(v)),
            facing: n2.2,
            light: dot(n2, lamp).max(0.0),
            side,
        });
    };
    for n in sides {
        // The face's right as one stands before it; down is down.
        add(n, (n.2, 0.0, -n.0), (0.0, -1.0, 0.0), true);
    }
    add((0.0, 1.0, 0.0), (1.0, 0.0, 0.0), (0.0, 0.0, 1.0), false);
    add((0.0, -1.0, 0.0), (1.0, 0.0, 0.0), (0.0, 0.0, -1.0), false);
    out
}

fn dot(a: V3, b: V3) -> f32 {
    a.0 * b.0 + a.1 * b.1 + a.2 * b.2
}

fn norm(a: V3) -> V3 {
    let l = dot(a, a).sqrt();
    (a.0 / l, a.1 / l, a.2 / l)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_on_shows_only_the_front() {
        let f = faces(10.0, 0.0, 0.0);
        assert_eq!(f.len(), 1);
        assert!(f[0].side);
        assert!((f[0].facing - 1.0).abs() < 1e-5);
        assert!((f[0].right.0 - 1.0).abs() < 1e-5 && (f[0].down.1 - 1.0).abs() < 1e-5);
    }

    #[test]
    fn at_rest_shows_two_sides_and_the_top() {
        let f = faces(10.0, REST_TURN, REST_TILT);
        assert_eq!(f.len(), 3);
        assert_eq!(f.iter().filter(|f| f.side).count(), 2);
        let top = f.iter().find(|f| !f.side).unwrap();
        // The top sits above the centre on the screen.
        assert!(top.centre.1 < 0.0);
    }

    #[test]
    fn fits_its_bound() {
        // A stone half 10 wide never reaches past the cube's corner radius.
        let r = 10.0 * 3f32.sqrt() + 1e-3;
        for turn in (0..360).step_by(15) {
            for f in faces(10.0, turn as f32, 30.0) {
                for (x, y) in f.corners {
                    assert!(x.hypot(y) <= r);
                }
            }
        }
    }
}
