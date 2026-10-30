//! Behind the conversation: runes drifting up in the project's colour, at
//! three depths.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

use super::view::{Gfx, Palette, View};
use crate::folio_look as look;
use crate::sparks::RUNES;
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
