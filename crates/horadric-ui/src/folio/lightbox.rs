//! An image from the conversation or the composer, opened over the glass.
//! A click on a thumbnail grows it from where it sat to as big as fits,
//! the glass dimmed behind. The wheel zooms about the pointer, a drag
//! pans, a click on the image goes to its own pixels and back, and Esc or
//! a click beside it shrinks it home.
//!
//! It is drawn on the window over the kept layer, so zooming and panning
//! run on the frame clock without laying the conversation out again.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap, ID2D1DeviceContext, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetCursorPos, IDC_ARROW, IDC_HAND, IDC_SIZEALL,
};

use super::rich;
use super::view::{d2d, view_of, Gfx, Palette, View};
use crate::layout::Rect;
use crate::motion;

/// The longest side kept, in pixels. Bigger is scaled down once, on open.
const MAX_PX: u32 = 8192;
/// Between the image as big as fits and the glass's edges, in DIPs.
const MARGIN: f32 = 28.0;
/// Under the image, for its size.
const CAPTION: f32 = 22.0;
const GROW: Duration = Duration::from_millis(240);
const SHRINK: Duration = Duration::from_millis(190);
/// How fast a zoom catches up with the wheel, per second.
const FOLLOW: f32 = 18.0;
/// The most DIPs one of the image's pixels may take.
const MOST: f32 = 16.0;
/// A press that moves less than this is a click, not a drag.
const SLOP: f32 = 3.0;

struct Lightbox {
    pixels: Vec<u8>,
    w: u32,
    h: u32,
    /// Made on the window's target, which is named so a new one makes it
    /// again.
    bitmap: Option<(ID2D1Bitmap, usize)>,
    /// The thumbnail it grew from, and shrinks back to.
    from: Option<Rect>,
    opened: Instant,
    closing: Option<Instant>,
    /// DIPs per pixel aimed at, None for as big as fits.
    zoom: Option<f32>,
    /// The image's middle from the glass's, aimed at.
    pan: (f32, f32),
    /// Zoom and pan as last drawn, catching up with those aimed at.
    shown: (f32, f32, f32),
    last: Option<Instant>,
    /// The glass in DIPs as last drawn.
    size: (f32, f32),
    /// Where a press on the image began, the pan then, and whether it has
    /// moved past a click.
    press: Option<((f32, f32), (f32, f32), bool)>,
    /// Owes a frame though nothing animates: a drag moved it.
    dirty: bool,
}

thread_local! {
    static OPEN: RefCell<HashMap<String, Lightbox>> = RefCell::new(HashMap::new());
}

fn with<R>(key: &str, f: impl FnOnce(&mut Lightbox) -> R) -> Option<R> {
    OPEN.with(|o| o.borrow_mut().get_mut(key).map(f))
}

/// Open and not on its way out: the mouse, the wheel and Esc are its.
fn live(key: &str) -> bool {
    with(key, |l| l.closing.is_none()).unwrap_or(false)
}

impl Lightbox {
    /// DIPs per pixel at which it is as big as fits, never blown up past
    /// one DIP a pixel.
    fn fit(&self) -> f32 {
        let (aw, ah) = self.room();
        (aw / self.w as f32).min(ah / self.h as f32).min(1.0)
    }

    fn room(&self) -> (f32, f32) {
        let (w, h) = self.size;
        ((w - 2.0 * MARGIN).max(1.0), (h - 2.0 * MARGIN - CAPTION).max(1.0))
    }

    /// Where the image is centred when not panned.
    fn middle(&self) -> (f32, f32) {
        let (w, h) = self.size;
        (w / 2.0, (h - CAPTION) / 2.0)
    }

    fn aim(&self) -> f32 {
        self.zoom.unwrap_or_else(|| self.fit())
    }

    /// A pan that keeps the glass covered where the image is bigger than
    /// it, and the image in the middle where it is not.
    fn clamp(&self, z: f32, (px, py): (f32, f32)) -> (f32, f32) {
        let (w, h) = self.size;
        let mx = ((self.w as f32 * z - w) / 2.0).max(0.0);
        let my = ((self.h as f32 * z - h) / 2.0).max(0.0);
        (px.clamp(-mx, mx), py.clamp(-my, my))
    }

    /// The image as drawn at zoom `z` and pan `p`.
    fn rect(&self, (z, px, py): (f32, f32, f32)) -> Rect {
        let (cx, cy) = self.middle();
        let (dw, dh) = (self.w as f32 * z, self.h as f32 * z);
        Rect::new(cx + px - dw / 2.0, cy + py - dh / 2.0, dw, dh)
    }

    /// Zooms to `z` keeping the pixel under `(x, y)` where it is.
    fn zoom_at(&mut self, z: f32, x: f32, y: f32) {
        let fit = self.fit();
        let z = z.clamp(fit, MOST.max(fit));
        let (cx, cy) = self.middle();
        let was = self.aim();
        let (ux, uy) = ((x - cx - self.pan.0) / was, (y - cy - self.pan.1) / was);
        self.zoom = (z > fit * 1.001).then_some(z);
        self.pan = self.clamp(z, (x - cx - ux * z, y - cy - uy * z));
    }

    fn settled(&self) -> bool {
        let (z, px, py) = self.shown;
        let aim = self.aim();
        (z - aim).abs() < aim * 0.002
            && (px - self.pan.0).abs() < 0.3
            && (py - self.pan.1).abs() < 0.3
    }

    fn moving(&self, now: Instant) -> bool {
        self.dirty
            || now.duration_since(self.opened) < GROW
            || self.closing.is_some()
            || !self.settled()
    }
}

/// Opens image `base64` over session `key`'s glass, grown from the
/// thumbnail under the pointer, which is the one just clicked.
pub(super) fn open(key: &str, base64: &str) {
    let Some(view) = view_of(key) else {
        return;
    };
    let Some((pixels, w, h)) = rich::decode(base64, MAX_PX) else {
        return;
    };
    let (x, y) = pointer(view);
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(view.hwnd(), &mut r);
    }
    let s = view.scale();
    let mut l = Lightbox {
        pixels,
        w,
        h,
        bitmap: None,
        from: view.hit_rect(x, y),
        opened: Instant::now(),
        closing: None,
        zoom: None,
        pan: (0.0, 0.0),
        shown: (0.0, 0.0, 0.0),
        last: None,
        size: (r.right as f32 / s, r.bottom as f32 / s),
        press: None,
        dirty: true,
    };
    l.shown = (l.fit(), 0.0, 0.0);
    OPEN.with(|o| o.borrow_mut().insert(key.to_string(), l));
    view.clock(true);
}

fn close(key: &str) {
    with(key, |l| {
        if l.closing.is_none() {
            l.closing = Some(Instant::now());
        }
    });
    kick(key);
}

fn kick(key: &str) {
    if let Some(v) = view_of(key) {
        v.clock(true);
    }
}

fn pointer(view: &View) -> (f32, f32) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
        let _ = ScreenToClient(view.hwnd(), &mut p);
    }
    let s = view.scale();
    (p.x as f32 / s, p.y as f32 / s)
}

/// Whether the frame clock has to run for it.
pub(super) fn moving(key: &str) -> bool {
    with(key, |l| l.moving(Instant::now())).unwrap_or(false)
}

/// A press on the glass. False when there is no image open to take it.
pub(super) fn press(view: &View, x: f32, y: f32) -> bool {
    let key = &view.key;
    if !live(key) {
        return false;
    }
    let on = with(key, |l| l.rect(l.shown).contains(x, y)).unwrap_or(false);
    if on {
        with(key, |l| l.press = Some(((x, y), l.pan, false)));
        unsafe { SetCapture(view.hwnd()) };
    } else {
        close(key);
    }
    true
}

/// The mouse moved. False when it is not the image's.
pub(super) fn drag(view: &View, x: f32, y: f32) -> bool {
    let key = &view.key;
    if !live(key) {
        return false;
    }
    let moved = with(key, |l| {
        let Some(((x0, y0), pan, far)) = l.press else {
            return false;
        };
        let (dx, dy) = (x - x0, y - y0);
        let far = far || dx.abs().max(dy.abs()) > SLOP;
        l.press = Some(((x0, y0), pan, far));
        if far {
            // The image stays under the hand, not catching up behind it.
            let z = l.aim();
            l.pan = l.clamp(z, (pan.0 + dx, pan.1 + dy));
            l.shown = (z, l.pan.0, l.pan.1);
            l.dirty = true;
        }
        far
    })
    .unwrap_or(false);
    if moved {
        kick(key);
    }
    true
}

/// The button came up. A press that did not drag goes to the image's own
/// pixels about the point, or back to as big as fits.
pub(super) fn release(view: &View, x: f32, y: f32) -> bool {
    let key = &view.key;
    let Some(Some((_, _, far))) = with(key, |l| l.press.take()) else {
        return false;
    };
    unsafe {
        let _ = ReleaseCapture();
    }
    if !far {
        let actual = 1.0 / view.scale();
        with(key, |l| {
            let fit = l.fit();
            if l.zoom.is_some() {
                l.zoom_at(fit, x, y);
            } else {
                // Already at its own pixels or smaller: twice as big.
                l.zoom_at(if fit < actual * 0.99 { actual } else { fit * 2.0 }, x, y);
            }
        });
        kick(key);
    }
    true
}

/// The mouse went elsewhere mid drag.
pub(super) fn lost_capture(key: &str) {
    with(key, |l| l.press = None);
}

/// The wheel, in Windows' units of 120 a notch. False when no image is
/// open.
pub(super) fn wheel(key: &str, delta: i32) -> bool {
    if !live(key) {
        return false;
    }
    let Some(view) = view_of(key) else {
        return false;
    };
    let (x, y) = pointer(view);
    with(key, |l| {
        let z = l.aim() * 1.2f32.powf(delta as f32 / 120.0);
        l.zoom_at(z, x, y);
    });
    kick(key);
    true
}

/// Esc shrinks it home. False for every other key, and when none is open.
pub(super) fn key(key: &str, vk: u16) -> bool {
    if vk != VK_ESCAPE.0 || !live(key) {
        return false;
    }
    close(key);
    true
}

/// The pointer over the glass while an image is open.
pub(super) fn cursor(view: &View, x: f32, y: f32) -> Option<PCWSTR> {
    with(&view.key, |l| {
        if l.closing.is_some() {
            return None;
        }
        if l.press.is_some_and(|p| p.2) {
            return Some(IDC_SIZEALL);
        }
        if !l.rect(l.shown).contains(x, y) {
            return Some(IDC_ARROW);
        }
        Some(if l.zoom.is_some() { IDC_SIZEALL } else { IDC_HAND })
    })
    .flatten()
}

/// Draws it over the frame, moved on to now. `gfx.rt` is the window's.
pub(super) unsafe fn draw(gfx: &Gfx, pal: &Palette, key: &str) {
    let now = Instant::now();
    let gone = with(key, |l| {
        let size = gfx.rt.GetSize();
        l.size = (size.width, size.height);
        let dt = (l.last.replace(now)).map_or(0.0, |t| now.duration_since(t).as_secs_f32().min(0.1));
        l.dirty = false;

        let aim = l.aim();
        let pan = l.clamp(aim, l.pan);
        l.pan = pan;
        let k = 1.0 - (-dt * FOLLOW).exp();
        let (z, px, py) = l.shown;
        l.shown = if l.settled() {
            (aim, pan.0, pan.1)
        } else {
            (z + (aim - z) * k, px + (pan.0 - px) * k, py + (pan.1 - py) * k)
        };

        let grown = motion::ease_out(motion::progress(now.duration_since(l.opened), GROW));
        let shrunk = l.closing.map_or(0.0, |t| {
            motion::ease_out(motion::progress(now.duration_since(t), SHRINK))
        });
        if shrunk >= 1.0 {
            return true;
        }
        let t = grown * (1.0 - shrunk);

        gfx.fill(&Rect::new(0.0, 0.0, size.width, size.height), pal.bg.with_alpha(0.88 * t));

        let to = l.rect(l.shown);
        let at = match l.from {
            Some(f) => lerp(&f, &to, t),
            // Nothing to grow from: it swells a little in place.
            None => {
                let s = 0.94 + 0.06 * t;
                let (cx, cy) = (to.x + to.w / 2.0, to.y + to.h / 2.0);
                Rect::new(cx - to.w * s / 2.0, cy - to.h * s / 2.0, to.w * s, to.h * s)
            }
        };
        let target = gfx.win.as_raw() as usize;
        if l.bitmap.as_ref().is_none_or(|(_, made)| *made != target) {
            l.bitmap = rich::bitmap(&gfx.rt, &l.pixels, l.w, l.h).map(|b| (b, target));
        }
        let opacity = if l.from.is_some() { 1.0 } else { t };
        if let Some((b, _)) = &l.bitmap {
            match gfx.rt.cast::<ID2D1DeviceContext>() {
                // Cubic, since a screenshot shrunk to fit turns to jagged
                // text under linear.
                Ok(dc) => dc.DrawBitmap(
                    b,
                    Some(&d2d(&at)),
                    opacity,
                    D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                    None,
                    None,
                ),
                Err(_) => gfx.rt.DrawBitmap(
                    b,
                    Some(&d2d(&at)),
                    opacity,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                ),
            }
            gfx.stroke_round(&at, 1.0, pal.fg.with_alpha(0.14 * t), 1.0);
        }

        let mut caption = format!("{} \u{00d7} {}", l.w, l.h);
        if let Some(z) = l.zoom {
            let scale = gfx.dpi as f32 / 96.0;
            caption.push_str(&format!("   {:.0}%", z * scale * 100.0));
        }
        // Under the image's left edge, as a figure's caption sits.
        let below = (to.bottom() + 6.0).min(size.height - CAPTION);
        let left = to.x.max(MARGIN);
        gfx.text_in(
            &caption,
            &gfx.small,
            &Rect::new(left, below, (size.width - left).max(0.0), CAPTION - 4.0),
            pal.faint.with_alpha(pal.faint.a * t),
        );
        false
    });
    if gone == Some(true) {
        OPEN.with(|o| o.borrow_mut().remove(key));
    }
}

fn lerp(a: &Rect, b: &Rect, t: f32) -> Rect {
    let m = |p: f32, q: f32| p + (q - p) * t;
    Rect::new(m(a.x, b.x), m(a.y, b.y), m(a.w, b.w), m(a.h, b.h))
}
