//! Where a session tile's rune stone and its subagents' little stones
//! stand: the stone turns a quarter for each new tool, and a little stone
//! circles it for each subagent. Pure, so every frame is a function of
//! its inputs alone; the drawing is in `render::keycube`.

use std::f32::consts::TAU;

/// How far round the stone stands, in degrees past the icon's angle,
/// after `turns` changes of icon with `turn` of the last quarter still to
/// go (1 the moment it changed, 0 once it stands). Eased, so it sets off
/// and settles like something heavy.
pub fn stone_deg(turns: u32, turn: f32) -> f32 {
    let left = ease_in_out(turn.clamp(0.0, 1.0));
    90.0 * ((turns % 4) as f32 - left)
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

/// A little stone at `turn` of the way round its ring (0 to 1), on an
/// ellipse `reach` wide and tall from the middle, `gather` of the way in
/// toward it (0 out on the ring, 1 fallen into the stone). The ring is
/// tipped toward you, so its near side passes low.
pub fn orbit(turn: f32, (rx, ry): (f32, f32), gather: f32) -> Orbiter {
    let a = TAU * turn;
    let k = 1.0 - gather.clamp(0.0, 1.0);
    let (s, c) = a.sin_cos();
    Orbiter {
        dx: c * rx * k,
        dy: s * ry * k,
        scale: 1.0 + 0.15 * s,
        behind: s < 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_new_tool_brings_the_next_face_round() {
        assert_eq!(stone_deg(0, 0.0), 0.0);
        assert_eq!(stone_deg(1, 0.0), 90.0);
        assert_eq!(stone_deg(1, 1.0), 0.0, "starts from where the last stood");
        assert_eq!(stone_deg(5, 0.0), 90.0, "four quarters come back round");
        let mid = stone_deg(2, 0.5);
        assert!(mid > 90.0 && mid < 180.0);
    }

    #[test]
    fn little_stones_pass_low_in_front_and_fall_in() {
        let front = orbit(0.25, (11.0, 4.0), 0.0);
        assert!(!front.behind && front.dy > 0.0 && front.scale > 1.0);
        assert!(orbit(0.75, (11.0, 4.0), 0.0).behind);
        let fallen = orbit(0.1, (11.0, 4.0), 1.0);
        assert!(fallen.dx.abs() < 1e-4 && fallen.dy.abs() < 1e-4);
    }
}
