//! Shadow and shade through the GPU's shadow effect: a key's shadow on the
//! plate, the bevel round its face and the hollow shade inside a sunk or
//! latched one. Each is a real blur of the shape's alpha, where a stack of
//! eight shapes each a little bigger only ever read as bands.
//!
//! A shade is baked once per shape into a bitmap the target keeps, and from
//! then on costs one bitmap drawn: a blur on every redraw of the still layer
//! would cost the UI thread more than the eight shapes did.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::mem::ManuallyDrop;

use windows::core::{Interface, Result};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_COMPOSITE_MODE_SOURCE_OVER,
    D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_F,
};
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1Shadow, ID2D1Bitmap, ID2D1BitmapRenderTarget, ID2D1DeviceContext, ID2D1Geometry,
    ID2D1Image, ID2D1RenderTarget, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_LAYER_OPTIONS_NONE, D2D1_LAYER_PARAMETERS, D2D1_PROPERTY_TYPE_FLOAT,
    D2D1_PROPERTY_TYPE_VECTOR4, D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION, D2D1_SHADOW_PROP_COLOR,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows_numerics::{Matrix3x2, Vector2};

use super::{color, rounded, Gpu, Painter};
use crate::layout::Rect;
use crate::theme::Color;

/// Baked shades by what they are, kept with a window's target, each with
/// when it was last drawn.
#[derive(Default)]
pub(super) struct Shades {
    kept: RefCell<HashMap<Vec<u32>, (ID2D1Bitmap, u64)>>,
    clock: Cell<u64>,
}

/// Enough for every key, field and screen a window has at each depth and
/// a few sub-pixel places. Past it the one drawn longest ago goes, so a
/// full cache never makes every key bake again at once.
const SHADES_KEPT: usize = 160;

/// How finely a shadow's blur follows a key's depth, in DIPs. A key lifted
/// under the cursor or landing changes its depth every frame, and a shade
/// baked per frame is a blur per frame on the UI thread; steps this fine
/// are not seen.
const BLUR_STEP: f32 = 1.0;

/// `blur` to the step its shadow is baked at.
pub(super) fn stepped(blur: f32) -> f32 {
    (blur.max(0.0) / BLUR_STEP).round() * BLUR_STEP
}

/// The key of the shade in `kept` drawn longest ago.
fn oldest<K: Clone, V>(kept: &HashMap<K, (V, u64)>) -> Option<K> {
    kept.iter().min_by_key(|(_, (_, at))| *at).map(|(k, _)| k.clone())
}

/// The Gaussian that softens an edge as much as the old stack of shapes
/// `blur` wide did: a box that wide has a standard deviation of
/// `blur / sqrt(12)`.
pub(super) fn sigma(blur: f32) -> f32 {
    blur.max(0.0) / 12f32.sqrt()
}

/// How far past its shape a shade is kept: three sigmas, where a Gaussian
/// is all but gone, the furthest it is moved, and a pixel for the edge.
fn pad(sigma: f32, moved: f32) -> f32 {
    (3.0 * sigma + moved.abs() + 1.0).ceil()
}

/// Where a bitmap meant to sit at `at` DIPs is drawn so its pixels fall on
/// the target's, at `scale` pixels per DIP, and how far inside it the shape
/// is drawn to make up the difference, to a quarter pixel. A bevel's lit
/// line is a pixel wide, and a bitmap drawn between pixels smears it.
fn snap(at: f32, scale: f32) -> (f32, f32) {
    let px = at * scale;
    let mut whole = px.floor();
    let mut part = ((px - whole) * 4.0).round() / 4.0;
    if part >= 1.0 {
        whole += 1.0;
        part = 0.0;
    }
    (whole / scale, part / scale)
}

fn quarter(v: f32) -> u32 {
    (v * 4.0).round() as i32 as u32
}

fn key_of(c: Color) -> u32 {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    byte(c.r) << 24 | byte(c.g) << 16 | byte(c.b) << 8 | byte(c.a)
}

/// One shade: the shape, how soft, and each colour with where it is moved.
struct Shade<'s> {
    w: f32,
    h: f32,
    radius: f32,
    sigma: f32,
    /// Drawn on whole pixels with the shape placed inside to a quarter
    /// pixel, for a crisp edge. A soft shadow needs none, and without it
    /// a key sliding or landing reuses one bitmap.
    snap: bool,
    /// Shade inside the shape, from everything outside it, rather than the
    /// shape's own shadow outside it.
    inside: bool,
    layers: &'s [((f32, f32), Color)],
}

impl Shade<'_> {
    fn moved(&self) -> f32 {
        self.layers
            .iter()
            .map(|((dx, dy), _)| dx.abs().max(dy.abs()))
            .fold(0.0, f32::max)
    }
}

impl Painter<'_> {
    /// `r`'s shadow on the plate, moved by `(dx, dy)`, `blur` soft, in `c`.
    pub(super) unsafe fn cast(
        &self,
        gpu: &Gpu,
        r: &Rect,
        radius: f32,
        (dx, dy): (f32, f32),
        blur: f32,
        c: Color,
    ) {
        // Baked opaque and drawn at the colour's alpha, so a key fading in
        // or lifting reuses one bitmap.
        let shade = Shade {
            w: r.w,
            h: r.h,
            radius,
            sigma: sigma(stepped(blur)),
            snap: false,
            inside: false,
            layers: &[((0.0, 0.0), c.with_alpha(1.0))],
        };
        self.shade(gpu, &shade, (r.x + dx, r.y + dy), c.a);
    }

    /// Light and shade inside `r`, along its edges: `near` on the top left
    /// rim, `far` on the bottom right, each `offset` deep and `blur` soft.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn inner(
        &self,
        gpu: &Gpu,
        r: &Rect,
        radius: f32,
        (offset, blur): (f32, f32),
        near: Color,
        far: Color,
        opacity: f32,
    ) {
        let shade = Shade {
            w: r.w,
            h: r.h,
            radius,
            sigma: sigma(blur),
            snap: true,
            inside: true,
            layers: &[((offset, offset), near), ((-offset, -offset), far)],
        };
        self.shade(gpu, &shade, (r.x, r.y), opacity);
    }

    /// Draws `s` with its shape's corner at `at`, baking it first if this
    /// target has not yet.
    unsafe fn shade(&self, gpu: &Gpu, s: &Shade, at: (f32, f32), opacity: f32) {
        if s.w <= 0.0 || s.h <= 0.0 || opacity <= 0.0 {
            return;
        }
        let pad = pad(s.sigma, s.moved());
        let (mut dpi_x, mut dpi_y) = (96.0, 96.0);
        self.rt.GetDpi(&mut dpi_x, &mut dpi_y);
        let scale = dpi_x / 96.0;
        let place = |at: f32| {
            if s.snap {
                snap(at - pad, scale)
            } else {
                (at - pad, 0.0)
            }
        };
        let (x, phase_x) = place(at.0);
        let (y, phase_y) = place(at.1);
        let mut key = vec![
            s.inside as u32,
            quarter(s.w),
            quarter(s.h),
            quarter(s.radius),
            quarter(s.sigma * 2.0),
            quarter(phase_x * scale),
            quarter(phase_y * scale),
            dpi_x.to_bits(),
        ];
        for &((dx, dy), c) in s.layers {
            key.extend([quarter(dx), quarter(dy), key_of(c)]);
        }
        let now = self.shades.clock.get() + 1;
        self.shades.clock.set(now);
        let mut kept = self.shades.kept.borrow_mut();
        let bitmap = match kept.get_mut(&key) {
            Some((b, at)) => {
                *at = now;
                b.clone()
            }
            None => {
                let Ok(b) = self.bake(gpu, s, pad, (phase_x, phase_y)) else {
                    return;
                };
                if kept.len() >= SHADES_KEPT {
                    if let Some(old) = oldest(&kept) {
                        kept.remove(&old);
                    }
                }
                kept.insert(key, (b.clone(), now));
                b
            }
        };
        let size = bitmap.GetSize();
        let to = D2D_RECT_F {
            left: x,
            top: y,
            right: x + size.width,
            bottom: y + size.height,
        };
        self.rt.DrawBitmap(
            &bitmap,
            Some(&to),
            opacity.min(1.0),
            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
            None,
        );
    }

    /// `s` into a bitmap of its own, its shape `pad` in from the corner
    /// and a further `phase` to land where it is meant to.
    unsafe fn bake(
        &self,
        gpu: &Gpu,
        s: &Shade,
        pad: f32,
        (phase_x, phase_y): (f32, f32),
    ) -> Result<ID2D1Bitmap> {
        // A pixel more than the shape and its pad, for the phase.
        let size = D2D_SIZE_F {
            width: s.w + 2.0 * pad + 1.0,
            height: s.h + 2.0 * pad + 1.0,
        };
        let scratch = || -> Result<ID2D1BitmapRenderTarget> {
            let format = D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            };
            let t = self.rt.CreateCompatibleRenderTarget(
                Some(&size),
                None,
                Some(&format),
                D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE,
            )?;
            t.BeginDraw();
            t.Clear(Some(&D2D1_COLOR_F::default()));
            Ok(t)
        };
        let shape = Rect::new(pad + phase_x, pad + phase_y, s.w, s.h);

        // The shape's alpha: the shape itself, or everything round it.
        let alpha = scratch()?;
        self.brush.SetColor(&color(Color::rgb(0xFFFFFF)));
        if s.inside {
            let reach = 2.0 * pad + 2.0;
            let ring = shape.inset(-reach / 2.0);
            alpha.DrawRoundedRectangle(
                &rounded(&ring, s.radius + reach / 2.0),
                self.brush,
                reach,
                None,
            );
        } else {
            alpha.FillRoundedRectangle(&rounded(&shape, s.radius), self.brush);
        }
        alpha.EndDraw(None, None)?;
        let source: ID2D1Image = alpha.GetBitmap()?.cast()?;

        let out = scratch()?;
        let dc = out.cast::<ID2D1DeviceContext>()?;
        let drawn = (|| -> Result<()> {
            // Everything that can fail comes before the clip is pushed, so
            // a failure never leaves it pushed.
            let mut shadows = Vec::new();
            for &((dx, dy), c) in s.layers {
                let shadow = dc.CreateEffect(&CLSID_D2D1Shadow)?;
                shadow.SetValue(
                    D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION.0 as u32,
                    D2D1_PROPERTY_TYPE_FLOAT,
                    &s.sigma.max(0.01).to_le_bytes(),
                )?;
                let rgba: Vec<u8> = [c.r, c.g, c.b, c.a]
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect();
                shadow.SetValue(
                    D2D1_SHADOW_PROP_COLOR.0 as u32,
                    D2D1_PROPERTY_TYPE_VECTOR4,
                    &rgba,
                )?;
                shadow.SetInput(0, &source, true);
                shadows.push((shadow.GetOutput()?, Vector2 { X: dx, Y: dy }));
            }
            let draw = || {
                for (image, at) in &shadows {
                    dc.DrawImage(
                        image,
                        Some(at),
                        None,
                        D2D1_INTERPOLATION_MODE_LINEAR,
                        D2D1_COMPOSITE_MODE_SOURCE_OVER,
                    );
                }
            };
            if !s.inside {
                draw();
                return Ok(());
            }
            let mask = gpu.d2d.CreateRoundedRectangleGeometry(&rounded(&shape, s.radius))?;
            let mask = mask.cast::<ID2D1Geometry>()?;
            let layer = out.CreateLayer(None)?;
            let params = D2D1_LAYER_PARAMETERS {
                contentBounds: super::rect(&shape),
                geometricMask: ManuallyDrop::new(Some(mask)),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: Matrix3x2::identity(),
                opacity: 1.0,
                opacityBrush: ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS_NONE,
            };
            out.PushLayer(&params, &layer);
            draw();
            out.PopLayer();
            drop(ManuallyDrop::into_inner(params.geometricMask));
            Ok(())
        })();
        out.EndDraw(None, None)?;
        drawn?;
        out.GetBitmap()
    }
}

/// Shadow and shade for a window that draws itself, not through a
/// [`super::Target`]: the stage's panes, the Folio. Kept with its target,
/// since what it bakes is the target's.
pub struct Shading {
    brush: ID2D1SolidColorBrush,
    gradients: super::Gradients,
    shades: Shades,
}

impl Shading {
    pub fn new(rt: &ID2D1RenderTarget) -> Result<Shading> {
        let brush = unsafe { rt.CreateSolidColorBrush(&color(Color::rgb(0)), None)? };
        Ok(Shading {
            brush,
            gradients: super::Gradients::default(),
            shades: Shades::default(),
        })
    }

    fn painter<'a>(&'a self, rt: &'a ID2D1RenderTarget) -> Painter<'a> {
        Painter {
            rt,
            brush: &self.brush,
            gradients: &self.gradients,
            shades: &self.shades,
            emit: None,
        }
    }

    /// `r`'s shadow on `rt`, moved by `by`, `blur` soft, in `c`. `rt` must
    /// be drawing.
    pub unsafe fn cast(
        &self,
        gpu: &Gpu,
        rt: &ID2D1RenderTarget,
        r: &Rect,
        radius: f32,
        by: (f32, f32),
        blur: f32,
        c: Color,
    ) {
        self.painter(rt).cast(gpu, r, radius, by, blur, c);
    }

    /// Light and shade inside `r` on `rt`, as [`Painter::inner`].
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn inner(
        &self,
        gpu: &Gpu,
        rt: &ID2D1RenderTarget,
        r: &Rect,
        radius: f32,
        depth: (f32, f32),
        near: Color,
        far: Color,
        opacity: f32,
    ) {
        self.painter(rt).inner(gpu, r, radius, depth, near, far, opacity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shade_lands_on_whole_pixels_within_an_eighth_of_one() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            for at in [0.0, 10.3, 10.5, 33.37, 99.99, -4.2] {
                let (corner, phase) = snap(at, scale);
                let px = corner * scale;
                assert!((px - px.round()).abs() < 1e-3, "{at} at {scale}");
                assert!(phase >= 0.0 && phase * scale < 1.0);
                assert!(((corner + phase) - at).abs() * scale <= 0.125 + 1e-4);
            }
        }
    }

    #[test]
    fn a_blur_softens_as_the_shapes_it_replaces_did() {
        assert_eq!(sigma(0.0), 0.0);
        assert_eq!(sigma(-3.0), 0.0);
        // A box 12 wide has a standard deviation of 12 / sqrt(12).
        assert!((sigma(12.0) - 12f32.sqrt()).abs() < 1e-5);
        // The pad holds the blur's reach and the move.
        assert!(pad(2.0, 1.5) >= 3.0 * 2.0 + 1.5);
    }

    #[test]
    fn a_key_lifting_bakes_its_shadow_a_few_times_not_every_frame() {
        // Depth runs continuously from rest to lifted; the blur 3 + 6 d.
        let blurs: std::collections::BTreeSet<u32> = (0..=100)
            .map(|i| stepped(3.0 + 6.0 * 0.3 * i as f32 / 100.0).to_bits())
            .collect();
        assert!(blurs.len() <= 3, "{}", blurs.len());
        assert_eq!(stepped(-1.0), 0.0);
        assert!((stepped(4.6) - 4.6).abs() <= BLUR_STEP / 2.0);
    }

    #[test]
    fn the_shade_drawn_longest_ago_goes_first() {
        let mut kept = HashMap::new();
        kept.insert("a", ((), 5u64));
        kept.insert("b", ((), 2u64));
        kept.insert("c", ((), 9u64));
        assert_eq!(oldest(&kept), Some("b"));
        assert_eq!(oldest(&HashMap::<&str, ((), u64)>::new()), None);
    }
}
