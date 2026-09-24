//! What a dragged window snaps to: the work area of the monitor under the
//! cursor and every other Horadric window, clusters and terminals alike. The
//! geometry itself is `layout::snap` and `layout::snap_edges`.

use std::ffi::c_void;

use windows::core::{Result, PCWSTR};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetCursorPos, GetWindowRect, IsIconic, IsWindowVisible,
};

use crate::app::{GAP_DIP, MARGIN_DIP};
use crate::layout::Spacing;

/// How close a dragged edge comes to a line before it snaps.
const SNAP_DIP: i32 = 10;

pub type Edges = (i32, i32, i32, i32);

/// The work area under the cursor and the spacing at `dpi`, or nothing
/// when Shift is held, which places a window freely. Shift rather than
/// Alt, because a lone Alt press and release opens the menu bar of
/// whichever app has the focus.
pub fn frame(dpi: u32) -> Option<(Edges, Spacing)> {
    if unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0 {
        return None;
    }
    let mut cursor = POINT::default();
    unsafe { GetCursorPos(&mut cursor) }.ok()?;
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let found = unsafe {
        GetMonitorInfoW(
            MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST),
            &mut info,
        )
    };
    if !found.as_bool() {
        return None;
    }
    let w = info.rcWork;
    let s = dpi.max(96) as f32 / 96.0;
    let px = |dip: i32| (dip as f32 * s).round() as i32;
    let spacing = Spacing {
        margin: px(MARGIN_DIP),
        gap: px(GAP_DIP),
        reach: px(SNAP_DIP),
    };
    Some(((w.left, w.top, w.right, w.bottom), spacing))
}

/// The edges you can see of every other visible Horadric window.
pub fn others(except: HWND) -> Vec<Edges> {
    let mut out = Vec::new();
    for class in [
        crate::window::CLASS,
        crate::usage::CLASS,
        crate::start::CLASS,
        crate::terminal::CLASS,
    ] {
        let mut after = None;
        while let Ok(hwnd) = unsafe { FindWindowExW(None, after, class, PCWSTR::null()) } {
            after = Some(hwnd);
            let shown = unsafe { IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() };
            if hwnd == except || !shown {
                continue;
            }
            if let Ok(r) = visible_rect(hwnd) {
                out.push((r.left, r.top, r.right, r.bottom));
            }
        }
    }
    out
}

/// The window's edges as drawn. A Windows 11 window with a frame has an
/// invisible resize border outside them; a popup has none and this is its
/// window rectangle.
pub fn visible_rect(hwnd: HWND) -> Result<RECT> {
    let mut seen = RECT::default();
    let dwm = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut seen as *mut RECT as *mut c_void,
            std::mem::size_of::<RECT>() as u32,
        )
    };
    if dwm.is_ok() {
        return Ok(seen);
    }
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut r) }?;
    Ok(r)
}

/// How far the invisible border reaches past each visible edge, as left,
/// top, right, bottom, all positive.
pub fn border(hwnd: HWND) -> [i32; 4] {
    let mut outer = RECT::default();
    let (Ok(seen), Ok(())) = (visible_rect(hwnd), unsafe {
        GetWindowRect(hwnd, &mut outer)
    }) else {
        return [0; 4];
    };
    [
        seen.left - outer.left,
        seen.top - outer.top,
        outer.right - seen.right,
        outer.bottom - seen.bottom,
    ]
}
