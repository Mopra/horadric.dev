//! The page clearing. When `/clear` starts a new conversation the old one
//! burns away from the composer up: an edge climbs the glass, every piece
//! of the page it passes lifts off as ash and fades, some shed a rune that
//! rises into the background, and the empty page shows below the edge.
//!
//! The old page is the kept layer as it was at the clear, so nothing of
//! the old conversation is laid out again. Like the runes behind it, all
//! of it is a function of the time alone.

use std::f32::consts::TAU;
use std::time::Instant;

use windows::Win32::Graphics::Direct2D::{ID2D1Bitmap, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR};

use super::view::{d2d, Gfx, Palette};
use crate::folio_look as look;
use crate::layout::Rect;
use crate::motion;
use crate::sparks::RUNES;

/// The edge climbing from the composer to the top, in seconds.
const SWEEP: f32 = 0.7;
/// How much later than the edge a piece may go, so the edge is ragged.
const JITTER: f32 = 0.18;
/// A piece lifting off and fading.
const ASH: f32 = 0.55;
/// A rune rising from where its piece was, and fading.
const RISE: f32 = 1.1;
/// From the clear to the last rune gone.
pub(super) const LENGTH: f32 = SWEEP + JITTER + RISE;
/// A piece of the page. Wide and low, so a line of text breaks into a few
/// pieces and not into dust.
const CELL_W: f32 = 26.0;
const CELL_H: f32 = 7.0;
/// The share of pieces that shed a rune.
const SHED: f32 = 0.14;

/// The old page, burning.
pub(super) struct Ghost {
    pub(super) page: ID2D1Bitmap,
    /// What burns: everything above the composer.
    pub(super) area: Rect,
    pub(super) since: Instant,
}

impl Ghost {
    pub(super) fn over(&self) -> bool {
        self.since.elapsed().as_secs_f32() >= LENGTH
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Piece {
    Whole,
    /// Lifting off, this far along.
    Ash(f32),
    Gone,
}

/// A piece the edge reaches at `reached`, at `t`.
fn piece(t: f32, reached: f32) -> Piece {
    match (t - reached) / ASH {
        p if p < 0.0 => Piece::Whole,
        p if p < 1.0 => Piece::Ash(p),
        _ => Piece::Gone,
    }
}

/// Stable noise from 0 to 1 for a piece.
fn noise(row: usize, col: usize, salt: u32) -> f32 {
    let mut h = (row as u32).wrapping_mul(0x9E37_79B1)
        ^ (col as u32).wrapping_mul(0x85EB_CA77)
        ^ salt.wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    (h % 10_000) as f32 / 10_000.0
}

/// The frame while the page burns, in place of the kept layer `still`
/// (the new page) drawn whole: over the runes, under the busy dots.
pub(super) unsafe fn draw(gfx: &Gfx, pal: &Palette, still: &ID2D1Bitmap, g: &Ghost) {
    let rt = &gfx.rt;
    let size = rt.GetSize();
    let (w, h) = (size.width, size.height);
    let t = g.since.elapsed().as_secs_f32();
    let a = g.area;
    let blit = |b: &ID2D1Bitmap, from: &Rect, to: &Rect, alpha: f32| {
        if from.w > 0.0 && from.h > 0.0 {
            rt.DrawBitmap(
                b,
                Some(&d2d(to)),
                alpha,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                Some(&d2d(from)),
            );
        }
    };

    // The new page: the composer at once, the rest below the edge.
    let front = a.bottom() - a.h * (t / SWEEP).min(1.0);
    let composer = Rect::new(0.0, a.bottom(), w, h - a.bottom());
    blit(still, &composer, &composer, 1.0);
    let shown = Rect::new(0.0, front, w, a.bottom() - front);
    blit(still, &shown, &shown, 1.0);

    // Either side of the column the old page holds only its edges, which
    // just end at the edge; inside it, it breaks up.
    let (cx, cw) = look::column(w);
    let (left, right) = ((cx - 24.0).max(0.0), (cx + cw + 24.0).min(w));
    for r in [
        Rect::new(0.0, a.y, left, front - a.y),
        Rect::new(right, a.y, w - right, front - a.y),
    ] {
        blit(&g.page, &r, &r, 1.0);
    }

    let rows = (a.h / CELL_H).ceil().max(1.0) as usize;
    let cols = ((right - left) / CELL_W).ceil().max(1.0) as usize;
    let cell_w = (right - left) / cols as f32;
    let ink = pal.fg.mix(pal.accent, 0.55);
    let mut runes = Vec::new();
    for row in 0..rows {
        let bottom = a.bottom() - row as f32 * CELL_H;
        let top = (bottom - CELL_H).max(a.y);
        let reached = SWEEP * row as f32 / rows as f32;
        if t < reached {
            let r = Rect::new(left, top, right - left, bottom - top);
            blit(&g.page, &r, &r, 1.0);
            continue;
        }
        if t > reached + JITTER + RISE {
            continue;
        }
        for col in 0..cols {
            let n = |salt| noise(row, col, salt);
            let x = left + col as f32 * cell_w;
            let r = Rect::new(x, top, cell_w, bottom - top);
            let at = reached + JITTER * n(0);
            match piece(t, at) {
                Piece::Whole => blit(&g.page, &r, &r, 1.0),
                Piece::Ash(p) => {
                    let e = motion::ease_out(p);
                    let to = Rect::new(
                        x + (n(1) - 0.5) * 20.0 * e,
                        top - (8.0 + 28.0 * n(2)) * e,
                        r.w,
                        r.h,
                    );
                    blit(&g.page, &r, &to, (1.0 - p).powi(2));
                }
                Piece::Gone => {}
            }
            let q = (t - at) / RISE;
            if n(3) < SHED && (0.0..1.0).contains(&q) {
                let size = 6.0 + 7.0 * n(6);
                let sway = (q * 4.0 + n(5) * TAU).sin() * 5.0 * q;
                let y = top - (40.0 + 70.0 * n(4)) * motion::ease_out(q);
                let alpha = (1.0 - q).powf(1.4) * (q * 10.0).min(1.0) * 0.9;
                let which = (n(7) * RUNES.len() as f32) as usize;
                runes.push((which, x + cell_w / 2.0 + sway, y, size, alpha));
            }
        }
    }
    // Over every piece, so none rises behind the page it left.
    for (which, x, y, size, alpha) in runes {
        let c = ink.with_alpha(alpha);
        gfx.rune(which, x, y, size, c, (size * 0.13).max(0.9));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_piece_is_whole_then_ash_then_gone() {
        assert_eq!(piece(0.1, 0.3), Piece::Whole);
        assert!(matches!(piece(0.3 + ASH / 2.0, 0.3), Piece::Ash(p) if (p - 0.5).abs() < 1e-4));
        assert_eq!(piece(0.3 + ASH, 0.3), Piece::Gone);
    }

    #[test]
    fn noise_stays_in_range_and_differs() {
        let all: Vec<f32> = (0..50).map(|i| noise(i, i * 3, 7)).collect();
        assert!(all.iter().all(|n| (0.0..1.0).contains(n)));
        assert!(all.windows(2).any(|w| w[0] != w[1]));
    }
}
