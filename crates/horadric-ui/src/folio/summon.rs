//! The Folio opening. Called up from its tile, the conversation is
//! summoned onto the glass: a seam of light cuts across the middle, the
//! page unfolds out of it with an edge of light going each way, the edges
//! shed runes that drift off and fade, and the Folio's own sigil flares
//! in the seam and goes out.
//!
//! The page is the kept layer, drawn once, so a frame of the opening is a
//! clipped blit and a few lines: as cheap as a tick of the runes. Like the
//! burning of a cleared page, all of it is a function of the time alone.

use std::f32::consts::{PI, TAU};

use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
};
use windows_numerics::Vector2;

use super::view::{d2d, Gfx, Palette};
use crate::folio_look as look;
use crate::layout::Rect;
use crate::motion;
use crate::render::color;
use crate::sparks::RUNES;

/// The seam growing from the middle to the column's edges, in seconds.
const SEAM: f32 = 0.25;
/// When the page starts to open, a little into the seam so the two read
/// as one motion.
const OPEN_AT: f32 = 0.1;
/// The page opening from the seam to the glass's edges.
const OPEN: f32 = 0.55;
/// A rune drifting away from the edge that shed it, and fading.
const DRIFT: f32 = 0.56;
/// The sigil flaring in the seam and going out.
const SIGIL: f32 = 0.52;
/// Runes are shed only over this share of the opening, so the last is
/// gone soon after the page is open.
const SHED_SPAN: f32 = 0.8;
/// From the open to the last rune gone.
pub(super) const LENGTH: f32 = OPEN_AT + OPEN * SHED_SPAN + DRIFT;
/// About one rune per this many DIPs of an edge.
const RUNE_EVERY: f32 = 38.0;
/// How far past the column the light reaches.
const REACH: f32 = 24.0;

/// Stable noise from 0 to 1 for rune `i` of edge `edge`.
fn noise(i: usize, edge: u32, salt: u32) -> f32 {
    let mut h = (i as u32).wrapping_mul(0x9E37_79B1)
        ^ edge.wrapping_mul(0x85EB_CA77)
        ^ salt.wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    (h % 10_000) as f32 / 10_000.0
}

/// How far open the page is at `t`, from 0 to 1. Soft at both ends: an
/// opening that starts at full speed reads as a flash.
fn opened(t: f32) -> f32 {
    let p = ((t - OPEN_AT) / OPEN).clamp(0.0, 1.0);
    p * p * p * (p * (p * 6.0 - 15.0) + 10.0)
}

/// Half the seam's length at `t`, as a share of its full half.
fn seam(t: f32) -> f32 {
    motion::ease_in_out(t / SEAM)
}

/// The frame while the Folio opens, in place of the kept layer `still`
/// drawn whole: over the runes behind, under the busy dots. `t` is
/// seconds since it began; `sigil` picks the rune that flares.
pub(super) unsafe fn draw(gfx: &Gfx, pal: &Palette, still: &ID2D1Bitmap, t: f32, sigil: u32) {
    let rt = &gfx.rt;
    let size = rt.GetSize();
    let (w, h) = (size.width, size.height);
    let mid = h * 0.5;
    let open = opened(t);
    let (cx, cw) = look::column(w);
    let (left, right) = ((cx - REACH).max(0.0), (cx + cw + REACH).min(w));
    let centre = (left + right) / 2.0;
    let ink = pal.accent.mix(pal.fg, 0.35);

    // The page, between the edges, settling from a touch smaller and
    // fainter so it seems to come up from behind the glass.
    let half = mid * open;
    if half > 0.0 {
        let band = Rect::new(0.0, mid - half, w, half * 2.0);
        rt.PushAxisAlignedClip(&d2d(&band), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        let s = 0.965 + 0.035 * open;
        let to = Rect::new(w * (1.0 - s) / 2.0, mid - mid * s, w * s, h * s);
        rt.DrawBitmap(
            still,
            Some(&d2d(&to)),
            open.sqrt(),
            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
            None,
        );
        rt.PopAxisAlignedClip();
    }

    // The edges: one line while the seam grows, then two going apart,
    // fading as they near the glass's edges.
    let reach = (right - left) / 2.0 * seam(t);
    let lit = (1.0 - open).powf(1.3) * motion::ease_in_out(t / 0.15) * 0.75;
    if lit > 0.01 && reach > 0.0 {
        let line = |y: f32, alpha: f32, width: f32| {
            gfx.brush.SetColor(&color(ink.with_alpha(alpha)));
            rt.DrawLine(
                Vector2 {
                    X: centre - reach,
                    Y: y,
                },
                Vector2 {
                    X: centre + reach,
                    Y: y,
                },
                &gfx.brush,
                width,
                &gfx.round,
            );
        };
        for y in [mid - half, mid + half] {
            line(y, lit * 0.8, 1.2);
            // A brighter, shorter core, so the edge is hottest where the
            // seam first opened.
            gfx.brush
                .SetColor(&color(pal.fg.mix(ink, 0.5).with_alpha(lit * 0.6)));
            let core = reach * 0.35;
            rt.DrawLine(
                Vector2 {
                    X: centre - core,
                    Y: y,
                },
                Vector2 {
                    X: centre + core,
                    Y: y,
                },
                &gfx.brush,
                1.3,
                &gfx.round,
            );
        }
    }

    // Runes shed by the edges as they pass, drifting on the way the edge
    // went and fading.
    let count = ((right - left) / RUNE_EVERY).ceil().max(1.0) as usize;
    for edge in 0..2u32 {
        let away = if edge == 0 { -1.0 } else { 1.0 };
        for i in 0..count {
            let n = |k| noise(i, edge, sigil.wrapping_add(k));
            let born = OPEN_AT + OPEN * SHED_SPAN * n(0);
            let q = (t - born) / DRIFT;
            if !(0.0..1.0).contains(&q) {
                continue;
            }
            let x = left + (i as f32 + n(1)) * (right - left) / count as f32;
            // Only where the seam had reached by then.
            if (x - centre).abs() > (right - left) / 2.0 * seam(born) {
                continue;
            }
            let from = mid + away * mid * opened(born);
            let size = 6.0 + 6.0 * n(2);
            let y = from + away * (10.0 + 30.0 * n(3)) * motion::ease_out(q) - size / 2.0;
            let sway = (q * 3.0 + n(4) * TAU).sin() * 4.0 * q;
            let alpha = (1.0 - q).powf(1.5) * motion::ease_in_out(q * 5.0) * 0.7;
            let which = (n(5) * RUNES.len() as f32) as usize;
            gfx.rune(
                which,
                x + sway,
                y,
                size,
                ink.with_alpha(alpha),
                (size * 0.13).max(0.9),
            );
        }
    }

    // The Folio's sigil, flaring in the seam as it opens.
    let q = t / SIGIL;
    if (0.0..1.0).contains(&q) {
        let size = 16.0 + 18.0 * motion::ease_out(q);
        let alpha = (q * PI).sin().powi(2) * 0.6;
        gfx.rune(
            sigil as usize,
            centre,
            mid - size / 2.0,
            size,
            pal.fg.mix(ink, 0.4).with_alpha(alpha),
            (size * 0.08).max(1.2),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_opens_once_and_stays_open() {
        assert_eq!(opened(0.0), 0.0);
        assert_eq!(opened(OPEN_AT), 0.0);
        assert!((opened(OPEN_AT + OPEN / 2.0) - 0.5).abs() < 1e-3);
        assert_eq!(opened(OPEN_AT + OPEN), 1.0);
        assert_eq!(opened(LENGTH), 1.0);
        let steps: Vec<f32> = (0..50).map(|i| opened(i as f32 * 0.01)).collect();
        assert!(steps.windows(2).all(|p| p[0] <= p[1]));
    }

    #[test]
    fn the_last_rune_is_gone_by_the_end() {
        let last = OPEN_AT + OPEN * SHED_SPAN;
        assert!(last + DRIFT <= LENGTH + 1e-6);
        assert!(OPEN_AT + OPEN < LENGTH);
    }
}
