//! What says, on every screen, that a session has the desktop: a thin,
//! soft light along the screen's edges, as if a lamp stood just past
//! them. It is never still and never pulses: two slow waves drift along
//! it against each other, and a brighter breath of light goes slowly
//! round the screen, swelling as it comes and fading behind.
//!
//! The human chose this over runes cut round the screen, which did not
//! look good on their desktop (2026-11-26), and for this one thing over
//! their rule that activity is shown by runes and not by glows. It stays
//! light and not a border: nothing sharp is on screen, only what a line
//! of light lying just off the edge spills inward, and what is under it
//! shows through it everywhere.
//!
//! The graphics card draws it and the compositor moves it. Each part of
//! the light (the steady line, each wave, the breath) is drawn once, as
//! the app's other light is: sharp into a [`crate::light::Light`] and
//! bloomed by it. Each is then a DirectComposition visual, and what moves
//! is the visual, by an animation the compositor runs itself. So after
//! it is made the light costs this process nothing: no frame, no timer,
//! no paint, however long control is on. Only its fading in and out is
//! set from here, a number a frame for a quarter of a second.
//!
//! Each edge is one narrow window with no surface of its own, that the
//! mouse goes through and that is never activated. With animations off
//! the line and the waves stand still and the breath is left out.
//!
//! Without a graphics card (a remote desktop session) the same is drawn
//! by the processor and [`crate::light::Light`] leaves the blur out: the
//! line is then let a little way onto the screen and shows as it is,
//! thin, which still marks the screen.

use std::f32::consts::TAU;
use std::time::Duration;

use windows::core::{w, Interface, Result};
use windows::Win32::Foundation::{HMODULE, HWND, POINT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_COMPOSITE_MODE_SOURCE_COPY,
    D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DeviceContext, ID2D1Factory1, ID2D1RenderTarget, D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
    D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE,
    D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionEffectGroup,
    IDCompositionSurface, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
};
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, IDXGISurface};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, SetLayeredWindowAttributes, ShowWindow, LWA_ALPHA,
    SW_SHOWNOACTIVATE, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows_numerics::{Matrix3x2, Vector2};

use super::CLASS;
use crate::layout::Rect;
use crate::light::{Bloom, Light};
use crate::motion;
use crate::particles;
use crate::render::{self, color};
use crate::theme::Color;

/// How the line's light spreads: tight enough to be an edge of light,
/// wide enough to have no edge of its own.
const BLOOM: Bloom = Bloom {
    near: 3.0,
    far: 9.0,
    far_passes: 2,
    gain: 1.0,
};
/// How far inward the light reaches, in DIPs: three of the wide blur's
/// sigmas, past which it is all but gone.
const REACH: f32 = 27.0;
/// The line of light: how wide, and how far past the screen's edge its
/// middle lies. Past the edge by half its width, so the line itself is
/// never on screen and only what it spills is.
const LINE: f32 = 6.0;
const BEYOND: f32 = 3.0;
/// Room the line needs in its layer on the far side.
const BEHIND: f32 = 4.0;
/// An edge's window: as deep as the light reaches.
pub(super) const DEPTH: f32 = REACH;
/// What a part of the light is drawn in before it is pushed outward:
/// deep enough for the reach and for what ends up past the edge.
const LAYER: f32 = REACH + BEHIND + BEYOND;
/// How far apart a part's brightness is worked out along it, in DIPs.
/// The shortest wave is several of these long.
const STOP: f32 = 24.0;

/// How bright the line is with nothing on it, how much each drifting
/// wave adds at its crest, and how much the light going round does.
const STEADY: f32 = 0.22;
const WAVE: f32 = 0.10;
const BREATH: f32 = 0.32;
/// The two waves: about how long each is, in DIPs, and how far it drifts
/// in a second, clockwise or against. Against each other, so their
/// crests meet and part and the light never repeats itself in a way the
/// eye finds.
const WAVES: [(f32, f32); 2] = [(520.0, 22.0), (190.0, -13.0)];
/// How far the light going round travels in a second, in DIPs, how far
/// ahead of itself it swells and how long it takes to fade behind.
const SPEED: f32 = 70.0;
pub(super) const SWELL: f32 = 320.0;
const FADE: Duration = Duration::from_millis(6500);
/// Lights going round each screen, evenly apart.
const LIGHTS: usize = 2;

/// How long the breath's tail is, in DIPs.
fn tail() -> f32 {
    SPEED * FADE.as_secs_f32()
}

/// How bright the breath of light is `ahead` of its head, in DIPs, or
/// behind it where that is negative: swelling as it comes, full as it
/// passes, fading behind. From 0 to 1.
pub(super) fn breath(ahead: f32) -> f32 {
    if ahead >= 0.0 {
        motion::ease_in_out(1.0 - (ahead / SWELL).min(1.0))
    } else {
        crate::sparks::cool(-ahead / SPEED, FADE)
    }
}

/// A wave `about` long fitted to an edge `long` long, so a whole number
/// of it lies along the edge.
pub(super) fn fitted(about: f32, long: f32) -> f32 {
    long / (long / about).round().max(1.0)
}

/// Where a light going round a ring `len` long is on an edge that starts
/// `start` along the ring, for a light now at `head`: how far past the
/// edge's start, from one swell before it (where its front first reaches
/// the edge) up to a lap later.
pub(super) fn lap_from(head: f32, start: f32, len: f32) -> f32 {
    (head - start + SWELL).rem_euclid(len.max(f32::EPSILON)) - SWELL
}

/// Which edge of its screen a strip is on. The light goes round
/// clockwise: rightward along the top, down the right, leftward along
/// the bottom, up the left.
#[derive(Clone, Copy, PartialEq)]
enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

impl Side {
    fn across(self) -> bool {
        matches!(self, Side::Top | Side::Bottom)
    }

    /// Whether clockwise is toward greater x or y along this edge.
    fn forward(self) -> bool {
        matches!(self, Side::Top | Side::Right)
    }
}

/// What draws and composes: one device for every strip.
struct Gfx {
    comp: IDCompositionDevice,
    dc: ID2D1DeviceContext,
}

struct Strip {
    hwnd: HWND,
    // Kept alive: the composition lets go of what nobody holds.
    _target: IDCompositionTarget,
    _visuals: Vec<IDCompositionVisual>,
    /// Fades all of the strip.
    fade: IDCompositionEffectGroup,
}

impl Drop for Strip {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

pub(super) struct Frame {
    gfx: Gfx,
    strips: Vec<Strip>,
    /// What it was made for: a change of either makes it anew.
    pub tint: Color,
    pub moving: bool,
    /// How strongly it was last shown.
    shown: Option<f32>,
}

unsafe fn device(hardware: bool) -> Result<ID3D11Device> {
    let mut d3d = None;
    D3D11CreateDevice(
        None,
        if hardware {
            D3D_DRIVER_TYPE_HARDWARE
        } else {
            D3D_DRIVER_TYPE_WARP
        },
        HMODULE::default(),
        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        None,
        D3D11_SDK_VERSION,
        Some(&mut d3d),
        None,
        None,
    )?;
    d3d.ok_or_else(windows::core::Error::empty)
}

fn pixel_format() -> D2D1_PIXEL_FORMAT {
    D2D1_PIXEL_FORMAT {
        format: DXGI_FORMAT_B8G8R8A8_UNORM,
        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
    }
}

/// How a visual moves along its edge, in pixels: at `speed` a second
/// from `begin` to `end`, over and over.
#[derive(Clone, Copy)]
struct Moves {
    speed: f32,
    begin: f32,
    end: f32,
}

/// An edge's place on the ring round its screen: the ring's length, how
/// far along it the edge starts, and where the first light is now, all
/// in DIPs.
#[derive(Clone, Copy)]
struct Ring {
    len: f32,
    start: f32,
    head: f32,
}

/// An edge of a screen, as its light is made for it.
#[derive(Clone, Copy)]
struct Edge {
    side: Side,
    /// Its length in DIPs, and its screen's scale.
    long: f32,
    scale: f32,
    tint: Color,
    moving: bool,
    ring: Ring,
}

impl Gfx {
    unsafe fn new(gpu: &render::Gpu) -> Result<Gfx> {
        let d3d = device(true).or_else(|_| device(false))?;
        let dxgi: IDXGIDevice = d3d.cast()?;
        let comp: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
        let d2d = gpu.d2d.cast::<ID2D1Factory1>()?.CreateDevice(&dxgi)?;
        let dc = d2d.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
        Ok(Gfx { comp, dc })
    }

    /// One part of an edge's light, drawn once: a line `long` DIPs long
    /// just past the edge, as bright along it as `level` says (0 to 1,
    /// by DIPs from the part's start), bloomed, as a surface the
    /// composition can lay down and move.
    unsafe fn part(
        &self,
        e: &Edge,
        long: f32,
        level: &dyn Fn(f32) -> f32,
    ) -> Result<IDCompositionSurface> {
        let dc = &self.dc;
        let rt: ID2D1RenderTarget = dc.cast()?;
        let px = |dips: f32| ((dips * e.scale).ceil() as u32).max(1);
        // Drawn deeper than it shows, then pushed outward until the
        // line's middle is `BEYOND` past the edge.
        let (drawn, kept) = if e.side.across() {
            ((px(long), px(LAYER)), (px(long), px(DEPTH)))
        } else {
            ((px(LAYER), px(long)), (px(DEPTH), px(long)))
        };
        let dpi = 96.0 * e.scale;
        let target = |size: (u32, u32)| {
            dc.CreateBitmap(
                D2D_SIZE_U {
                    width: size.0,
                    height: size.1,
                },
                None,
                0,
                &D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: pixel_format(),
                    dpiX: dpi,
                    dpiY: dpi,
                    bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET,
                    ..Default::default()
                },
            )
        };
        let bloomed = target(kept)?;
        // The light's layer is as large as the target it is begun on: a
        // deeper one, so the line has room behind it.
        let deep = target(drawn)?;
        let light = Light::default();
        dc.SetDpi(dpi, dpi);
        dc.SetTarget(&deep);
        dc.BeginDraw();
        let begun = light.begin(&rt);
        dc.SetTarget(&bloomed);
        dc.Clear(Some(&D2D1_COLOR_F::default()));
        let drawn = begun.and_then(|layer| {
            let across = match e.side {
                Side::Top | Side::Left => BEHIND,
                Side::Bottom | Side::Right => LAYER - BEHIND,
            };
            let count = (long / STOP).ceil().max(1.0) as usize;
            let stops: Vec<D2D1_GRADIENT_STOP> = (0..=count)
                .map(|i| {
                    let position = i as f32 / count as f32;
                    let level = level(position * long).clamp(0.0, 1.0);
                    // The one ramp all of Horadric's light is coloured
                    // through: a little whiter where it is brightest.
                    let ink = particles::heat_color(e.tint, 0.6 * level).with_alpha(level);
                    D2D1_GRADIENT_STOP {
                        position,
                        color: color(ink),
                    }
                })
                .collect();
            let (a, b) = if e.side.across() {
                ((0.0, across), (long, across))
            } else {
                ((across, 0.0), (across, long))
            };
            let (a, b) = (Vector2 { X: a.0, Y: a.1 }, Vector2 { X: b.0, Y: b.1 });
            let ramp =
                rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            let along = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                startPoint: a,
                endPoint: b,
            };
            let brush = rt.CreateLinearGradientBrush(&along, None, &ramp)?;
            layer.DrawLine(a, b, &brush, LINE, None);
            let (lw, lh) = if e.side.across() {
                (long, LAYER)
            } else {
                (LAYER, long)
            };
            light.lit(Rect::new(0.0, 0.0, lw, lh));
            // Where nothing blurs there is nothing to spill: the line is
            // then let in by a third of its width, to show at all.
            let beyond = if light.blooms(&rt) { BEYOND } else { 1.0 };
            let push = BEHIND + beyond;
            let (dx, dy) = match e.side {
                Side::Top => (0.0, -push),
                Side::Left => (-push, 0.0),
                // These two were drawn deeper on their near side.
                Side::Bottom => (0.0, DEPTH - LAYER + push),
                Side::Right => (DEPTH - LAYER + push, 0.0),
            };
            dc.SetTransform(&Matrix3x2::translation(dx, dy));
            let lit = light.present(&rt, &BLOOM);
            dc.SetTransform(&Matrix3x2::identity());
            lit
        });
        let ended = dc.EndDraw(None, None);
        drawn?;
        ended?;

        // Onto a surface of the composition's, which may be a part of a
        // larger one: copied to where it says.
        let surface = self.comp.CreateSurface(
            kept.0,
            kept.1,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            DXGI_ALPHA_MODE_PREMULTIPLIED,
        )?;
        let mut at = POINT::default();
        let dxgi: IDXGISurface = surface.BeginDraw(None, &mut at)?;
        let onto = dc.CreateBitmapFromDxgiSurface(
            &dxgi,
            Some(&D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: pixel_format(),
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                ..Default::default()
            }),
        );
        let copied = onto.and_then(|onto| {
            dc.SetTarget(&onto);
            dc.SetDpi(96.0, 96.0);
            dc.BeginDraw();
            dc.DrawImage(
                &bloomed,
                Some(&Vector2 {
                    X: at.x as f32,
                    Y: at.y as f32,
                }),
                None,
                D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                D2D1_COMPOSITE_MODE_SOURCE_COPY,
            );
            dc.EndDraw(None, None)
        });
        dc.SetTarget(None);
        surface.EndDraw()?;
        copied?;
        Ok(surface)
    }

    /// A visual showing `surface` with its start `at` pixels along its
    /// edge, and moving as `moves` says when it moves.
    unsafe fn visual(
        &self,
        side: Side,
        surface: &IDCompositionSurface,
        at: f32,
        moves: Option<Moves>,
    ) -> Result<IDCompositionVisual> {
        let v = self.comp.CreateVisual()?;
        v.SetContent(surface)?;
        match moves {
            None if side.across() => v.SetOffsetX2(at)?,
            None => v.SetOffsetY2(at)?,
            Some(m) => {
                // Straight from where it is to where its lap ends, then
                // lap after lap from where a lap begins. The compositor
                // runs this by itself from now on.
                let a = self.comp.CreateAnimation()?;
                let first = f64::from(((m.end - at) / m.speed).max(0.0));
                let lap = f64::from(((m.end - m.begin) / m.speed).abs().max(0.01));
                if first > 0.001 {
                    a.AddCubic(0.0, at, m.speed, 0.0, 0.0)?;
                }
                a.AddCubic(first, m.begin, m.speed, 0.0, 0.0)?;
                a.AddRepeat(first + lap, lap)?;
                if side.across() {
                    v.SetOffsetX(&a)?;
                } else {
                    v.SetOffsetY(&a)?;
                }
            }
        }
        Ok(v)
    }

    /// One edge: its window at `at` and `size` in screen pixels, and in
    /// it the steady line, the two waves and the lights going round,
    /// each a visual of its own.
    unsafe fn strip(&self, at: (i32, i32), size: (i32, i32), e: &Edge) -> Result<Strip> {
        // No redirection bitmap: the window has no surface of its own,
        // only what the composition lays in it. Layered and transparent,
        // so the mouse goes through it to whatever program is under.
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP
                | WS_EX_LAYERED
                | WS_EX_TRANSPARENT
                | WS_EX_TOPMOST
                | WS_EX_NOACTIVATE
                | WS_EX_TOOLWINDOW,
            CLASS,
            w!(""),
            WS_POPUP,
            at.0,
            at.1,
            size.0,
            size.1,
            None,
            None,
            Some(GetModuleHandleW(None)?.into()),
            None,
        )?;
        let made = self.fill(hwnd, e);
        if made.is_err() {
            let _ = DestroyWindow(hwnd);
        }
        let (target, visuals, fade) = made?;
        Ok(Strip {
            hwnd,
            _target: target,
            _visuals: visuals,
            fade,
        })
    }

    unsafe fn fill(
        &self,
        hwnd: HWND,
        e: &Edge,
    ) -> Result<(
        IDCompositionTarget,
        Vec<IDCompositionVisual>,
        IDCompositionEffectGroup,
    )> {
        // A layered window shows nothing until it is given its alpha.
        SetLayeredWindowAttributes(hwnd, Default::default(), 255, LWA_ALPHA)?;
        let target = self.comp.CreateTargetForHwnd(hwnd, true)?;
        let root = self.comp.CreateVisual()?;
        let fade = self.comp.CreateEffectGroup()?;
        fade.SetOpacity2(0.0)?;
        root.SetEffect(&fade)?;
        let mut visuals = Vec::new();
        let mut add = |v: IDCompositionVisual| -> Result<()> {
            root.AddVisual(&v, true, None::<&IDCompositionVisual>)?;
            visuals.push(v);
            Ok(())
        };
        let (side, long, scale) = (e.side, e.long, e.scale);
        // Clockwise is toward greater x or y on the top and the right,
        // and toward less on the bottom and the left.
        let way = if side.forward() { 1.0 } else { -1.0 };

        let steady = self.part(e, long, &|_| STEADY)?;
        add(self.visual(side, &steady, 0.0, None)?)?;

        for (about, drift) in WAVES {
            // One wave longer than the edge, so that sliding by one wave
            // it always covers the edge and ends where it began.
            let wave = fitted(about, long);
            let crest = |d: f32| WAVE * (0.5 + 0.5 * (TAU * d / wave).sin());
            let part = self.part(e, long + wave, &crest)?;
            let speed = drift * way * scale;
            let (begin, end) = if speed > 0.0 {
                (-wave * scale, 0.0)
            } else {
                (0.0, -wave * scale)
            };
            let moves = e.moving.then_some(Moves { speed, begin, end });
            add(self.visual(side, &part, begin, moves)?)?;
        }

        if e.moving {
            // The breath of light: its tail, its head, its swell, drawn
            // the way it travels on this edge, head first.
            let whole = tail() + SWELL;
            let head_at = if side.forward() { tail() } else { SWELL };
            let shape = |d: f32| BREATH * breath((d - head_at) * way);
            let part = self.part(e, whole, &shape)?;
            // Where the part's start lies with its head `along` the
            // edge, counted the way the light travels.
            let place = |along: f32| {
                let head = if side.forward() { along } else { long - along };
                (head - head_at) * scale
            };
            for i in 0..LIGHTS {
                let head = e.ring.head + e.ring.len * i as f32 / LIGHTS as f32;
                let now = lap_from(head, e.ring.start, e.ring.len);
                let moves = Moves {
                    speed: SPEED * way * scale,
                    begin: place(-SWELL),
                    end: place(e.ring.len - SWELL),
                };
                add(self.visual(side, &part, place(now), Some(moves))?)?;
            }
        }
        target.SetRoot(&root)?;
        visuals.push(root);
        Ok((target, visuals, fade))
    }
}

impl Frame {
    /// The light round every screen there is now, in `tint`, not yet
    /// shown: [`Frame::show`] fades it in. With `moving` false it stands
    /// still. None when nothing here can draw it; control goes on without
    /// it, with the plate and the mark to show it.
    pub fn new(gpu: &render::Gpu, tint: Color, moving: bool) -> Option<Frame> {
        let gfx = unsafe { Gfx::new(gpu) }
            .map_err(|e| eprintln!("horadric: no light round the screens: {e}"))
            .ok()?;
        let mut strips = Vec::new();
        for monitor in super::monitors() {
            let Some(([l, t, r, b], _)) = super::monitor_rects(monitor) else {
                continue;
            };
            let scale = super::monitor_scale(monitor);
            let depth = ((DEPTH * scale).ceil() as i32)
                .min((r - l) / 2)
                .min((b - t) / 2);
            if depth <= 0 {
                continue;
            }
            let (w, h) = ((r - l) as f32 / scale, (b - t) as f32 / scale);
            let seed = particles::seed_of(&format!("{:?}", [l, t, r, b]));
            // Where on the ring this screen's lights are now, so two
            // screens are not in step.
            let head = (seed % 4096) as f32;
            // The sides run the screen's whole height, under the top and
            // the bottom in the corners: there two lights meet.
            let edges = [
                (Side::Top, (l, t), (r - l, depth), 0.0),
                (Side::Right, (r - depth, t), (depth, b - t), w),
                (Side::Bottom, (l, b - depth), (r - l, depth), w + h),
                (Side::Left, (l, t), (depth, b - t), 2.0 * w + h),
            ];
            for (side, at, size, start) in edges {
                let edge = Edge {
                    side,
                    long: if side.across() { w } else { h },
                    scale,
                    tint,
                    moving,
                    ring: Ring {
                        len: 2.0 * (w + h),
                        start,
                        head,
                    },
                };
                match unsafe { gfx.strip(at, size, &edge) } {
                    Ok(strip) => strips.push(strip),
                    Err(e) => eprintln!("horadric: no light on one edge of a screen: {e}"),
                }
            }
        }
        unsafe {
            let _ = gfx.comp.Commit();
            for s in &strips {
                let _ = ShowWindow(s.hwnd, SW_SHOWNOACTIVATE);
            }
        }
        Some(Frame {
            gfx,
            strips,
            tint,
            moving,
            shown: None,
        })
    }

    /// Shows all of it `shown` strong, 0 to 1. Nothing when it shows so
    /// already, which is every look but the ones of a fade.
    pub fn show(&mut self, shown: f32) {
        if self.shown == Some(shown) {
            return;
        }
        self.shown = Some(shown);
        unsafe {
            for s in &self.strips {
                let _ = s.fade.SetOpacity2(shown.clamp(0.0, 1.0));
            }
            let _ = self.gfx.comp.Commit();
        }
    }
}
