//! Start with Windows: a value under the user's `Run` key that starts
//! `horadricw.exe`, which brings the app up hidden in the tray with the saved
//! sessions as paused tiles.

use std::path::{Path, PathBuf};

use windows::core::HSTRING;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const NAME: &str = "Horadric";

/// `horadricw.exe` beside this binary. None when it is not there, since
/// starting `horadric.exe` at login would open a console window.
fn beside_me() -> Option<PathBuf> {
    let horadricw = std::env::current_exe()
        .ok()?
        .with_file_name("horadricw.exe");
    horadricw.is_file().then_some(horadricw)
}

pub fn is_enabled() -> bool {
    let mut size = 0u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN),
            &HSTRING::from(NAME),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        ) == ERROR_SUCCESS
    }
}

/// Starts the `horadricw.exe` beside this binary at login. True when it worked.
pub fn enable() -> bool {
    beside_me().is_some_and(|g| enable_at(&g))
}

/// Starts this `horadricw.exe` at login. `horadric install` uses it to point at
/// the installed copy rather than the one doing the installing.
pub fn enable_at(horadricw: &Path) -> bool {
    let cmd = format!("\"{}\"", horadricw.display());
    let data: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN),
            &HSTRING::from(NAME),
            REG_SZ.0,
            Some(data.as_ptr() as *const _),
            (data.len() * 2) as u32,
        ) == ERROR_SUCCESS
    }
}

pub fn disable() {
    unsafe {
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(RUN), &HSTRING::from(NAME));
    }
}
