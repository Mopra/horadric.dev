//! Light that blooms, for every window. What shines is drawn sharp into a
//! layer of its own, then added over the window three times, never painted
//! over: as it is, blurred a little, and blurred wide. Light spreads and
//! saturates the way it does in a game, where stacked rings only ever read
//! as stickers. The GPU does the blur.
//!
//! A window keeps one [`Light`]. Each frame with something lit it asks
//! [`Light::begin`] for the layer, draws its emitters into it with its own
//! painter, reports their bounds with [`Light::lit`], and ends with
//! [`Light::present`]. A frame with nothing lit never calls it, so a quiet
//! window costs no blur.

use std::cell::{Cell, RefCell};

use windows::core::{Interface, Result};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLORMATRIX_ALPHA_MODE_STRAIGHT, D2D1_ALPHA_MODE_UNKNOWN, D2D1_COLOR_F, D2D1_COMPOSITE_MODE_PLUS,
    D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1ColorMatrix, CLSID_D2D1GaussianBlur, ID2D1BitmapRenderTarget, ID2D1DeviceContext,
    ID2D1Effect, ID2D1Image, ID2D1RenderTarget, D2D1_BUFFER_PRECISION_16BPC_FLOAT,
    D2D1_COLORMATRIX_PROP_ALPHA_MODE, D2D1_COLORMATRIX_PROP_COLOR_MATRIX, D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION,
    D2D1_INTERPOLATION_MODE_LINEAR, D2D1_PRIMITIVE_BLEND_ADD, D2D1_PROPERTY_PRECISION,
    D2D1_PROPERTY_TYPE_ENUM, D2D1_PROPERTY_TYPE_FLOAT, D2D1_PROPERTY_TYPE_MATRIX_5X4, D2D1_PROPERTY_TYPE_UNKNOWN,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_HARDWARE, D2D1_RENDER_TARGET_USAGE_NONE,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_UNKNOWN,
};
use windows_numerics::Vector2;

use crate::layout::Rect;
use crate::theme::{self, Theme};

/// How a window's light spreads. Two blurs, because one is either a smudge
/// or a halo; real light is both.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bloom {
    /// The tight glow round an emitter, a blur's sigma in DIPs.
    pub near: f32,
    /// The wide glow, in DIPs.
    pub far: f32,
    /// How many times the wide glow is laid on: it spreads its light thin.
    pub far_passes: usize,
    /// What the light is scaled by before it spreads. Past one, a core
    /// saturates sooner and its bloom carries the extra out.
    pub gain: f32,
}

impl Bloom {
    /// A cluster's tiles, per theme. Skeuomorph is the surge as the human
    /// approved it; Matrix glows like phosphor; Winamp stays a hard LCD.
    pub fn tile() -> Bloom {
        match theme::current() {
            Theme::Skeuomorph => Bloom {
                near: 1.8,
                far: 7.0,
                far_passes: 2,
                gain: 1.0,
            },
            Theme::Matrix => Bloom {
                near: 2.2,
                far: 9.0,
                far_passes: 3,
                gain: 1.15,
            },
            Theme::Winamp => Bloom {
                near: 1.2,
                far: 4.0,
                far_passes: 1,
                gain: 1.0,
            },
        }
    }

    /// The stage's panes: softer than a tile, since it sits beside text
    /// being read.
    pub fn pane() -> Bloom {
        let tile = Bloom::tile();
        Bloom {
            near: tile.near * 0.8,
            far: tile.far * 0.7,
            far_passes: 1,
            gain: tile.gain * 0.8,
        }
    }

    /// How far past its emitter a light reaches: a Gaussian is all but
    /// gone at three sigmas.
    fn reach(&self) -> f32 {
        self.far.max(self.near) * 3.0
    }
}

/// A window's light layer and the effects that spread it, kept between
/// frames.
#[derive(Default)]
pub struct Light {
    layer: RefCell<Option<Layer>>,
    effects: RefCell<Option<Effects>>,
    /// Whether the device is a GPU, asked once. On WARP a blur is CPU work.
    hardware: Cell<Option<bool>>,
    /// The union of this frame's emitters.
    lit: Cell<Option<Rect>>,
}

struct Layer {
    target: ID2D1BitmapRenderTarget,
    size: D2D_SIZE_U,
    /// Half floats, so lights sum past one and the bloom carries the
    /// extra, where eight bits clip and a clipped core blooms flat.
    float: bool,
}

struct Effects {
    gain: ID2D1Effect,
    near: ID2D1Effect,
    far: ID2D1Effect,
    /// The wide glow scaled by its passes, so it is blurred and added
    /// once, not once a pass.
    far_passes: ID2D1Effect,
    look: Bloom,
}

impl Light {
    /// Whether this light blooms on `rt`: no on a software device, where
    /// emitters are added sharp and still read.
    pub unsafe fn blooms(&self, rt: &ID2D1RenderTarget) -> bool {
        if let Some(h) = self.hardware.get() {
            return h;
        }
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_HARDWARE,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_UNKNOWN,
                alphaMode: D2D1_ALPHA_MODE_UNKNOWN,
            },
            dpiX: 0.0,
            dpiY: 0.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let h = rt.IsSupported(&props).as_bool();
        self.hardware.set(Some(h));
        h
    }

    /// The layer to draw this frame's emitters into, sharp, on black, begun
    /// and cleared. Made once per size of `rt`. Light drawn into it adds.
    pub unsafe fn begin(&self, rt: &ID2D1RenderTarget) -> Result<ID2D1BitmapRenderTarget> {
        let size = rt.GetPixelSize();
        let mut layer = self.layer.borrow_mut();
        if layer.as_ref().is_none_or(|l| l.size != size) {
            *layer = Some(make_layer(rt, size)?);
            // Effects keep the old layer's precision; make them again.
            *self.effects.borrow_mut() = None;
        }
        let Some(l) = layer.as_ref() else {
            unreachable!("the layer was just made")
        };
        l.target.BeginDraw();
        l.target.Clear(Some(&D2D1_COLOR_F::default()));
        self.lit.set(None);
        Ok(l.target.clone())
    }

    /// An emitter's bounds this frame. Only what they cover, grown by the
    /// bloom's reach, is blurred and added.
    pub fn lit(&self, r: Rect) {
        let joined = match self.lit.get() {
            None => r,
            Some(a) => {
                let x = a.x.min(r.x);
                let y = a.y.min(r.y);
                Rect::new(
                    x,
                    y,
                    a.right().max(r.right()) - x,
                    a.bottom().max(r.bottom()) - y,
                )
            }
        };
        self.lit.set(Some(joined));
    }

    /// Ends the layer and adds its light over `rt`, as it is and through
    /// both blurs. `rt` must be drawing.
    pub unsafe fn present(&self, rt: &ID2D1RenderTarget, look: &Bloom) -> Result<()> {
        let layer = self.layer.borrow();
        let Some(l) = layer.as_ref() else {
            return Ok(());
        };
        l.target.EndDraw(None, None)?;
        let Some(lit) = self.lit.take() else {
            return Ok(());
        };
        let dc = rt.cast::<ID2D1DeviceContext>()?;
        let blooms = self.blooms(rt);
        let size = l.target.GetSize();
        let reach = if blooms { look.reach() } else { 0.0 } + 1.0;
        let area = D2D_RECT_F {
            left: (lit.x - reach).max(0.0),
            top: (lit.y - reach).max(0.0),
            right: (lit.right() + reach).min(size.width),
            bottom: (lit.bottom() + reach).min(size.height),
        };
        if area.right <= area.left || area.bottom <= area.top {
            return Ok(());
        }
        let at = Vector2 {
            X: area.left,
            Y: area.top,
        };
        let add = |image: &ID2D1Image| {
            dc.DrawImage(
                image,
                Some(&at),
                Some(&area),
                D2D1_INTERPOLATION_MODE_LINEAR,
                D2D1_COMPOSITE_MODE_PLUS,
            )
        };

        let mut effects = self.effects.borrow_mut();
        if effects.as_ref().is_none_or(|e| e.look != *look) {
            *effects = Some(make_effects(&dc, look, l.float)?);
        }
        let Some(e) = effects.as_ref() else {
            unreachable!("the effects were just made")
        };
        let mut source: ID2D1Image = l.target.GetBitmap()?.cast()?;
        if look.gain != 1.0 {
            e.gain.SetInput(0, &source, true);
            source = e.gain.GetOutput()?;
        }
        add(&source);
        if !blooms {
            return Ok(());
        }
        e.near.SetInput(0, &source, true);
        add(&e.near.GetOutput()?);
        if look.far_passes == 0 {
            return Ok(());
        }
        e.far.SetInput(0, &source, true);
        let mut wide = e.far.GetOutput()?;
        if look.far_passes > 1 {
            e.far_passes.SetInput(0, &wide, true);
            wide = e.far_passes.GetOutput()?;
        }
        add(&wide);
        Ok(())
    }
}

/// A light layer the size of `rt`, in half floats where the device takes
/// them, else eight bits.
unsafe fn make_layer(rt: &ID2D1RenderTarget, size: D2D_SIZE_U) -> Result<Layer> {
    let floats = rt.cast::<ID2D1DeviceContext>().is_ok_and(|dc| {
        dc.IsBufferPrecisionSupported(D2D1_BUFFER_PRECISION_16BPC_FLOAT)
            .as_bool()
    });
    let make = |format| {
        let format = D2D1_PIXEL_FORMAT {
            format,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        };
        rt.CreateCompatibleRenderTarget(
            None,
            None,
            Some(&format),
            D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE,
        )
    };
    let (target, float) = match floats.then(|| make(DXGI_FORMAT_R16G16B16A16_FLOAT)) {
        Some(Ok(t)) => (t, true),
        _ => (make(DXGI_FORMAT_B8G8R8A8_UNORM)?, false),
    };
    // Two lights meeting are brighter than either: emitters add, so a hot
    // core sums toward white instead of the top one hiding the other.
    if let Ok(dc) = target.cast::<ID2D1DeviceContext>() {
        dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_ADD);
    }
    Ok(Layer {
        target,
        size,
        float,
    })
}

unsafe fn make_effects(dc: &ID2D1DeviceContext, look: &Bloom, float: bool) -> Result<Effects> {
    let make = |clsid| -> Result<ID2D1Effect> {
        let e = dc.CreateEffect(clsid)?;
        // Effects work in the context's eight bits unless told, which
        // would clip the float layer's extra before it spreads.
        if float {
            let precision = D2D1_BUFFER_PRECISION_16BPC_FLOAT.0 as u32;
            e.SetValue(
                D2D1_PROPERTY_PRECISION.0 as u32,
                D2D1_PROPERTY_TYPE_UNKNOWN,
                &precision.to_le_bytes(),
            )?;
        }
        Ok(e)
    };
    let blur = |sigma: f32| -> Result<ID2D1Effect> {
        let e = make(&CLSID_D2D1GaussianBlur)?;
        let index = D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32;
        e.SetValue(index, D2D1_PROPERTY_TYPE_FLOAT, &sigma.to_le_bytes())?;
        Ok(e)
    };
    // `straight` scales the premultiplied channels as they are, so light
    // is `g` times as bright. Without it the effect unpremultiplies first
    // and scales the alpha too, which comes out `g` squared: the gain was
    // tuned that way and is kept so.
    let scale = |g: f32, straight: bool| -> Result<ID2D1Effect> {
        let e = make(&CLSID_D2D1ColorMatrix)?;
        #[rustfmt::skip]
        let matrix: [f32; 20] = [
            g, 0.0, 0.0, 0.0,
            0.0, g, 0.0, 0.0,
            0.0, 0.0, g, 0.0,
            0.0, 0.0, 0.0, g,
            0.0, 0.0, 0.0, 0.0,
        ];
        let bytes: Vec<u8> = matrix.iter().flat_map(|v| v.to_le_bytes()).collect();
        e.SetValue(
            D2D1_COLORMATRIX_PROP_COLOR_MATRIX.0 as u32,
            D2D1_PROPERTY_TYPE_MATRIX_5X4,
            &bytes,
        )?;
        if straight {
            let mode = D2D1_COLORMATRIX_ALPHA_MODE_STRAIGHT.0 as u32;
            e.SetValue(
                D2D1_COLORMATRIX_PROP_ALPHA_MODE.0 as u32,
                D2D1_PROPERTY_TYPE_ENUM,
                &mode.to_le_bytes(),
            )?;
        }
        Ok(e)
    };
    // The tone curve is the window's own: its eight bit target clips what
    // the float layer carries past one, after the blurs have spread it.
    // The gain only sets how soon a core gets there. Adding the wide glow
    // n times and adding it once n times as bright clip alike, since light
    // only adds; once is one blur instead of n.
    Ok(Effects {
        gain: scale(look.gain, false)?,
        near: blur(look.near)?,
        far: blur(look.far)?,
        far_passes: scale(look.far_passes as f32, true)?,
        look: *look,
    })
}
