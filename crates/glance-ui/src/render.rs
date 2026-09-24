//! Direct2D and DirectWrite drawing for a cluster window and the usage
//! window.
//!
//! [`Gpu`] holds the process wide factories and text formats. [`Target`] is
//! one window's render target and brushes. Drawing happens in DIPs; Direct2D
//! applies the DPI.
//!
//! A cluster is glass: acrylic from DWM behind, a dark tint over it, and
//! tiles as faint panes of light laid on top. A session's phase is light
//! too, in its tile's edge and icon: a working tile has a light going round
//! it, a waiting one breathes, a finished one flashes once and settles.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use glance_core::usage::format_until;
use glance_core::{format_age, Limit, Phase, Session, Usage};
use windows::core::{w, Interface, Result, BOOL, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP,
    D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN, D2D_SIZE_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1BitmapRenderTarget, ID2D1Factory, ID2D1GradientStopCollection,
    ID2D1HwndRenderTarget, ID2D1LinearGradientBrush, ID2D1RadialGradientBrush, ID2D1RenderTarget,
    ID2D1SolidColorBrush, ID2D1StrokeStyle, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
    D2D1_CAP_STYLE_FLAT, D2D1_CAP_STYLE_ROUND, D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE,
    D2D1_DASH_STYLE_CUSTOM, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_GAMMA_2_2,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    D2D1_LINE_JOIN_ROUND, D2D1_PRESENT_OPTIONS_NONE, D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_DASH_STYLE_SOLID,
    D2D1_SWEEP_DIRECTION_CLOCKWISE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, IDWriteRenderingParams,
    IDWriteTextFormat, IDWriteTextLayout, IDWriteTextLayout1, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_RANGE,
    DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows_numerics::Vector2;

use crate::anim::Look;
use crate::files::{Row, Tree};
use crate::layout::{
    self, Button, ClusterLayout, FilesLayout, Hit, Metrics, Rect, UsageHit, UsageLayout,
};
use crate::motion::{self, BREATH, ORBIT};
use crate::theme::{self, Color};

const FONT: PCWSTR = w!("Segoe UI Variable Text");
/// For the project's name: the optical size cut for larger text.
const FONT_DISPLAY: PCWSTR = w!("Segoe UI Variable Display");
/// Windows 11's icon font, and the one Windows 10 has in its place.
const ICON_FONTS: [PCWSTR; 2] = [w!("Segoe Fluent Icons"), w!("Segoe MDL2 Assets")];
/// DirectWrite's enhanced contrast. The usual system value is 0.5 to 1.
const TEXT_CONTRAST: f32 = 2.0;

/// Where the name and the lines under it start in a tile, after the icon.
const TILE_TEXT_X: f32 = 46.0;
/// The activity trace at the bottom right of a tile.
const TRACE_BARS: usize = 20;
const TRACE_BAR_W: f32 = 1.6;
const TRACE_GAP: f32 = 0.8;
const TRACE_H: f32 = 11.0;

/// Process wide Direct2D and DirectWrite objects.
pub struct Gpu {
    pub d2d: ID2D1Factory,
    pub dw: IDWriteFactory,
    pub title: IDWriteTextFormat,
    pub body: IDWriteTextFormat,
    pub small: IDWriteTextFormat,
    pub small_right: IDWriteTextFormat,
    /// The project's name at the top of its cluster.
    pub display: IDWriteTextFormat,
    /// A session's name on its tile.
    pub name: IDWriteTextFormat,
    /// The counts in a cluster's header.
    pub chip: IDWriteTextFormat,
    /// Icons, centred in the rect they are drawn in.
    pub icon: IDWriteTextFormat,
    pub icon_small: IDWriteTextFormat,
    pub text_params: IDWriteRenderingParams,
}

impl Gpu {
    pub fn new() -> Result<Self> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;

            let normal = DWRITE_FONT_WEIGHT_NORMAL;
            let semi = DWRITE_FONT_WEIGHT_SEMI_BOLD;
            let title = format(&dw, FONT, 14.0, semi, false)?;
            let body = format(&dw, FONT, 14.0, normal, false)?;
            let small = format(&dw, FONT, 12.5, normal, false)?;
            let small_right = format(&dw, FONT, 12.5, normal, true)?;
            let display = format(&dw, FONT_DISPLAY, 15.5, semi, false)?;
            let name = format(&dw, FONT, 13.5, semi, false)?;
            let chip = format(&dw, FONT, 11.5, semi, false)?;
            let icons = icon_family(&dw)?;
            let icon = format(&dw, icons, 14.0, normal, false)?;
            icon.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            let icon_small = format(&dw, icons, 11.0, normal, false)?;
            icon_small.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            // The system's text contrast is tuned for dark text on white.
            // Light strokes on a near black background come out thin with
            // it, so raise it and keep the rest of the user's tuning.
            let system = dw.CreateRenderingParams()?;
            let text_params = dw.CreateCustomRenderingParams(
                system.GetGamma(),
                system.GetEnhancedContrast().max(TEXT_CONTRAST),
                system.GetClearTypeLevel(),
                system.GetPixelGeometry(),
                system.GetRenderingMode(),
            )?;
            Ok(Gpu {
                text_params,
                d2d,
                dw,
                title,
                body,
                small,
                small_right,
                display,
                name,
                chip,
                icon,
                icon_small,
            })
        }
    }
}

/// The first icon font this Windows has.
unsafe fn icon_family(dw: &IDWriteFactory) -> Result<PCWSTR> {
    let mut collection: Option<IDWriteFontCollection> = None;
    dw.GetSystemFontCollection(&mut collection, false)?;
    let Some(collection) = collection else {
        return Ok(ICON_FONTS[0]);
    };
    for name in ICON_FONTS {
        let (mut index, mut exists) = (0u32, BOOL(0));
        collection.FindFamilyName(name, &mut index, &mut exists)?;
        if exists.as_bool() {
            return Ok(name);
        }
    }
    Ok(ICON_FONTS[0])
}

/// One line, vertically centred, trimmed with an ellipsis, no wrapping.
unsafe fn format(
    dw: &IDWriteFactory,
    family: PCWSTR,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
    right: bool,
) -> Result<IDWriteTextFormat> {
    let f = dw.CreateTextFormat(
        family,
        None,
        weight,
        DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_STRETCH_NORMAL,
        size,
        w!("en-us"),
    )?;
    f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
    f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
    if right {
        f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
    }
    let trimming = DWRITE_TRIMMING {
        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
        delimiter: 0,
        delimiterCount: 0,
    };
    let sign = dw.CreateEllipsisTrimmingSign(&f)?;
    f.SetTrimming(&trimming, &sign)?;
    Ok(f)
}

/// Everything one frame of a cluster needs.
pub struct Scene<'a> {
    pub layout: &'a ClusterLayout,
    pub name: &'a str,
    pub collapsed: bool,
    pub sessions: &'a [&'a Session],
    /// How each tile draws this frame, in the same order.
    pub looks: &'a [Look],
    /// This project is the one the stage shows.
    pub on_stage: bool,
    /// The project's colour.
    pub accent: Color,
    pub now: SystemTime,
    pub files: Option<FilesScene<'a>>,
    /// What the cursor is over and what the left button is held on, for
    /// the buttons to light up.
    pub hot: Hit,
    pub pressed: Option<Hit>,
    /// Windows' animation setting. Off, the light holds still.
    pub ambient: bool,
    /// Something other than the light changed since the last frame, so the
    /// kept layer has to be drawn again.
    pub rebuild: bool,
}

impl Scene<'_> {
    fn button(&self, which: Hit) -> Button {
        layout::button(which, self.hot, self.pressed)
    }
}

/// What the files tile shows.
pub struct FilesScene<'a> {
    pub tree: &'a Tree,
    /// Every row, open folders expanded. `scroll` of them are above the top.
    pub rows: &'a [Row],
    pub scroll: usize,
    pub collapsed: bool,
}

/// Everything one frame of the usage window needs.
pub struct UsageScene<'a> {
    pub layout: &'a UsageLayout,
    pub collapsed: bool,
    pub usage: Option<&'a Usage>,
    /// Unix seconds.
    pub now: u64,
    /// Each setting's name and what it is set to.
    pub settings: Vec<(&'static str, &'static str)>,
    pub hot: UsageHit,
    pub pressed: Option<UsageHit>,
}

impl UsageScene<'_> {
    fn button(&self, which: UsageHit) -> Button {
        layout::button(which, self.hot, self.pressed)
    }
}

/// A window's render target. Recreated when Direct2D asks for it.
pub struct Target {
    rt: ID2D1HwndRenderTarget,
    brush: ID2D1SolidColorBrush,
    /// DWM draws a material behind, so what is left clear shows it.
    glass: bool,
    /// What holds still, kept between frames, and the size it was made at.
    layer: RefCell<Option<(ID2D1BitmapRenderTarget, D2D_SIZE_U)>>,
    gradients: Gradients,
}

/// Gradient brushes by their stops. A gradient is a texture on the GPU, and
/// making dozens a frame cost more than all the rest of the drawing. The
/// stops of each are fixed; what moves is where the brush sits and how
/// opaque it is, which a kept brush can be told.
#[derive(Default)]
struct Gradients {
    linear: RefCell<HashMap<Vec<u32>, ID2D1LinearGradientBrush>>,
    radial: RefCell<HashMap<Vec<u32>, ID2D1RadialGradientBrush>>,
}

/// Enough for every colour a cluster uses, with room for the passing ones
/// a tile sliding in makes. Past it the cache starts over.
const GRADIENTS_KEPT: usize = 96;

fn stops_key(stops: &[(f32, Color)]) -> Vec<u32> {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    stops
        .iter()
        .flat_map(|&(p, c)| {
            [
                (p * 1000.0) as u32,
                byte(c.r) << 24 | byte(c.g) << 16 | byte(c.b) << 8 | byte(c.a),
            ]
        })
        .collect()
}

/// A render target for a window, at its DPI, drawing in DIPs.
pub fn hwnd_target(
    gpu: &Gpu,
    hwnd: HWND,
    width_px: u32,
    height_px: u32,
    dpi: u32,
) -> Result<ID2D1HwndRenderTarget> {
    target_with_alpha(gpu, hwnd, width_px, height_px, dpi, false)
}

/// With `alpha`, what is drawn keeps its transparency, for a window with a
/// material behind it. Text is then greyscale: ClearType needs to know the
/// colour under it, and only DWM does.
fn target_with_alpha(
    gpu: &Gpu,
    hwnd: HWND,
    width_px: u32,
    height_px: u32,
    dpi: u32,
    alpha: bool,
) -> Result<ID2D1HwndRenderTarget> {
    unsafe {
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: if alpha {
                    D2D1_ALPHA_MODE_PREMULTIPLIED
                } else {
                    D2D1_ALPHA_MODE_IGNORE
                },
            },
            dpiX: 0.0,
            dpiY: 0.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd,
            pixelSize: D2D_SIZE_U {
                width: width_px.max(1),
                height: height_px.max(1),
            },
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };
        let rt = gpu.d2d.CreateHwndRenderTarget(&props, &hwnd_props)?;
        rt.SetDpi(dpi as f32, dpi as f32);
        rt.SetTextRenderingParams(&gpu.text_params);
        if alpha {
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        }
        Ok(rt)
    }
}

pub fn resize_target(rt: &ID2D1HwndRenderTarget, width_px: u32, height_px: u32) -> Result<()> {
    unsafe {
        rt.Resize(&D2D_SIZE_U {
            width: width_px.max(1),
            height: height_px.max(1),
        })
    }
}

impl Target {
    /// An opaque target.
    pub fn new(gpu: &Gpu, hwnd: HWND, width_px: u32, height_px: u32, dpi: u32) -> Result<Self> {
        Self::with_glass(gpu, hwnd, width_px, height_px, dpi, false)
    }

    /// With `glass`, for a window with a material behind it: what is left
    /// clear shows it.
    pub fn with_glass(
        gpu: &Gpu,
        hwnd: HWND,
        width_px: u32,
        height_px: u32,
        dpi: u32,
        glass: bool,
    ) -> Result<Self> {
        let rt = target_with_alpha(gpu, hwnd, width_px, height_px, dpi, glass)?;
        let brush = unsafe { rt.CreateSolidColorBrush(&color(theme::TEXT), None)? };
        Ok(Target {
            rt,
            brush,
            glass,
            layer: RefCell::new(None),
            gradients: Gradients::default(),
        })
    }

    pub fn resize(&self, width_px: u32, height_px: u32) -> Result<()> {
        resize_target(&self.rt, width_px, height_px)
    }

    pub fn set_dpi(&self, dpi: u32) {
        unsafe { self.rt.SetDpi(dpi as f32, dpi as f32) }
    }

    /// Draws a whole cluster. `Err` means the target must be recreated.
    ///
    /// Everything that holds still is drawn once into a layer kept between
    /// frames. A frame that only moves the light (a working tile's orbit, a
    /// waiting tile's breath) copies the layer and draws the light over it,
    /// which is most frames and costs a fraction of drawing it all.
    pub fn draw(&self, gpu: &Gpu, m: &Metrics, scene: &Scene) -> Result<()> {
        unsafe {
            let size = self.rt.GetPixelSize();
            let mut layer = self.layer.borrow_mut();
            let stale = scene.rebuild || layer.as_ref().is_none_or(|(_, s)| *s != size);
            if stale {
                let bitmap = match layer.take() {
                    Some((b, s)) if s == size => b,
                    _ => self.rt.CreateCompatibleRenderTarget(
                        None,
                        None,
                        None,
                        D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE,
                    )?,
                };
                bitmap.SetTextRenderingParams(&gpu.text_params);
                if self.glass {
                    bitmap.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                }
                bitmap.BeginDraw();
                self.painter(&bitmap).still(gpu, m, scene);
                bitmap.EndDraw(None, None)?;
                *layer = Some((bitmap, size));
            }

            self.rt.BeginDraw();
            self.rt.Clear(Some(&color(Color::rgb(0).with_alpha(0.0))));
            if let Some((bitmap, _)) = layer.as_ref() {
                let still = bitmap.GetBitmap()?;
                self.rt.DrawBitmap(
                    &still,
                    None,
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                    None,
                );
            }
            if scene.ambient {
                self.painter(&self.rt).light(gpu, m, scene);
            }
            self.rt.EndDraw(None, None)
        }
    }

    /// Draws the usage window. `Err` means the target must be recreated.
    pub fn draw_usage(&self, gpu: &Gpu, m: &Metrics, scene: &UsageScene) -> Result<()> {
        unsafe {
            self.rt.BeginDraw();
            self.painter(&self.rt).usage(gpu, m, scene);
            self.rt.EndDraw(None, None)
        }
    }

    fn painter<'a>(&'a self, rt: &'a ID2D1RenderTarget) -> Painter<'a> {
        Painter {
            rt,
            brush: &self.brush,
            glass: self.glass,
            gradients: &self.gradients,
        }
    }
}

/// Draws on the window or on its kept layer, which share their brushes.
struct Painter<'a> {
    rt: &'a ID2D1RenderTarget,
    brush: &'a ID2D1SolidColorBrush,
    glass: bool,
    gradients: &'a Gradients,
}

impl Painter<'_> {
    /// Everything that holds still between frames.
    unsafe fn still(&self, gpu: &Gpu, m: &Metrics, scene: &Scene) {
        if self.glass {
            self.rt.Clear(Some(&color(theme::GLASS_TINT)));
        } else {
            self.rt.Clear(Some(&color(theme::WINDOW_BG)));
        }
        self.wash(scene);
        if scene.on_stage {
            self.frame(m, scene);
        } else if self.glass {
            // Glass needs an edge to end at, as Windows' own flyouts have.
            let (w, h) = scene.layout.size;
            let r = Rect::new(0.5, 0.5, w - 1.0, h - 1.0);
            self.stroke_rounded(&r, m.window_radius - 0.5, theme::GLASS_EDGE.fade(0.8), 1.0);
        }

        self.header(gpu, scene);
        for (i, (r, s, look)) in tiles(scene).enumerate() {
            self.tile(gpu, m, scene, i, &r, s, &look);
        }
        if let Some(add) = &scene.layout.add {
            self.add(gpu, m, add, scene.button(Hit::Add));
        }
        if let (Some(l), Some(f)) = (&scene.layout.files, &scene.files) {
            self.files(gpu, m, l, f);
        }
    }

    unsafe fn usage(&self, gpu: &Gpu, m: &Metrics, scene: &UsageScene) {
        self.rt.Clear(Some(&color(theme::WINDOW_BG)));
        let l = scene.layout;
        let h = l.header;
        if let (Some(fill), _) = theme::button_look(scene.button(UsageHit::Header)) {
            let x = h.x - 4.0;
            let r = Rect::new(x, h.y + 3.0, h.right() + 4.0 - x, h.h - 6.0);
            self.fill_rounded(&r, 6.0, fill);
        }
        let chevron = if scene.collapsed {
            "\u{25B8}"
        } else {
            "\u{25BE}"
        };
        self.text(
            &gpu.title,
            theme::TEXT_DIM,
            chevron,
            Rect::new(h.x, h.y, 14.0, h.h),
        );
        self.text(
            &gpu.title,
            theme::TEXT,
            "Claude",
            Rect::new(h.x + 16.0, h.y, h.w * 0.5, h.h),
        );
        let heard = match scene.usage {
            Some(u) => format!(
                "{} ago",
                format_age(Duration::from_secs(scene.now.saturating_sub(u.at)))
            ),
            None => "no usage yet".to_string(),
        };
        self.text(
            &gpu.small_right,
            theme::TEXT_DIM,
            &heard,
            Rect::new(h.x, h.y, h.w - 4.0, h.h),
        );

        if let Some(b) = l.limits_box {
            self.fill_rounded(&b, m.tile_radius, theme::TILE_BG);
            let limits = scene.usage.map(|u| u.limits.named()).unwrap_or_default();
            if limits.is_empty() {
                if let Some(r) = l.limits.first() {
                    let r = Rect::new(r.x + 10.0, r.y, r.w - 20.0, r.h);
                    self.text(
                        &gpu.small,
                        theme::TEXT_DIM,
                        "Shows after a reply in a Glance session",
                        r,
                    );
                }
            }
            for (r, (name, limit)) in l.limits.iter().zip(limits) {
                self.limit(gpu, r, name, &limit, scene.now);
            }
        }

        if let Some(b) = l.settings_box {
            self.fill_rounded(&b, m.tile_radius, theme::TILE_BG);
        }
        for (i, (r, (label, value))) in l.settings.iter().zip(&scene.settings).enumerate() {
            if let (Some(fill), _) = theme::button_look(scene.button(UsageHit::Setting(i))) {
                self.fill_rounded(&r.inset(2.0), 6.0, fill);
            }
            let inner = Rect::new(r.x + 10.0, r.y, r.w - 20.0, r.h);
            self.text(&gpu.small, theme::TEXT_DIM, label, inner);
            let ink = if *value == "Default" {
                theme::TEXT_DIM
            } else {
                theme::TEXT
            };
            self.text(&gpu.small_right, ink, &format!("{value}  \u{25BE}"), inner);
        }
    }

    /// One limit: its name and how much is used over a bar of it, and when
    /// it starts over.
    unsafe fn limit(&self, gpu: &Gpu, r: &Rect, name: &str, limit: &Limit, now: u64) {
        let (used, left) = limit.at(now);
        let inner = Rect::new(r.x + 10.0, r.y + 2.0, r.w - 20.0, 20.0);
        self.text(&gpu.body, theme::TEXT, name, inner);
        let numbers = match left {
            Some(s) => format!("{}% \u{00B7} resets in {}", used.round(), format_until(s)),
            None => format!("{}%", used.round()),
        };
        self.text(&gpu.small_right, theme::TEXT_DIM, &numbers, inner);
        let track = Rect::new(inner.x, inner.bottom() + 5.0, inner.w, 4.0);
        self.fill_rounded(&track, 2.0, theme::TILE_BG_STAGED);
        let fill = track.w * (used / 100.0).clamp(0.0, 1.0);
        if fill > 0.0 {
            let bar = Rect::new(track.x, track.y, fill.max(track.h), track.h);
            self.fill_rounded(&bar, 2.0, theme::fullness_color(used));
        }
    }

    /// The light that never stops while a session works or waits, drawn
    /// fresh over the kept layer every frame.
    unsafe fn light(&self, gpu: &Gpu, m: &Metrics, scene: &Scene) {
        let radius = m.tile_radius;
        for (r, s, look) in tiles(scene) {
            let phase = &s.phase;
            let c = theme::phase_color(phase);
            match phase {
                Phase::Working => {
                    let t = motion::cycle(look.phase_age, ORBIT);
                    self.comet(gpu, &r, radius, c, t, look.enter);
                }
                Phase::Waiting(_) => {
                    let breath = motion::breathe(look.phase_age, BREATH);
                    let strength = theme::edge_strength(phase) * (0.6 + 0.4 * breath);
                    self.glow_edge(&r, radius, c, strength * look.enter);
                    let (ix, iy) = icon_centre(&r);
                    let presence = theme::presence(phase) * look.enter;
                    self.glow_dot(ix, iy, 20.0, c, (0.28 + 0.2 * breath) * presence);
                }
                _ => {}
            }
        }
    }

    /// The project's colour, washed faintly down from the top edge, so each
    /// cluster reads as its own project before a word is read.
    unsafe fn wash(&self, scene: &Scene) {
        let (w, h) = scene.layout.size;
        let depth = 64.0f32.min(h);
        self.fill_gradient(
            &Rect::new(0.0, 0.0, w, depth),
            (0.0, depth),
            &[
                (0.0, scene.accent.with_alpha(0.16)),
                (1.0, scene.accent.with_alpha(0.0)),
            ],
        );
    }

    unsafe fn header(&self, gpu: &Gpu, scene: &Scene) {
        let Scene {
            layout,
            name,
            collapsed,
            sessions,
            ..
        } = *scene;
        let h = layout.header;
        let header_button = scene.button(Hit::Header);
        // Out into the padding on the left so the mark is not against its
        // edge, and short of the plus, which lights up on its own.
        if let (Some(fill), _) = theme::button_look(header_button) {
            let x = h.x - 4.0;
            let r = Rect::new(x, h.y + 3.0, layout.new.x - 2.0 - x, h.h - 6.0);
            self.fill_rounded(&r, 6.0, fill);
        }

        // The project's mark: a small gem in its colour, lit from within.
        let cy = h.y + h.h / 2.0;
        let gem_x = h.x + 5.0;
        self.glow_dot(gem_x, cy, 10.0, scene.accent, 0.35);
        self.fill_rounded(
            &Rect::new(gem_x - 4.0, cy - 4.0, 8.0, 8.0),
            2.5,
            scene.accent,
        );

        let name_x = h.x + 18.0;
        let name_w = self.measure(gpu, &gpu.display, name).min(h.w * 0.62);
        self.text(
            &gpu.display,
            theme::TEXT,
            name,
            Rect::new(name_x, h.y, name_w + 1.0, h.h),
        );
        // Folding is the header's click. The chevron says so where it
        // matters: under the cursor, and when folded.
        if collapsed || header_button != Button::Idle {
            let chevron = if collapsed { '\u{E76C}' } else { '\u{E70D}' };
            self.icon(
                &gpu.icon_small,
                theme::TEXT_DIM,
                chevron,
                Rect::new(name_x + name_w + 4.0, h.y + 1.0, 14.0, h.h),
            );
        }

        let waiting = sessions.iter().filter(|s| s.phase.is_waiting()).count();
        let working = sessions
            .iter()
            .filter(|s| s.phase == Phase::Working)
            .count();
        let mut right = layout.new.x - 2.0;
        for (n, label, c) in [
            (waiting, "waiting", theme::WAITING),
            (working, "working", theme::WORKING),
        ] {
            if n == 0 {
                continue;
            }
            let text = format!("{n} {label}");
            let w = self.measure(gpu, &gpu.chip, &text) + 14.0;
            let chip = Rect::new(right - w, cy - 9.0, w, 18.0);
            if chip.x < name_x + name_w + 20.0 {
                break;
            }
            self.fill_rounded(&chip, 9.0, c.with_alpha(0.14));
            self.text(
                &gpu.chip,
                c,
                &text,
                Rect::new(chip.x + 7.0, chip.y, w - 7.0, chip.h),
            );
            right = chip.x - 5.0;
        }

        let (fill, ink) = theme::button_look(scene.button(Hit::New));
        if let Some(fill) = fill {
            self.fill_rounded(&layout.new.inset(3.0), 6.0, fill);
        }
        self.icon(&gpu.icon_small, ink, '\u{E710}', layout.new);
    }

    unsafe fn fill_rounded(&self, r: &Rect, radius: f32, c: Color) {
        let rr = rounded(r, radius);
        self.brush.SetColor(&color(c));
        self.rt.FillRoundedRectangle(&rr, self.brush);
    }

    unsafe fn stroke_rounded(&self, r: &Rect, radius: f32, c: Color, width: f32) {
        let rr = rounded(r, radius);
        self.brush.SetColor(&color(c));
        self.rt.DrawRoundedRectangle(&rr, self.brush, width, None);
    }

    /// A vertical or diagonal gradient across `r`, from `(from_y, to_y)`
    /// with stops at 0 to 1 along it.
    unsafe fn fill_gradient(&self, r: &Rect, (from_y, to_y): (f32, f32), stops: &[(f32, Color)]) {
        let Some(brush) = self.linear(r.x, from_y, r.x, to_y, stops) else {
            return;
        };
        self.rt.FillRectangle(&rect(r), &brush);
    }

    unsafe fn gradient_stops(&self, stops: &[(f32, Color)]) -> Option<ID2D1GradientStopCollection> {
        let stops: Vec<D2D1_GRADIENT_STOP> = stops
            .iter()
            .map(|&(position, c)| D2D1_GRADIENT_STOP {
                position,
                color: color(c),
            })
            .collect();
        self.rt
            .CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)
            .ok()
    }

    unsafe fn linear(
        &self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        stops: &[(f32, Color)],
    ) -> Option<ID2D1LinearGradientBrush> {
        let key = stops_key(stops);
        let mut kept = self.gradients.linear.borrow_mut();
        if kept.len() > GRADIENTS_KEPT {
            kept.clear();
        }
        let brush = match kept.get(&key) {
            Some(b) => b.clone(),
            None => {
                let collection = self.gradient_stops(stops)?;
                let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES::default();
                let b = self
                    .rt
                    .CreateLinearGradientBrush(&props, None, &collection)
                    .ok()?;
                kept.insert(key, b.clone());
                b
            }
        };
        brush.SetStartPoint(Vector2 { X: x0, Y: y0 });
        brush.SetEndPoint(Vector2 { X: x1, Y: y1 });
        brush.SetOpacity(1.0);
        Some(brush)
    }

    unsafe fn radial(
        &self,
        x: f32,
        y: f32,
        radius: f32,
        stops: &[(f32, Color)],
    ) -> Option<ID2D1RadialGradientBrush> {
        let key = stops_key(stops);
        let mut kept = self.gradients.radial.borrow_mut();
        if kept.len() > GRADIENTS_KEPT {
            kept.clear();
        }
        let brush = match kept.get(&key) {
            Some(b) => b.clone(),
            None => {
                let collection = self.gradient_stops(stops)?;
                let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES::default();
                let b = self
                    .rt
                    .CreateRadialGradientBrush(&props, None, &collection)
                    .ok()?;
                kept.insert(key, b.clone());
                b
            }
        };
        brush.SetCenter(Vector2 { X: x, Y: y });
        brush.SetRadiusX(radius);
        brush.SetRadiusY(radius);
        brush.SetOpacity(1.0);
        Some(brush)
    }

    /// A soft round glow, strongest at its centre.
    unsafe fn glow_dot(&self, x: f32, y: f32, radius: f32, c: Color, strength: f32) {
        if strength <= 0.0 {
            return;
        }
        let c = c.with_alpha(1.0);
        let Some(brush) = self.radial(
            x,
            y,
            radius,
            &[
                (0.0, c),
                (0.45, c.with_alpha(0.4)),
                (1.0, c.with_alpha(0.0)),
            ],
        ) else {
            return;
        };
        brush.SetOpacity(strength.min(1.0));
        let e = D2D1_ELLIPSE {
            point: Vector2 { X: x, Y: y },
            radiusX: radius,
            radiusY: radius,
        };
        self.rt.FillEllipse(&e, &brush);
    }

    /// A tile's edge lit in `c`: a crisp line with light spilling off it.
    unsafe fn glow_edge(&self, r: &Rect, radius: f32, c: Color, strength: f32) {
        if strength <= 0.0 {
            return;
        }
        let edge = r.inset(0.5);
        self.stroke_rounded(&edge, radius - 0.5, c.with_alpha(strength * 0.09), 9.0);
        self.stroke_rounded(&edge, radius - 0.5, c.with_alpha(strength * 0.22), 4.0);
        self.stroke_rounded(&edge, radius - 0.5, c.with_alpha(strength), 1.2);
    }

    /// A light travelling round a tile's edge, `t` of the way round, with a
    /// tail that fades behind it.
    unsafe fn comet(&self, gpu: &Gpu, r: &Rect, radius: f32, c: Color, t: f32, strength: f32) {
        let edge = r.inset(0.5);
        let radius = radius - 0.5;
        let length = motion::perimeter(edge.w, edge.h, radius);
        let tail = (length * 0.3).min(140.0);
        let head = t * length;
        let rr = rounded(&edge, radius);
        let Ok(geometry) = gpu.d2d.CreateRoundedRectangleGeometry(&rr) else {
            return;
        };
        let mut at = Vector2::default();
        if geometry
            .ComputePointAtLength(head, None, 0.25, Some(&mut at), None)
            .is_err()
        {
            return;
        }
        let Some(fade) = self.radial(
            at.X,
            at.Y,
            tail,
            &[
                (0.0, c),
                (0.35, c.with_alpha(0.55)),
                (1.0, c.with_alpha(0.0)),
            ],
        ) else {
            return;
        };
        for (width, alpha) in [(5.0, 0.22), (1.5, 1.0)] {
            let Some(style) = dash(gpu, head - tail, tail, length, width) else {
                continue;
            };
            fade.SetOpacity(alpha * strength);
            self.rt.DrawRoundedRectangle(&rr, &fade, width, &style);
        }
        self.glow_dot(at.X, at.Y, 7.0, c, 0.5 * strength);
    }

    /// The stage shows a whole project, so the project is what gets marked,
    /// not each of its tiles: its colour around everything in the window.
    unsafe fn frame(&self, m: &Metrics, scene: &Scene) {
        let (w, h) = scene.layout.size;
        let inset = 1.0;
        let r = Rect::new(inset, inset, w - 2.0 * inset, h - 2.0 * inset);
        // Inside the corner DWM rounds the window to.
        let radius = m.window_radius - inset;
        self.stroke_rounded(&r, radius, scene.accent.with_alpha(0.14), 5.0);
        self.stroke_rounded(&r, radius, scene.accent.with_alpha(0.75), 1.2);
    }

    /// The `i`th tile, at `r` this frame, showing `s`.
    #[allow(clippy::too_many_arguments)]
    unsafe fn tile(
        &self,
        gpu: &Gpu,
        m: &Metrics,
        scene: &Scene,
        i: usize,
        r: &Rect,
        s: &Session,
        look: &Look,
    ) {
        let b = scene.button(Hit::Tile(i));
        // The layout puts the browser button where the tile will be; the
        // tile may still be sliding there.
        let slid = r.y - scene.layout.tiles.get(i).map_or(r.y, |t| t.y);
        let mark = scene
            .layout
            .marks
            .get(i)
            .copied()
            .flatten()
            .map(|k| Rect::new(k.x, k.y + slid, k.w, k.h));
        let phase = &s.phase;
        let c = theme::phase_color(phase);
        let presence = theme::presence(phase) * look.enter;
        let radius = m.tile_radius;
        let ambient = scene.ambient;

        // The pane of glass, and light catching its top edge.
        self.fill_rounded(r, radius, theme::GLASS_TILE.fade(look.enter));
        if let Some(edge) = self.linear(
            r.x,
            r.y,
            r.x,
            r.bottom(),
            &[
                (0.0, theme::GLASS_EDGE),
                (1.0, theme::GLASS_EDGE.fade(0.25)),
            ],
        ) {
            edge.SetOpacity(look.enter);
            self.rt
                .DrawRoundedRectangle(&rounded(&r.inset(0.5), radius - 0.5), &edge, 1.0, None);
        }

        // The phase, as light. Waiting needs you, so it is the one that
        // moves most: it breathes. Working has light going round it. A
        // finished turn flashes once.
        let arrival = if ambient { look.arrival } else { 0.0 };
        match phase {
            Phase::Waiting(_) => {
                self.fill_rounded(r, radius, c.with_alpha(0.1));
                if !ambient {
                    self.glow_edge(r, radius, c, theme::edge_strength(phase) * 0.85);
                }
                if arrival > 0.0 {
                    // A ring leaving the tile the moment it starts waiting.
                    let spread = (1.0 - arrival) * 5.0;
                    let ring = Rect::new(
                        r.x - spread,
                        r.y - spread,
                        r.w + 2.0 * spread,
                        r.h + 2.0 * spread,
                    );
                    self.stroke_rounded(&ring, radius + spread, c.with_alpha(arrival * 0.8), 1.5);
                }
            }
            Phase::Working => {
                self.fill_rounded(r, radius, c.with_alpha(0.05));
                self.glow_edge(r, radius, c, theme::edge_strength(phase) * look.enter);
            }
            Phase::Done => {
                self.fill_rounded(r, radius, c.with_alpha(0.03 + 0.22 * arrival));
                let strength = theme::edge_strength(phase) + 0.6 * arrival;
                self.glow_edge(r, radius, c, strength * look.enter);
            }
            Phase::Idle | Phase::Ended | Phase::Paused => {}
        }
        // Over the phase colour rather than instead of it, so a waiting
        // tile still reads as waiting under the cursor. The text keeps its
        // colours: dimming a session's name on a press would look like the
        // session changed.
        let hover = match b {
            Button::Pressed => 0.4,
            _ => look.hover,
        };
        if hover > 0.0 {
            self.fill_rounded(r, radius, theme::HOVER_FILL.fade(hover));
        }

        // The icon: what the agent is doing, in the phase's light.
        let icon_c = if matches!(phase, Phase::Idle | Phase::Ended | Phase::Paused) {
            theme::TEXT_DIM
        } else {
            c
        };
        let (ix, iy) = icon_centre(r);
        let halo = match phase {
            // The light layer breathes it.
            Phase::Waiting(_) if ambient => 0.0,
            Phase::Waiting(_) => 0.4,
            Phase::Working => 0.22,
            Phase::Done => 0.16 + 0.3 * arrival,
            _ => 0.0,
        };
        self.glow_dot(ix, iy, 20.0, c, halo * presence);
        let disc = D2D1_ELLIPSE {
            point: Vector2 { X: ix, Y: iy },
            radiusX: 14.0,
            radiusY: 14.0,
        };
        self.brush
            .SetColor(&color(icon_c.with_alpha(0.13 * presence)));
        self.rt.FillEllipse(&disc, self.brush);
        // How full the context is, as a ring round the icon filling
        // clockwise from the top. Only while the process that measured it
        // runs.
        let context = s
            .status
            .as_ref()
            .and_then(|st| st.context)
            .filter(|_| !matches!(phase, Phase::Paused | Phase::Ended));
        if let Some(c) = context {
            let ink = if c >= 75.0 {
                theme::fullness_color(c)
            } else {
                theme::TEXT_DIM.with_alpha(0.7)
            };
            self.ring(gpu, ix, iy, 16.0, 1.0, theme::TEXT_DIM.with_alpha(0.14));
            self.arc(gpu, ix, iy, 16.0, c / 100.0, ink.fade(presence));
        }
        self.icon(
            &gpu.icon,
            icon_c.fade(presence),
            theme::icon(phase, s.tool.as_deref()),
            Rect::new(ix - 14.0, iy - 14.0, 28.0, 28.0),
        );

        let pad = 10.0;
        let left = r.x + TILE_TEXT_X;
        let width = r.right() - pad - left;
        let row_h = r.h / 2.0;
        let top = Rect::new(left, r.y + 5.0, width, row_h - 3.0);
        let bottom = Rect::new(left, r.y + row_h - 1.0, width, row_h - 5.0);

        // Name, then how long it has been so, right aligned on the same row.
        // The icon already says working, done or idle. Waiting says what
        // for, since that decides what you do about it.
        let age = format_age(scene.now.duration_since(s.since).unwrap_or_default());
        let age = match phase {
            Phase::Waiting(_) | Phase::Paused | Phase::Ended => {
                format!("{} {age}", theme::phase_verb(phase))
            }
            _ => age,
        };
        let age_w = self.measure(gpu, &gpu.small, &age).min(top.w * 0.55);
        let name_rect = Rect::new(top.x, top.y, top.w - age_w - 8.0, top.h);
        self.text(&gpu.name, theme::TEXT.fade(presence), s.label(), name_rect);
        let age_c = match phase {
            Phase::Waiting(_) => c,
            _ => theme::TEXT_DIM,
        };
        self.text_tabular(gpu, &gpu.small_right, age_c.fade(presence), &age, top);

        // Last line, and what the agent did lately beside it.
        let last = if s.last_line.is_empty() {
            &s.cwd
        } else {
            &s.last_line
        };
        // Stopping short of a browser button at its end.
        let short = if mark.is_some() { m.mark_w + 2.0 } else { 0.0 };
        let mut bottom = Rect::new(bottom.x, bottom.y, bottom.w - short, bottom.h);
        // A context nearly full is worth words, not only the ring: it says
        // the session is due a `/compact` or a fresh start. It takes the
        // trace's place.
        let crowded = context.filter(|&c| c >= 75.0);
        if let Some(c) = crowded {
            let w = 58.0;
            let at = Rect::new(bottom.right() - w, bottom.y, w, bottom.h);
            self.text_tabular(
                gpu,
                &gpu.small_right,
                theme::fullness_color(c).fade(presence),
                &format!("ctx {}%", c.round()),
                at,
            );
            bottom.w -= w + 4.0;
        }
        let activity = s.activity(scene.now, TRACE_BARS);
        let trace_w = TRACE_BARS as f32 * (TRACE_BAR_W + TRACE_GAP) - TRACE_GAP;
        let busy = crowded.is_none() && activity.iter().any(|&a| a > 0.0);
        let text_w = if busy {
            bottom.w - trace_w - 8.0
        } else {
            bottom.w
        };
        self.text(
            &gpu.small,
            theme::TEXT_DIM.fade(presence),
            last,
            Rect::new(bottom.x, bottom.y, text_w, bottom.h),
        );
        if busy {
            let trace_c = if icon_c == theme::TEXT_DIM {
                theme::TEXT_DIM.with_alpha(0.45)
            } else {
                c.with_alpha(0.7)
            };
            let base = bottom.y + bottom.h / 2.0 + TRACE_H / 2.0;
            self.trace(
                &activity,
                bottom.right() - trace_w,
                base,
                trace_c.fade(presence),
            );
        }
        if let Some(mark) = mark {
            self.browser_mark(&mark, scene.button(Hit::Browser(i)));
        }
    }

    /// A circle's outline.
    unsafe fn ring(&self, _gpu: &Gpu, x: f32, y: f32, radius: f32, width: f32, c: Color) {
        self.brush.SetColor(&color(c));
        self.rt.DrawEllipse(
            &D2D1_ELLIPSE {
                point: Vector2 { X: x, Y: y },
                radiusX: radius,
                radiusY: radius,
            },
            self.brush,
            width,
            None,
        );
    }

    /// `fraction` of a circle's outline, clockwise from the top.
    unsafe fn arc(&self, gpu: &Gpu, x: f32, y: f32, radius: f32, fraction: f32, c: Color) {
        let fraction = fraction.clamp(0.0, 1.0);
        if fraction <= 0.0 {
            return;
        }
        if fraction >= 0.999 {
            self.ring(gpu, x, y, radius, 1.6, c);
            return;
        }
        let Ok(path) = gpu.d2d.CreatePathGeometry() else {
            return;
        };
        let Ok(sink) = path.Open() else {
            return;
        };
        let angle = fraction * std::f32::consts::TAU;
        sink.BeginFigure(
            Vector2 {
                X: x,
                Y: y - radius,
            },
            D2D1_FIGURE_BEGIN_HOLLOW,
        );
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: Vector2 {
                X: x + radius * angle.sin(),
                Y: y - radius * angle.cos(),
            },
            size: D2D_SIZE_F {
                width: radius,
                height: radius,
            },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
            arcSize: if fraction > 0.5 {
                D2D1_ARC_SIZE_LARGE
            } else {
                D2D1_ARC_SIZE_SMALL
            },
        });
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
        if sink.Close().is_err() {
            return;
        }
        let round = gpu
            .d2d
            .CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    dashCap: D2D1_CAP_STYLE_ROUND,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    miterLimit: 1.0,
                    dashStyle: D2D1_DASH_STYLE_SOLID,
                    dashOffset: 0.0,
                },
                None,
            )
            .ok();
        self.brush.SetColor(&color(c));
        self.rt.DrawGeometry(&path, self.brush, 1.6, round.as_ref());
    }

    /// Bars of how busy a session was over the last minutes, oldest on the
    /// left, standing on `base`. A slice with nothing in it is a dot, so
    /// quiet reads as quiet rather than as missing.
    unsafe fn trace(&self, activity: &[f32], x: f32, base: f32, c: Color) {
        for (i, &a) in activity.iter().enumerate() {
            let bx = x + i as f32 * (TRACE_BAR_W + TRACE_GAP);
            let h = if a > 0.0 {
                2.0 + a * (TRACE_H - 2.0)
            } else {
                1.0
            };
            let alpha = if a > 0.0 { 1.0 } else { 0.35 };
            self.fill_rounded(
                &Rect::new(bx, base - h, TRACE_BAR_W, h),
                TRACE_BAR_W / 2.0,
                c.fade(alpha),
            );
        }
    }

    /// The session has a browser open: a small window with a tab bar, the
    /// shape every browser shares. A button, since a click brings it up.
    unsafe fn browser_mark(&self, r: &Rect, b: Button) {
        let (fill, ink) = theme::button_look(b);
        if let Some(fill) = fill {
            self.fill_rounded(r, 5.0, fill);
        }
        let (w, h) = (14.0, 11.0);
        let x = (r.x + (r.w - w) / 2.0).round() + 0.5;
        let y = (r.y + (r.h - h) / 2.0).round() + 0.5;
        let outline = D2D1_ROUNDED_RECT {
            rect: rect(&Rect::new(x, y, w - 1.0, h - 1.0)),
            radiusX: 2.0,
            radiusY: 2.0,
        };
        self.brush.SetColor(&color(ink));
        self.rt
            .DrawRoundedRectangle(&outline, self.brush, 1.2, None);
        self.rt.DrawLine(
            Vector2 { X: x, Y: y + 3.0 },
            Vector2 {
                X: x + w - 1.0,
                Y: y + 3.0,
            },
            self.brush,
            1.2,
            None,
        );
    }

    /// Another session in this project: an empty slot where the next tile
    /// would go, a dashed outline so it never reads as a session.
    unsafe fn add(&self, gpu: &Gpu, m: &Metrics, r: &Rect, b: Button) {
        let (fill, ink) = theme::button_look(b);
        if let Some(fill) = fill {
            self.fill_rounded(r, m.tile_radius, fill);
        }
        let edge = r.inset(0.75);
        let radius = m.tile_radius - 0.75;
        let length = motion::perimeter(edge.w, edge.h, radius);
        // Whole dashes all the way round, so no seam where the outline meets
        // itself.
        let count = (length / 7.0).round().max(1.0);
        let unit = length / count / 1.2;
        let style = gpu.d2d.CreateStrokeStyle(
            &D2D1_STROKE_STYLE_PROPERTIES {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                dashCap: D2D1_CAP_STYLE_ROUND,
                lineJoin: D2D1_LINE_JOIN_ROUND,
                miterLimit: 1.0,
                dashStyle: D2D1_DASH_STYLE_CUSTOM,
                dashOffset: 0.0,
            },
            Some(&[unit * 0.35, unit * 0.65]),
        );
        let outline = theme::TEXT_DIM.with_alpha(if b == Button::Idle { 0.28 } else { 0.45 });
        self.brush.SetColor(&color(outline));
        self.rt.DrawRoundedRectangle(
            &rounded(&edge, radius),
            self.brush,
            1.2,
            style.ok().as_ref(),
        );
        self.icon(&gpu.icon_small, ink, '\u{E710}', *r);
    }

    /// The project's files as VS Code's explorer shows them: folders with a
    /// chevron, names in the colour of their change, the change letter at the
    /// right edge, a dot on a folder holding one.
    unsafe fn files(&self, gpu: &Gpu, m: &Metrics, l: &FilesLayout, f: &FilesScene) {
        self.fill_rounded(&l.rect, m.tile_radius, theme::GLASS_TILE);
        if let Some(edge) = self.linear(
            l.rect.x,
            l.rect.y,
            l.rect.x,
            l.rect.bottom(),
            &[
                (0.0, theme::GLASS_EDGE),
                (1.0, theme::GLASS_EDGE.fade(0.25)),
            ],
        ) {
            self.rt.DrawRoundedRectangle(
                &rounded(&l.rect.inset(0.5), m.tile_radius - 0.5),
                &edge,
                1.0,
                None,
            );
        }

        let pad = 10.0;
        let h = l.header;
        let chevron = if f.collapsed { '\u{E76C}' } else { '\u{E70D}' };
        self.icon(
            &gpu.icon_small,
            theme::TEXT_DIM,
            chevron,
            Rect::new(h.x + pad - 2.0, h.y, 14.0, h.h),
        );
        self.text_spaced(
            gpu,
            &gpu.chip,
            theme::TEXT_DIM,
            "FILES",
            1.2,
            Rect::new(h.x + pad + 14.0, h.y, h.w * 0.5, h.h),
        );
        let (summary, c) = match f.tree.changed {
            0 => ("no changes".to_string(), theme::TEXT_DIM),
            n => (format!("{n} changed"), theme::GIT_MODIFIED),
        };
        self.text_tabular(
            gpu,
            &gpu.small_right,
            c,
            &summary,
            Rect::new(h.x, h.y, h.w - pad, h.h),
        );

        let visible = f.rows.iter().skip(f.scroll);
        let badge_w = 18.0;
        for (r, row) in l.rows.iter().zip(visible) {
            let node = f.tree.node(row.node);
            let x = r.x + pad + row.depth as f32 * m.file_indent;
            if node.dir {
                let chevron = if row.open { '\u{E70D}' } else { '\u{E76C}' };
                self.icon(
                    &gpu.icon_small,
                    theme::TEXT_DIM.with_alpha(0.8),
                    chevron,
                    Rect::new(x - 2.0, r.y, 14.0, r.h),
                );
            }
            let name_c = node.change.map_or(theme::TEXT, theme::change_color);
            let name_x = x + 14.0;
            let name = Rect::new(name_x, r.y, r.right() - pad - badge_w - name_x, r.h);
            self.text(&gpu.body, name_c, &row.label, name);

            let Some(change) = node.change else {
                continue;
            };
            let badge = Rect::new(r.right() - pad - badge_w, r.y, badge_w, r.h);
            if node.dir {
                let (dx, dy) = (badge.right() - 4.0, badge.y + badge.h / 2.0);
                self.glow_dot(dx, dy, 6.0, theme::change_color(change), 0.35);
                let dot = D2D1_ELLIPSE {
                    point: Vector2 { X: dx, Y: dy },
                    radiusX: 2.5,
                    radiusY: 2.5,
                };
                self.brush.SetColor(&color(theme::change_color(change)));
                self.rt.FillEllipse(&dot, self.brush);
            } else {
                self.text(
                    &gpu.small_right,
                    theme::change_color(change),
                    change.letter(),
                    badge,
                );
            }
        }

        // Where the view is in a list longer than the tile.
        let shown = l.rows.len();
        if shown > 0 && f.rows.len() > shown {
            let body = l.body();
            let track = shown as f32 * m.file_row_h;
            let total = f.rows.len() as f32;
            let thumb_h = (track * shown as f32 / total).max(12.0);
            let thumb_y = body.y + (track - thumb_h) * f.scroll as f32 / (total - shown as f32);
            self.fill_rounded(
                &Rect::new(body.right() - 4.0, thumb_y, 3.0, thumb_h),
                1.5,
                theme::TEXT_DIM.with_alpha(0.5),
            );
        }

        // The bottom edge can be dragged, and nothing else says so until
        // the cursor changes over it.
        if l.grip.is_some() {
            let w = 24.0;
            self.fill_rounded(
                &Rect::new(
                    l.rect.x + (l.rect.w - w) / 2.0,
                    l.rect.bottom() - m.file_foot / 2.0 - 1.0,
                    w,
                    2.0,
                ),
                1.0,
                theme::TEXT_DIM.with_alpha(0.35),
            );
        }
    }

    unsafe fn text(&self, fmt: &IDWriteTextFormat, c: Color, s: &str, r: Rect) {
        if r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        let wide: Vec<u16> = s.encode_utf16().collect();
        self.brush.SetColor(&color(c));
        self.rt.DrawText(
            &wide,
            fmt,
            &rect(&r),
            self.brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }

    unsafe fn icon(&self, fmt: &IDWriteTextFormat, c: Color, glyph: char, r: Rect) {
        let mut buf = [0u8; 4];
        self.text(fmt, c, glyph.encode_utf8(&mut buf), r);
    }

    /// Text with every digit the same width, so a counting age does not
    /// shuffle the letters beside it each second.
    unsafe fn text_tabular(&self, gpu: &Gpu, fmt: &IDWriteTextFormat, c: Color, s: &str, r: Rect) {
        let Some(layout) = self.layout(gpu, fmt, s, r) else {
            return;
        };
        if let Ok(typography) = gpu.dw.CreateTypography() {
            let _ = typography.AddFontFeature(DWRITE_FONT_FEATURE {
                nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                parameter: 1,
            });
            let _ = layout.SetTypography(&typography, whole(s));
        }
        self.draw_layout(&layout, c, r);
    }

    /// Small capitals read better with air between the letters.
    unsafe fn text_spaced(
        &self,
        gpu: &Gpu,
        fmt: &IDWriteTextFormat,
        c: Color,
        s: &str,
        spacing: f32,
        r: Rect,
    ) {
        let Some(layout) = self.layout(gpu, fmt, s, r) else {
            return;
        };
        if let Ok(l1) = layout.cast::<IDWriteTextLayout1>() {
            let _ = l1.SetCharacterSpacing(0.0, spacing, 0.0, whole(s));
        }
        self.draw_layout(&layout, c, r);
    }

    unsafe fn layout(
        &self,
        gpu: &Gpu,
        fmt: &IDWriteTextFormat,
        s: &str,
        r: Rect,
    ) -> Option<IDWriteTextLayout> {
        if r.w <= 0.0 || r.h <= 0.0 {
            return None;
        }
        let wide: Vec<u16> = s.encode_utf16().collect();
        gpu.dw.CreateTextLayout(&wide, fmt, r.w, r.h).ok()
    }

    unsafe fn draw_layout(&self, layout: &IDWriteTextLayout, c: Color, r: Rect) {
        self.brush.SetColor(&color(c));
        self.rt.DrawTextLayout(
            Vector2 { X: r.x, Y: r.y },
            layout,
            self.brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
    }

    /// How wide a line of text is.
    unsafe fn measure(&self, gpu: &Gpu, fmt: &IDWriteTextFormat, s: &str) -> f32 {
        let wide: Vec<u16> = s.encode_utf16().collect();
        gpu.dw
            .CreateTextLayout(&wide, fmt, 10_000.0, 100.0)
            .and_then(|l| {
                let mut m = Default::default();
                l.GetMetrics(&mut m)
                    .map(|_| m.widthIncludingTrailingWhitespace)
            })
            .unwrap_or(0.0)
    }
}

/// Each tile with where it is this frame and how it looks.
fn tiles<'a>(scene: &'a Scene) -> impl Iterator<Item = (Rect, &'a Session, Look)> + 'a {
    scene
        .layout
        .tiles
        .iter()
        .zip(scene.sessions)
        .enumerate()
        .map(|(i, (rect, s))| {
            let look = scene
                .looks
                .get(i)
                .copied()
                .unwrap_or_else(|| Look::still(rect.y));
            (Rect::new(rect.x, look.y, rect.w, rect.h), *s, look)
        })
}

fn icon_centre(r: &Rect) -> (f32, f32) {
    (r.x + 23.0, r.y + r.h / 2.0)
}

/// A stroke with one dash, `length` long, starting `from` along an outline
/// `total` long, for a line `width` wide. Dashes are measured in widths.
unsafe fn dash(
    gpu: &Gpu,
    from: f32,
    length: f32,
    total: f32,
    width: f32,
) -> Option<ID2D1StrokeStyle> {
    let from = from.rem_euclid(total);
    gpu.d2d
        .CreateStrokeStyle(
            &D2D1_STROKE_STYLE_PROPERTIES {
                startCap: D2D1_CAP_STYLE_FLAT,
                endCap: D2D1_CAP_STYLE_FLAT,
                dashCap: D2D1_CAP_STYLE_FLAT,
                lineJoin: D2D1_LINE_JOIN_ROUND,
                miterLimit: 1.0,
                dashStyle: D2D1_DASH_STYLE_CUSTOM,
                // A positive offset pulls the pattern back toward the start,
                // so this puts the dash's start at `from`.
                dashOffset: (total - from) / width,
            },
            Some(&[length / width, (total - length) / width]),
        )
        .ok()
}

fn whole(s: &str) -> DWRITE_TEXT_RANGE {
    DWRITE_TEXT_RANGE {
        startPosition: 0,
        length: s.encode_utf16().count() as u32,
    }
}

fn rounded(r: &Rect, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT {
        rect: rect(r),
        radiusX: radius.max(0.0),
        radiusY: radius.max(0.0),
    }
}

pub(crate) fn color(c: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
}

fn rect(r: &Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.x,
        top: r.y,
        right: r.right(),
        bottom: r.bottom(),
    }
}
