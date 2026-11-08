//! Behind the conversation: runes drifting up in the project's colour, at
//! three depths.

use windows::core::Result;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::{ID2D1RenderTarget, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_ELLIPSE};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

use super::view::{d2d, Gfx, Palette, View};
use crate::render::color;
use crate::folio_look as look;
use crate::layout::Rect;
use crate::light::Bloom;
use crate::sparks::RUNES;
use windows_numerics::Vector2;

/// How bright the working dot `k` of three is at clock `t`, 0 to 1: each a
/// beat behind the one before, so the light runs along them.
pub(super) fn pulse(t: f32, k: usize) -> f32 {
    ((t * 4.0 - k as f32 * 0.7).sin() * 0.5 + 0.5).powf(1.5)
}

/// How much light an ember centred at `x` gives off, 0 under the column
/// to 1 well clear of it: the column is read, so its runes stay quiet and
/// only the margins glow.
fn margin(x: f32, column: (f32, f32)) -> f32 {
    ((look::calm(x, column) - look::CALM) / (1.0 - look::CALM)).clamp(0.0, 1.0)
}

/// How bright an ember's light is beside the rune itself: faint, so the
/// margins shimmer and nothing washes.
const EMBER_LIGHT: f32 = 0.45;
impl View {
    /// Runes drifting up behind the conversation, farthest first so the
    /// near ones pass in front. While Claude works more join, they burn
    /// brighter and take more of the project's colour, and the near ones
    /// rush past while the far ones barely stir, so the turn feels like
    /// moving forward. No glow under them: a faint wash that wide bands
    /// into rings on a dark background, and the runes say it better.
    pub(super) unsafe fn background(
        &self,
        gfx: &Gfx,
        pal: &Palette,
        view: (f32, f32),
        clocks: (f32, f32),
        heat: f32,
    ) {
        let ink = pal.fg.mix(pal.accent, 0.35 + 0.5 * heat);
        let column = look::column(view.0);
        let share = self.share_of_screen();
        for (k, l) in look::LAYERS.iter().enumerate() {
            for i in look::shown(l, share) {
                let e = look::ember(k, i, clocks, view, heat, RUNES.len());
                let alpha = e.alpha * look::calm(e.x + e.size / 2.0, column);
                if alpha <= 0.004 {
                    continue;
                }
                let c = ink.mix(pal.bg, e.sunk).with_alpha(alpha);
                gfx.rune(e.rune, e.x, e.y, e.size, c, e.stroke);
            }
        }
    }

    /// What gives off light this frame, sharp into the light layer and
    /// bloomed over the window, which is drawing: the embers in the margins
    /// and the working dots. `calm` is false while a page burns away or
    /// opens, when the dots are not shown. Nothing when nothing is lit.
    pub(super) unsafe fn shine(
        &self,
        gfx: &Gfx,
        pal: &Palette,
        view: (f32, f32),
        clocks: (f32, f32),
        heat: f32,
        calm: bool,
    ) -> Result<()> {
        let ink = pal.fg.mix(pal.accent, 0.35 + 0.5 * heat);
        let column = look::column(view.0);
        let share = self.share_of_screen();
        let mut embers = Vec::new();
        for (k, l) in look::LAYERS.iter().enumerate() {
            for i in look::shown(l, share) {
                let e = look::ember(k, i, clocks, view, heat, RUNES.len());
                let lit = e.alpha * margin(e.x + e.size / 2.0, column) * EMBER_LIGHT;
                if lit > 0.01 {
                    embers.push((e, ink.mix(pal.bg, e.sunk).with_alpha(lit)));
                }
            }
        }
        let dots = self.dots.get().filter(|_| calm);
        if embers.is_empty() && dots.is_none() {
            return Ok(());
        }
        let layer = gfx.light.begin(&gfx.rt)?;
        let to: &ID2D1RenderTarget = &layer;
        for (e, c) in &embers {
            gfx.rune_on(to, e.rune, (e.x, e.y), e.size, *c, e.stroke);
            gfx.light.lit(Rect::new(e.x - e.size / 2.0, e.y, e.size * 2.0, e.size));
        }
        if let Some((at, area)) = dots {
            to.PushAxisAlignedClip(&d2d(&area), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            let hot = pal.accent.mix(pal.fg, 0.3);
            for k in 0..3 {
                gfx.brush.SetColor(&color(hot.with_alpha(0.5 * pulse(clocks.0, k))));
                let dot = D2D1_ELLIPSE {
                    point: Vector2 {
                        X: at.X + k as f32 * 12.0,
                        Y: at.Y,
                    },
                    radiusX: 2.4,
                    radiusY: 2.4,
                };
                to.FillEllipse(&dot, &gfx.brush);
            }
            to.PopAxisAlignedClip();
            gfx.light.lit(Rect::new(at.X - 4.0, at.Y - 4.0, 32.0, 8.0));
        }
        gfx.light.present(&gfx.rt, &Bloom::pane())
    }

    /// How much of its monitor's work area the pane covers. Both in
    /// pixels, so the screen's scale cancels out.
    unsafe fn share_of_screen(&self) -> f32 {
        let hwnd = self.hwnd();
        let mut pane = RECT::default();
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if GetClientRect(hwnd, &mut pane).is_err() || !GetMonitorInfoW(monitor, &mut info).as_bool()
        {
            return 1.0;
        }
        let area =
            |r: RECT| ((r.right - r.left).max(0) as f32) * ((r.bottom - r.top).max(0) as f32);
        let screen = area(info.rcWork);
        if screen <= 0.0 {
            return 1.0;
        }
        area(pane) / screen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embers_give_off_light_only_clear_of_the_column() {
        let column = (300.0, 400.0);
        assert_eq!(margin(500.0, column), 0.0);
        assert_eq!(margin(300.0, column), 0.0);
        assert_eq!(margin(40.0, column), 1.0);
        assert_eq!(margin(960.0, column), 1.0);
        let near = margin(280.0, column);
        assert!(near > 0.0 && near < 1.0, "{near}");
    }

    #[test]
    fn the_working_dots_take_turns() {
        assert!((0.0..=1.0).contains(&pulse(0.3, 0)));
        assert_ne!(pulse(0.3, 0), pulse(0.3, 1));
    }
}
