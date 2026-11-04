//! The icon's rune stone turning, for a hireling's portrait in the
//! retinue and a session's tile. Ray marched like the icon, so baked:
//! `bake.py turning` renders the stone at every [`STEP_DEG`] round from the
//! icon's own angle, and `bake.py poses` the same for each other face up
//! and every roll away (see `tilecube`), each frame as the stone and,
//! apart, the light of its runes, so it is lit in its state's colour.

/// Must match TURN_FRAMES, TURN_STEP and TURN_SIZE in bake.py.
pub const FRAMES: usize = 72;
pub const STEP_DEG: f32 = 5.0;
pub const SIZE: usize = 64;

/// Per frame, per pixel: the stone premultiplied as red, green, blue and
/// alpha, then its runes' light.
static DATA: &[u8] = include_bytes!("../assets/icon/turning.bin");

/// How many ways up the stone is baked, and how many rolls; must match
/// SPUN, ROLLED and ROLL_STEP in bake.py.
pub const GROUPS: usize = 6;
pub const ROLL_STEP_DEG: f32 = 10.0;
pub const ROLL_FRAMES: usize = 36;

/// The turn of each way up but the first, which `DATA` is, then each
/// roll a whole turn round, as `DATA` holds frames.
static POSES: &[u8] = include_bytes!("../assets/icon/poses.bin");

/// Every frame there is: the turns', then the rolls'.
pub const ALL: usize = GROUPS * FRAMES + GROUPS * ROLL_FRAMES;

/// The frame nearest a stone turned `deg` past the icon's angle.
pub fn frame_at(deg: f32) -> usize {
    ((deg / STEP_DEG).round() as i64).rem_euclid(FRAMES as i64) as usize
}

/// The frame nearest a stone with face `up` up, turned `deg`.
pub fn spun_at(up: usize, deg: f32) -> usize {
    up % GROUPS * FRAMES + frame_at(deg)
}

/// The frame nearest roll `group` at `deg` round.
pub fn rolled_at(group: usize, deg: f32) -> usize {
    let step = ((deg / ROLL_STEP_DEG).round() as i64).rem_euclid(ROLL_FRAMES as i64) as usize;
    GROUPS * FRAMES + group % GROUPS * ROLL_FRAMES + step
}

/// Frame `i` of [`ALL`] as two images of premultiplied BGRA rows: the
/// stone, and its runes' light in the alpha alone.
pub fn frame(i: usize) -> (Vec<u8>, Vec<u8>) {
    let n = SIZE * SIZE;
    let i = i % ALL;
    let data = if i < FRAMES {
        &DATA[i * n * 5..][..n * 5]
    } else {
        &POSES[(i - FRAMES) * n * 5..][..n * 5]
    };
    let mut stone = Vec::with_capacity(n * 4);
    let mut light = Vec::with_capacity(n * 4);
    for p in data.as_chunks::<5>().0 {
        stone.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        light.extend_from_slice(&[0, 0, 0, p[4]]);
    }
    (stone, light)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bake_holds_every_frame() {
        assert_eq!(DATA.len(), FRAMES * SIZE * SIZE * 5);
        assert_eq!(POSES.len(), (ALL - FRAMES) * SIZE * SIZE * 5);
    }

    #[test]
    fn frames_land_in_their_groups() {
        assert_eq!(spun_at(0, 10.0), frame_at(10.0));
        assert_eq!(spun_at(5, -STEP_DEG), GROUPS * FRAMES - 1);
        assert_eq!(rolled_at(0, 0.0), GROUPS * FRAMES);
        assert_eq!(rolled_at(5, 350.0), ALL - 1);
        assert_eq!(rolled_at(2, 360.0), rolled_at(2, 0.0));
    }

    #[test]
    fn a_whole_turn_comes_back_to_rest() {
        assert_eq!(frame_at(0.0), 0);
        assert_eq!(frame_at(360.0), 0);
        assert_eq!(frame_at(-STEP_DEG), FRAMES - 1);
        assert_eq!(frame_at(STEP_DEG * 1.4), 1);
    }
}
