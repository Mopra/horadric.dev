//! The Horadric icon: a block of dark rune stone, a rune cut into each face
//! and lit from inside. Too heavy to draw at run time, it is ray marched once
//! by `assets/icon/stone.html` and baked by `assets/icon/bake.py`.
//!
//! The small sizes are baked as two layers, the stone and the light of its
//! runes, so one colour still lights it all and the tray can change and
//! breathe it. The large sizes are finished PNGs.
//!
//! Self contained on purpose: the `horadric` build script includes this file
//! to bake the same icon into the executables, so it can not reach the rest
//! of the crate. A dev instance's light is the error red instead of gold, so
//! the two tray icons can not be mistaken.

pub const GOLD: u32 = 0xE8B04A;
pub const ERROR: u32 = 0xFF5D66;

/// The sizes `layers.bin` holds, smallest first. Each is `size * size`
/// pixels of six bytes: the stone premultiplied as red, green, blue and
/// alpha, then the rune light, then the alpha of the light's halo.
const LAYERED: [u32; 9] = [16, 20, 24, 32, 40, 48, 64, 96, 128];
static LAYERS: &[u8] = include_bytes!("../assets/icon/layers.bin");

/// The large sizes, finished, gold and the dev instance's red.
pub fn png(size: u32, dev: bool) -> Option<&'static [u8]> {
    Some(match (size, dev) {
        (256, false) => include_bytes!("../assets/icon/gold-256.png"),
        (512, false) => include_bytes!("../assets/icon/gold-512.png"),
        (1024, false) => include_bytes!("../assets/icon/gold-1024.png"),
        (256, true) => include_bytes!("../assets/icon/red-256.png"),
        (512, true) => include_bytes!("../assets/icon/red-512.png"),
        (1024, true) => include_bytes!("../assets/icon/red-1024.png"),
        _ => return None,
    })
}

/// Pixels, row by row from the top, as `0xAARRGGBB` with straight alpha,
/// which is what a 32 bit icon bitmap wants.
pub fn pixels(size: u32) -> Vec<u32> {
    lit(size, GOLD)
}

/// The dev instance's icon.
pub fn dev_pixels(size: u32) -> Vec<u32> {
    lit(size, ERROR)
}

/// The stone lit by any colour, given as `0xRRGGBB`. Sizes between the baked
/// ones are filtered down from the next larger.
pub fn lit(size: u32, light: u32) -> Vec<u32> {
    let size = size.max(1);
    let (baked, offset) = layered_for(size);
    let n = (baked * baked) as usize;
    let data = &LAYERS[offset..offset + n * 6];
    let l = [(light >> 16) & 0xff, (light >> 8) & 0xff, light & 0xff].map(|c| c as f32 / 255.0);
    // The halo fades with the light, so a dimmed light does not leave a ring.
    let k = l[0].max(l[1]).max(l[2]);
    let premul: Vec<[f32; 4]> = (data.as_chunks::<6>().0.iter())
        .map(|p| {
            let [r, g, b, a, glow, halo] = p.map(|v| v as f32 / 255.0);
            let a = (a + halo * k).min(1.0);
            [
                (r + glow * l[0]).min(a),
                (g + glow * l[1]).min(a),
                (b + glow * l[2]).min(a),
                a,
            ]
        })
        .collect();
    let premul = resample(&premul, baked as usize, size as usize);
    premul.iter().map(|&p| straight(p)).collect()
}

/// The stone with its runes on fire, `phase` of the way through one loop of
/// the flames: 0 and 1 are the same frame, so the frames cycle seamlessly.
///
/// The fire is the runes' own light, nothing laid over the icon: each stroke
/// burns unevenly with noise that rises through it, ember dark where it is
/// cool and white where it is hottest, and short tongues lick up off the
/// strokes. The tongues stay on the stone, since colour is clamped to its
/// alpha.
pub fn burning(size: u32, light: u32, phase: f32) -> Vec<u32> {
    let size = size.max(1);
    let phase = phase.rem_euclid(1.0);
    let (baked, offset) = layered_for(size);
    let b = baked as usize;
    let data = &LAYERS[offset..offset + b * b * 6];
    let l = [(light >> 16) & 0xff, (light >> 8) & 0xff, light & 0xff].map(|c| c as f32 / 255.0);
    let k = l[0].max(l[1]).max(l[2]);
    let glow: Vec<f32> = data
        .as_chunks::<6>()
        .0
        .iter()
        .map(|p| p[4] as f32 / 255.0)
        .collect();
    // Tongues about a tenth of the icon tall, never under two pixels or the
    // tray's 16 px icon would show none.
    let scale = size as f32 / baked as f32;
    let reach = ((size as f32 / 10.0).clamp(2.0, 6.0) / scale)
        .round()
        .max(1.0) as usize;
    let premul: Vec<[f32; 4]> = (0..b * b)
        .map(|i| {
            let (x, y) = (i % b, i / b);
            let [r, g, bl, a, gl, halo] =
                [0, 1, 2, 3, 4, 5].map(|c| data[i * 6 + c] as f32 / 255.0);
            let n = flame(x as f32 / b as f32, y as f32 / b as f32, phase);
            let lick = (1..=reach)
                .filter(|d| y + d < b)
                .map(|d| glow[(y + d) * b + x] * (1.0 - d as f32 / (reach + 1) as f32))
                .fold(0.0, f32::max);
            let heat = gl * (0.5 + 0.9 * n) + lick * smooth(0.5, 0.9, n) * 0.9;
            let fire = ramp(l, heat);
            let a = (a + halo * k * (0.6 + 0.5 * n)).min(1.0);
            [
                (r + fire[0]).min(a),
                (g + fire[1]).min(a),
                (bl + fire[2]).min(a),
                a,
            ]
        })
        .collect();
    let premul = resample(&premul, b, size as usize);
    premul.iter().map(|&p| straight(p)).collect()
}

/// How the flames burn at `(u, v)`, both 0 to 1 across the icon, from 0
/// to 1. Two octaves of value noise scroll up through the icon, the finer one
/// twice as fast, each wrapping vertically after a whole number of loops so
/// `phase` 1 meets `phase` 0. A flicker shared by the whole fire rides on
/// top.
fn flame(u: f32, v: f32, phase: f32) -> f32 {
    use std::f32::consts::TAU;
    const CELLS: f32 = 6.0;
    let sway = 0.35 * (TAU * phase + v * 5.0).sin();
    let coarse = value_noise(u * CELLS + sway, (v + phase) * CELLS, CELLS as i32, 11);
    let fine = value_noise(
        u * CELLS * 2.0 - sway,
        (v + 2.0 * phase) * CELLS * 2.0,
        2 * CELLS as i32,
        29,
    );
    let flicker = 0.1 * (TAU * 3.0 * phase).sin() + 0.06 * (TAU * 7.0 * phase + 1.3).sin();
    (0.65 * coarse + 0.35 * fine + flicker).clamp(0.0, 1.0)
}

/// Smooth value noise from 0 to 1, repeating every `period` cells in `y`.
fn value_noise(x: f32, y: f32, period: i32, seed: u32) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (sx, sy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let at = |dx: i32, dy: i32| lattice(x0 as i32 + dx, (y0 as i32 + dy).rem_euclid(period), seed);
    let top = at(0, 0) + (at(1, 0) - at(0, 0)) * sx;
    let bottom = at(0, 1) + (at(1, 1) - at(0, 1)) * sx;
    top + (bottom - top) * sy
}

fn lattice(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343)
        ^ (y as u32).wrapping_mul(0xd816_3841)
        ^ seed.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0xffff) as f32 / 65535.0
}

/// The fire's colour at this heat, premultiplied: dark ember red below
/// half, the light itself at 1, then on toward white.
fn ramp(l: [f32; 3], heat: f32) -> [f32; 3] {
    let ember = [l[0] * 0.8, l[1] * 0.3, l[2] * 0.12];
    let white = l.map(|c| c + (1.0 - c) * 0.75);
    let lerp = |a: [f32; 3], b: [f32; 3], t: f32| [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t);
    if heat < 0.5 {
        ember.map(|c| c * heat / 0.5)
    } else if heat < 1.0 {
        lerp(ember, l, (heat - 0.5) / 0.5)
    } else {
        lerp(l, white, ((heat - 1.0) / 0.3).min(1.0))
    }
}

fn smooth(from: f32, to: f32, x: f32) -> f32 {
    let t = ((x - from) / (to - from)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The smallest baked size not below `size`, and where it starts in
/// `LAYERS`. Above the largest, the largest.
fn layered_for(size: u32) -> (u32, usize) {
    let mut offset = 0;
    for &s in &LAYERED {
        if s >= size || s == LAYERED[LAYERED.len() - 1] {
            return (s, offset);
        }
        offset += (s * s) as usize * 6;
    }
    unreachable!("LAYERED is not empty")
}

/// Area filtered from `from` square to `to` square, each output pixel the
/// average of what it covers.
fn resample(src: &[[f32; 4]], from: usize, to: usize) -> Vec<[f32; 4]> {
    if from == to {
        return src.to_vec();
    }
    let taps = taps(from, to);
    let mut rows = vec![[0.0; 4]; from * to];
    for y in 0..from {
        for (x, tap) in taps.iter().enumerate() {
            rows[y * to + x] = mix(tap.iter().map(|&(i, w)| (src[y * from + i], w)));
        }
    }
    let mut out = vec![[0.0; 4]; to * to];
    for (y, tap) in taps.iter().enumerate() {
        for x in 0..to {
            out[y * to + x] = mix(tap.iter().map(|&(i, w)| (rows[i * to + x], w)));
        }
    }
    out
}

/// For each output index, the source indices it covers and their weights.
fn taps(from: usize, to: usize) -> Vec<Vec<(usize, f32)>> {
    let scale = from as f32 / to as f32;
    (0..to)
        .map(|i| {
            let (a, b) = (i as f32 * scale, (i + 1) as f32 * scale);
            let mut tap = Vec::new();
            let mut j = a.floor() as usize;
            while (j as f32) < b && j < from {
                let w = b.min(j as f32 + 1.0) - a.max(j as f32);
                if w > 0.0 {
                    tap.push((j, w));
                }
                j += 1;
            }
            let total: f32 = tap.iter().map(|t| t.1).sum();
            tap.iter_mut().for_each(|t| t.1 /= total);
            tap
        })
        .collect()
}

fn mix(samples: impl Iterator<Item = ([f32; 4], f32)>) -> [f32; 4] {
    samples.fold([0.0; 4], |mut acc, (p, w)| {
        for c in 0..4 {
            acc[c] += p[c] * w;
        }
        acc
    })
}

fn straight([r, g, b, a]: [f32; 4]) -> u32 {
    if a <= 0.0 {
        return 0;
    }
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    byte(a) << 24 | byte(r / a) << 16 | byte(g / a) << 8 | byte(b / a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(p: &[u32], size: u32, x: u32, y: u32) -> u32 {
        p[(y * size + x) as usize]
    }

    #[test]
    fn the_layers_hold_every_baked_size() {
        let total: u32 = LAYERED.iter().map(|s| s * s * 6).sum();
        assert_eq!(LAYERS.len(), total as usize);
    }

    #[test]
    fn the_corners_are_clear_and_the_middle_is_stone() {
        for size in [16, 32, 48] {
            let p = pixels(size);
            assert_eq!(at(&p, size, 0, 0) >> 24, 0, "{size}");
            assert_eq!(at(&p, size, size / 2, size / 2) >> 24, 0xff, "{size}");
        }
    }

    #[test]
    fn the_light_only_changes_the_runes() {
        let (gold, red) = (pixels(32), dev_pixels(32));
        let differ = gold.iter().zip(&red).filter(|(a, b)| a != b).count();
        assert!(differ > 0);
        assert!(differ < gold.len() / 2);
    }

    #[test]
    fn scales_to_sizes_between_and_beyond() {
        for size in [1, 17, 28, 80, 200] {
            assert_eq!(lit(size, GOLD).len(), (size * size) as usize);
        }
    }

    #[test]
    fn resampling_keeps_a_flat_colour_flat() {
        let src = vec![[0.25, 0.5, 0.75, 1.0]; 20 * 20];
        for p in resample(&src, 20, 7) {
            for (c, want) in p.iter().zip([0.25, 0.5, 0.75, 1.0]) {
                assert!((c - want).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn the_flames_loop_and_move() {
        for size in [16, 32] {
            assert_eq!(burning(size, GOLD, 0.0), burning(size, GOLD, 1.0), "{size}");
            assert_ne!(burning(size, GOLD, 0.0), burning(size, GOLD, 0.3), "{size}");
        }
    }

    #[test]
    fn the_fire_stays_on_the_stone() {
        let (rest, fire) = (pixels(32), burning(32, GOLD, 0.4));
        for (a, b) in rest.iter().zip(&fire) {
            if a >> 24 == 0 {
                assert_eq!(b >> 24, 0);
            }
        }
    }

    #[test]
    fn the_large_sizes_are_pngs() {
        for dev in [false, true] {
            for size in [256, 512, 1024] {
                assert!(png(size, dev).unwrap().starts_with(b"\x89PNG"));
            }
        }
        assert!(png(32, false).is_none());
    }
}
