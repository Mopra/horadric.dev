//! The icon's rune stone turning, for a hireling's portrait in the
//! retinue and a session's tile. Ray marched like the icon, so baked:
//! `bake.py turning` renders the stone at every [`STEP_DEG`] round from the
//! icon's own angle, and `bake.py tumbling` it rolling forward once from
//! each quarter of that turn, each frame as the stone and, apart, the
//! light of its runes, so it is lit in its state's colour.

/// Must match TURN_FRAMES, TURN_STEP and TURN_SIZE in bake.py.
pub const FRAMES: usize = 72;
pub const STEP_DEG: f32 = 5.0;
pub const SIZE: usize = 64;

/// Per frame, per pixel: the stone premultiplied as red, green, blue and
/// alpha, then its runes' light.
static DATA: &[u8] = include_bytes!("../assets/icon/turning.bin");

/// Must match TUMBLE_QUARTERS and TUMBLE_STEPS in bake.py.
pub const TUMBLE_QUARTERS: usize = 4;
pub const TUMBLE_STEPS: usize = 18;

/// The stone rolling forward: for each quarter of its turn, the frames
/// after it stands, as `DATA` holds them.
static TUMBLE: &[u8] = include_bytes!("../assets/icon/tumbling.bin");

/// Every frame there is: the turn's, then each quarter's tumble.
pub const ALL: usize = FRAMES + TUMBLE_QUARTERS * (TUMBLE_STEPS - 1);

/// The frame nearest a stone turned `deg` past the icon's angle.
pub fn frame_at(deg: f32) -> usize {
    ((deg / STEP_DEG).round() as i64).rem_euclid(FRAMES as i64) as usize
}

/// The frame of a stone `step` of [`TUMBLE_STEPS`] round its forward roll
/// from quarter `quarter` of its turn. Step 0 and a whole roll are the
/// stone standing, which is a frame of the turn.
pub fn tumble_at(quarter: usize, step: usize) -> usize {
    let quarter = quarter % TUMBLE_QUARTERS;
    match step % TUMBLE_STEPS {
        0 => frame_at(90.0 * quarter as f32),
        s => FRAMES + quarter * (TUMBLE_STEPS - 1) + s - 1,
    }
}

/// Frame `i` of [`ALL`] as two images of premultiplied BGRA rows: the
/// stone, and its runes' light in the alpha alone.
pub fn frame(i: usize) -> (Vec<u8>, Vec<u8>) {
    let n = SIZE * SIZE;
    let i = i % ALL;
    let data = if i < FRAMES {
        &DATA[i * n * 5..][..n * 5]
    } else {
        &TUMBLE[(i - FRAMES) * n * 5..][..n * 5]
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
        assert_eq!(TUMBLE.len(), TUMBLE_QUARTERS * (TUMBLE_STEPS - 1) * SIZE * SIZE * 5);
    }

    #[test]
    fn a_roll_starts_and_ends_on_the_standing_stone() {
        assert_eq!(tumble_at(1, 0), frame_at(90.0));
        assert_eq!(tumble_at(1, TUMBLE_STEPS), frame_at(90.0));
        assert_eq!(tumble_at(0, 1), FRAMES);
        assert_eq!(tumble_at(3, TUMBLE_STEPS - 1), ALL - 1);
    }

    #[test]
    fn a_whole_turn_comes_back_to_rest() {
        assert_eq!(frame_at(0.0), 0);
        assert_eq!(frame_at(360.0), 0);
        assert_eq!(frame_at(-STEP_DEG), FRAMES - 1);
        assert_eq!(frame_at(STEP_DEG * 1.4), 1);
    }
}
