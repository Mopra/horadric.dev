//! Light that blooms. What shines is drawn sharp into a layer of its own,
//! then laid over the window three times, added on, never painted over:
//! as it is, blurred a little, and blurred wide. The light spreads and
//! saturates the way it does in a game, where stacked rings only ever
//! read as stickers. The GPU does the blur.

use std::cell::RefCell;

use windows::core::{Interface, Result};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_COMPOSITE_MODE_PLUS, D2D1_PIXEL_FORMAT,
    D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1GaussianBlur, ID2D1BitmapRenderTarget, ID2D1DeviceContext, ID2D1Effect, ID2D1Image,
    D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE, D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION,
    D2D1_INTERPOLATION_MODE_LINEAR, D2D1_PROPERTY_TYPE_FLOAT,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

use super::{Gpu, Painter, Scene, Target};
use crate::layout::Rect;
use crate::motion::{self, ORBIT};
use crate::theme::{self, Color};
use horadric_core::session::Phase;

/// How far the near glow and the far glow spread, in DIPs. Two, because
/// one blur is either a smudge or a halo; real light is both.
const NEAR: f32 = 1.8;
const FAR: f32 = 7.0;
/// How many times the far glow is laid on: it spreads its light thin.
const FAR_PASSES: usize = 2;

/// The layer the light is drawn into and the blurs that spread it, kept
/// between frames.
#[derive(Default)]
pub(super) struct Bloom {
    layer: RefCell<Option<(ID2D1BitmapRenderTarget, D2D_SIZE_U)>>,
    blurs: RefCell<Option<[ID2D1Effect; 2]>>,
}

impl Target {
    /// Lays the frame's light over what is drawn. Nothing when nothing
    /// shines, so a quiet window costs no blur.
    pub(super) unsafe fn bloom(&self, gpu: &Gpu, scene: &Scene) -> Result<()> {
        if !scene.ambient || !shines(scene) {
            return Ok(());
        }
        let Ok(dc) = self.rt.cast::<ID2D1DeviceContext>() else {
            return Ok(());
        };
        let size = self.rt.GetPixelSize();
        let mut layer = self.bloom.layer.borrow_mut();
        if layer.as_ref().is_none_or(|(_, s)| *s != size) {
            let format = D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            };
            let target = self.rt.CreateCompatibleRenderTarget(
                None,
                None,
                Some(&format),
                D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE,
            )?;
            *layer = Some((target, size));
        }
        let Some((light, _)) = layer.as_ref() else {
            return Ok(());
        };
        light.BeginDraw();
        light.Clear(Some(&D2D1_COLOR_F::default()));
        self.painter(light).shine(gpu, scene);
        light.EndDraw(None, None)?;

        let image: ID2D1Image = light.GetBitmap()?.cast()?;
        let mut blurs = self.bloom.blurs.borrow_mut();
        if blurs.is_none() {
            let blur = |sigma: f32| -> Result<ID2D1Effect> {
                let e = dc.CreateEffect(&CLSID_D2D1GaussianBlur)?;
                let index = D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32;
                e.SetValue(index, D2D1_PROPERTY_TYPE_FLOAT, &sigma.to_le_bytes())?;
                Ok(e)
            };
            *blurs = Some([blur(NEAR)?, blur(FAR)?]);
        }
        let Some([near, far]) = blurs.as_ref() else {
            return Ok(());
        };
        let add = |image: &ID2D1Image| {
            dc.DrawImage(
                image,
                None,
                None,
                D2D1_INTERPOLATION_MODE_LINEAR,
                D2D1_COMPOSITE_MODE_PLUS,
            )
        };
        add(&image);
        near.SetInput(0, &image, true);
        add(&near.GetOutput()?);
        far.SetInput(0, &image, true);
        let wide = far.GetOutput()?;
        for _ in 0..FAR_PASSES {
            add(&wide);
        }
        Ok(())
    }
}

/// Whether anything in the frame gives off light.
fn shines(scene: &Scene) -> bool {
    super::tiles(scene)
        .enumerate()
        .any(|(i, (_, s, _))| s.phase == Phase::Working && !scene.beckons(i, s))
}

impl Painter<'_> {
    /// What gives off light this frame, sharp, on black.
    unsafe fn shine(&self, gpu: &Gpu, scene: &Scene) {
        for (i, (r, s, look)) in super::tiles(scene).enumerate() {
            if s.phase != Phase::Working || scene.beckons(i, s) {
                continue;
            }
            let t = motion::cycle(look.phase_age, ORBIT);
            self.surge(gpu, &super::lamp_rect(&r), theme::phase_color(&s.phase), t, look.enter);
        }
    }

    /// Light running up a working session's lamp, `t` of the way through
    /// one climb: two pulses, each a white hot head with a tail of the
    /// phase's colour behind it, kept inside the glass. The bloom does the
    /// rest, so the glow stays round the lamp where the light is.
    unsafe fn surge(&self, gpu: &Gpu, r: &Rect, c: Color, t: f32, strength: f32) {
        let strength = strength.clamp(0.0, 1.0);
        if strength <= 0.0 {
            return;
        }
        let white = Color::rgb(0xFFFFFF);
        let hot = c.mix(white, 0.55);
        let tail = r.h * 0.55;
        let travel = r.h + tail;
        self.masked(gpu, r, r.w / 2.0, || {
            for k in [0.0, 0.5] {
                let p = (t + k).rem_euclid(1.0);
                // Fast through the middle, easing at the ends, so it
                // reads as a surge and not as a conveyor.
                let p = motion::ease_in_out(p);
                let head = r.bottom() - travel * p;
                let streak = Rect::new(r.x, head, r.w, tail);
                let stops = [
                    (0.0, hot),
                    (0.12, c),
                    (1.0, c.with_alpha(0.0)),
                ];
                self.fill_rounded_gradient(&streak, r.w / 2.0, &stops, strength);
            }
        });
    }
}
