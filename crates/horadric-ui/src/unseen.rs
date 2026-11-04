//! Whether nobody can see the screen at all: the session is locked or the
//! display is off.
//!
//! Every moving light asks this before it spends a frame. Light that runs
//! for as long as an agent works would otherwise run all night behind a
//! locked screen.

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::{w, GUID};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, RegisterClassW, WINDOW_EX_STYLE, WM_POWERBROADCAST,
    WNDCLASSW, WS_POPUP,
};

static LOCKED: AtomicBool = AtomicBool::new(false);
static DARK: AtomicBool = AtomicBool::new(false);

const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
const WTS_SESSION_LOCK: usize = 0x7;
const WTS_SESSION_UNLOCK: usize = 0x8;
const PBT_POWERSETTINGCHANGE: usize = 0x8013;
const DEVICE_NOTIFY_WINDOW_HANDLE: u32 = 0;
const GUID_CONSOLE_DISPLAY_STATE: GUID = GUID::from_u128(0x6fe69556_704a_47a0_8f24_c28d936fda47);

#[repr(C)]
struct PowerSetting {
    setting: GUID,
    length: u32,
    data: [u8; 1],
}

// Declared here rather than turning on the `Win32_System_Power` feature of
// the windows crate for one call.
#[link(name = "user32")]
extern "system" {
    fn RegisterPowerSettingNotification(h: HANDLE, guid: *const GUID, flags: u32) -> *mut c_void;
}

/// Whether the screen is locked or dark. The first call, on the UI thread,
/// starts listening, so whoever asks first need not set anything up.
pub fn now() -> bool {
    thread_local! {
        static LISTENING: Cell<bool> = const { Cell::new(false) };
    }
    if !LISTENING.with(|l| l.replace(true)) {
        listen();
    }
    LOCKED.load(Ordering::Relaxed) || DARK.load(Ordering::Relaxed)
}

/// A hidden window of its own, never shown, because the power setting
/// broadcast reaches top level windows and not message-only ones.
fn listen() {
    unsafe {
        let Ok(instance) = GetModuleHandleW(None) else {
            return;
        };
        let class = w!("HoradricUnseen");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        ) else {
            return;
        };
        let _ = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
        // Windows answers at once with the display's state, then on each
        // change.
        RegisterPowerSettingNotification(
            HANDLE(hwnd.0),
            &GUID_CONSOLE_DISPLAY_STATE,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        );
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_WTSSESSION_CHANGE => match wparam.0 {
            WTS_SESSION_LOCK => LOCKED.store(true, Ordering::Relaxed),
            WTS_SESSION_UNLOCK => LOCKED.store(false, Ordering::Relaxed),
            _ => {}
        },
        WM_POWERBROADCAST if wparam.0 == PBT_POWERSETTINGCHANGE && lparam.0 != 0 => {
            let s = &*(lparam.0 as *const PowerSetting);
            if s.setting == GUID_CONSOLE_DISPLAY_STATE && s.length >= 1 {
                // 0 is off, 1 on, 2 dimmed, and a dimmed screen is still seen.
                DARK.store(s.data[0] == 0, Ordering::Relaxed);
            }
            return LRESULT(1);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}
