//! The Horadric icon, drawn in code: a blue square holding two dark tiles,
//! one amber (waiting) and one green (done). No icon file to ship, and it is
//! drawn at exactly the size the tray asks for.
//!
//! Self contained on purpose: the `horadric` build script includes this file
//! to bake the same icon into the executables, so it can not reach the rest
//! of the crate. The colours are the theme's. A dev instance frames it in
//! the error red instead, so the two tray icons can not be mistaken.

#[derive(Clone, Copy)]
struct Color {
    r: f32,
    g: f32,
    b: f32,
}

const fn rgb(hex: u32) -> Color {
    Color {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
    }
}

const WORKING: Color = rgb(0x3DB4FF);
const WAITING: Color = rgb(0xFFB224);
const DONE: Color = rgb(0x3DD68C);
const WINDOW_BG: Color = rgb(0x0A0A0D);
const ERROR: Color = rgb(0xFF5D66);

/// Pixels, row by row from the top, as `0xAARRGGBB` with straight alpha,
/// which is what a 32 bit icon bitmap wants.
pub fn pixels(size: u32) -> Vec<u32> {
    draw(size, WORKING)
}

/// The dev instance's icon.
pub fn dev_pixels(size: u32) -> Vec<u32> {
    draw(size, ERROR)
}

fn draw(size: u32, frame: Color) -> Vec<u32> {
    let u = size as f32 / 16.0;
    let shapes: [(Shape, Color); 5] = [
        (
            Shape::Rounded {
                x0: 0.5 * u,
                y0: 0.5 * u,
                x1: 15.5 * u,
                y1: 15.5 * u,
                r: 3.5 * u,
            },
            frame,
        ),
        (
            Shape::Rounded {
                x0: 2.5 * u,
                y0: 3.0 * u,
                x1: 13.5 * u,
                y1: 7.5 * u,
                r: 1.25 * u,
            },
            WINDOW_BG,
        ),
        (
            Shape::Rounded {
                x0: 2.5 * u,
                y0: 8.5 * u,
                x1: 13.5 * u,
                y1: 13.0 * u,
                r: 1.25 * u,
            },
            WINDOW_BG,
        ),
        (
            Shape::Circle {
                cx: 5.0 * u,
                cy: 5.25 * u,
                r: 1.4 * u,
            },
            WAITING,
        ),
        (
            Shape::Circle {
                cx: 5.0 * u,
                cy: 10.75 * u,
                r: 1.4 * u,
            },
            DONE,
        ),
    ];

    const SAMPLES: u32 = 4;
    let mut out = Vec::with_capacity((size * size) as usize);
    for py in 0..size {
        for px in 0..size {
            // Straight alpha compositing, back to front, averaged over a
            // grid of samples for smooth edges.
            let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = px as f32 + (sx as f32 + 0.5) / SAMPLES as f32;
                    let y = py as f32 + (sy as f32 + 0.5) / SAMPLES as f32;
                    let mut c = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
                    for (shape, color) in &shapes {
                        if shape.contains(x, y) {
                            c = (color.r, color.g, color.b, 1.0);
                        }
                    }
                    r += c.0 * c.3;
                    g += c.1 * c.3;
                    b += c.2 * c.3;
                    a += c.3;
                }
            }
            let n = (SAMPLES * SAMPLES) as f32;
            let alpha = a / n;
            let (r, g, b) = if a > 0.0 {
                (r / a, g / a, b / a)
            } else {
                (0.0, 0.0, 0.0)
            };
            let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
            out.push(byte(alpha) << 24 | byte(r) << 16 | byte(g) << 8 | byte(b));
        }
    }
    out
}

enum Shape {
    Rounded {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        r: f32,
    },
    Circle {
        cx: f32,
        cy: f32,
        r: f32,
    },
}

impl Shape {
    fn contains(&self, x: f32, y: f32) -> bool {
        match *self {
            Shape::Rounded { x0, y0, x1, y1, r } => {
                if x < x0 || x > x1 || y < y0 || y > y1 {
                    return false;
                }
                // Only the corners are round: measure from the nearest
                // corner circle's centre when inside its square.
                let dx = (x0 + r - x).max(x - (x1 - r)).max(0.0);
                let dy = (y0 + r - y).max(y - (y1 - r)).max(0.0);
                dx * dx + dy * dy <= r * r
            }
            Shape::Circle { cx, cy, r } => (x - cx).powi(2) + (y - cy).powi(2) <= r * r,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_are_clear_and_the_middle_is_solid() {
        let p = pixels(16);
        assert_eq!(p.len(), 256);
        assert_eq!(p[0] >> 24, 0, "rounded corner is transparent");
        assert_eq!(p[8 * 16 + 1] >> 24, 255, "edge of the square is opaque");
        // The top tile's dot is amber.
        let dot = p[5 * 16 + 5];
        assert_eq!(dot & 0xFFFFFF, 0xFFB224);
    }

    #[test]
    fn dev_icon_differs_only_in_the_frame() {
        let (p, d) = (pixels(16), dev_pixels(16));
        assert_eq!(p[8 * 16 + 1] & 0xFFFFFF, 0x3DB4FF);
        assert_eq!(d[8 * 16 + 1] & 0xFFFFFF, 0xFF5D66);
        assert_eq!(p[5 * 16 + 5], d[5 * 16 + 5], "the dots are the same");
    }

    #[test]
    fn scales_to_any_size() {
        assert_eq!(pixels(32).len(), 32 * 32);
        assert_eq!(pixels(20).len(), 400);
    }
}
