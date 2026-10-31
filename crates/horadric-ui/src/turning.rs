//! The icon's rune stone turning, for a hireling's portrait in the
//! retinue. Ray marched like the icon, so baked: `bake.py turning` renders
//! the stone at every [`STEP_DEG`] round from the icon's own angle, each
//! frame as the stone and, apart, the light of its runes, so the portrait
//! lights them in its state's colour.

/// Must match TURN_FRAMES, TURN_STEP and TURN_SIZE in bake.py.
pub const FRAMES: usize = 72;
pub const STEP_DEG: f32 = 5.0;
pub const SIZE: usize = 64;

/// Per frame, per pixel: the stone premultiplied as red, green, blue and
/// alpha, then its runes' light.
static DATA: &[u8] = include_bytes!("../assets/icon/turning.bin");

/// The frame nearest a stone turned `deg` past the icon's angle.
pub fn frame_at(deg: f32) -> usize {
    ((deg / STEP_DEG).round() as i64).rem_euclid(FRAMES as i64) as usize
}

/// Frame `i` as two images of premultiplied BGRA rows: the stone, and its
/// runes' light in the alpha alone.
pub fn frame(i: usize) -> (Vec<u8>, Vec<u8>) {
    let n = SIZE * SIZE;
    let data = &DATA[(i % FRAMES) * n * 5..][..n * 5];
    let mut stone = Vec::with_capacity(n * 4);
    let mut light = Vec::with_capacity(n * 4);
    for p in data.chunks_exact(5) {
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
    }

    #[test]
    fn a_whole_turn_comes_back_to_rest() {
        assert_eq!(frame_at(0.0), 0);
        assert_eq!(frame_at(360.0), 0);
        assert_eq!(frame_at(-STEP_DEG), FRAMES - 1);
        assert_eq!(frame_at(STEP_DEG * 1.4), 1);
    }
}
