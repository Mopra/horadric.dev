//! Windows 11 materials behind a window: acrylic for the clusters, Mica for
//! the stage.
//!
//! DWM draws the material and our pixels go over it, so whatever we leave
//! transparent shows the blurred desktop. That needs the frame extended over
//! the whole client area, which makes DWM honour the alpha we draw with.
//! Older builds refuse the attribute, and then the window stays opaque.

use std::ffi::c_void;

use windows::core::{BOOL, HRESULT};
use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMSBT_MAINWINDOW, DWMSBT_TRANSIENTWINDOW, DWMWA_BORDER_COLOR,
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWM_SYSTEMBACKDROP_TYPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

use crate::theme::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Material {
    /// Blurred, for small windows that float over others: the clusters.
    Acrylic,
    /// The wallpaper, faintly, for the window you work in: the stage.
    Mica,
}

/// The windows crate only has `MARGINS` behind `Win32_UI_Controls`, a large
/// feature to turn on for one struct and one call.
#[repr(C)]
struct Margins {
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
}

#[link(name = "dwmapi", kind = "raw-dylib")]
extern "system" {
    fn DwmExtendFrameIntoClientArea(hwnd: HWND, margins: *const Margins) -> HRESULT;
}

/// Puts `material` behind the window in dark mode. False when this Windows
/// has no system backdrops, in which case the window must draw opaque.
pub fn apply(hwnd: HWND, material: Material) -> bool {
    let kind: DWM_SYSTEMBACKDROP_TYPE = match material {
        Material::Acrylic => DWMSBT_TRANSIENTWINDOW,
        Material::Mica => DWMSBT_MAINWINDOW,
    };
    unsafe {
        let dark = BOOL(1);
        let _ = set(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE.0, &dark);
        if set(hwnd, DWMWA_SYSTEMBACKDROP_TYPE.0, &kind).is_err() {
            return false;
        }
        let all = Margins {
            left: -1,
            right: -1,
            top: -1,
            bottom: -1,
        };
        DwmExtendFrameIntoClientArea(hwnd, &all).is_ok()
    }
}

/// The one pixel edge Windows 11 draws around a window, in `c`, or none.
pub fn border(hwnd: HWND, c: Option<Color>) {
    // DWMWA_COLOR_NONE: no edge at all.
    let value = c.map_or(0xFFFF_FFFE, colorref);
    unsafe {
        let _ = set(hwnd, DWMWA_BORDER_COLOR.0, &COLORREF(value));
    }
}

fn colorref(c: Color) -> u32 {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    byte(c.r) | byte(c.g) << 8 | byte(c.b) << 16
}

unsafe fn set<T>(hwnd: HWND, attribute: i32, value: &T) -> windows::core::Result<()> {
    DwmSetWindowAttribute(
        hwnd,
        windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(attribute),
        value as *const T as *const c_void,
        std::mem::size_of::<T>() as u32,
    )
}

/// Whether Windows wants things to move: Settings, Accessibility, Visual
/// effects, Animation effects. Off, the light holds still.
pub fn animations_on() -> bool {
    let mut on = BOOL(1);
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut on as *mut BOOL as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    on.as_bool()
}
