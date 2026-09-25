//! The Windows folder picker, and a yes or no question.

use std::path::{Path, PathBuf};

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, FOS_FORCEFILESYSTEM,
    FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDNO, IDOK, IDYES, MB_DEFBUTTON1, MB_DEFBUTTON2, MB_ICONQUESTION, MB_ICONWARNING,
    MB_OKCANCEL, MB_SETFOREGROUND, MB_YESNO, MB_YESNOCANCEL,
};

/// Asks for a folder, starting in `start`. None when cancelled. Runs a modal
/// loop, so the caller must not hold anything the message handlers need.
pub fn pick_folder(owner: HWND, start: Option<&Path>) -> Option<PathBuf> {
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog
            .SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
            .ok()?;
        let _ = dialog.SetTitle(w!("Start a Horadric session in"));
        let _ = dialog.SetOkButtonLabel(w!("Start session"));
        if let Some(dir) = start.filter(|d| d.is_dir()) {
            let item: windows::core::Result<IShellItem> =
                SHCreateItemFromParsingName(&HSTRING::from(dir.as_os_str()), None);
            if let Ok(item) = item {
                let _ = dialog.SetFolder(&item);
            }
        }
        // An owner that is not visible is fine: it only makes the dialog
        // modal to it. Cancelling is reported as an error.
        dialog.Show(Some(owner)).ok()?;
        let item = dialog.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(name.0 as *const _));
        path
    }
}

/// Asks a yes, no or cancel question whose yes keeps something running:
/// Some(true) for yes, Some(false) for no, None for cancel. Enter answers
/// yes when `keep_first`, otherwise no.
pub fn keep_or_stop(owner: HWND, text: &str, keep_first: bool) -> Option<bool> {
    let default = if keep_first {
        MB_DEFBUTTON1
    } else {
        MB_DEFBUTTON2
    };
    let answer = unsafe {
        MessageBoxW(
            Some(owner),
            &HSTRING::from(text),
            w!("Horadric"),
            MB_YESNOCANCEL | MB_ICONQUESTION | MB_SETFOREGROUND | default,
        )
    };
    match answer {
        IDYES => Some(true),
        IDNO => Some(false),
        _ => None,
    }
}

/// An OK or Cancel warning. True for OK.
pub fn confirm(owner: HWND, text: &str) -> bool {
    unsafe {
        MessageBoxW(
            Some(owner),
            &HSTRING::from(text),
            w!("Horadric"),
            MB_OKCANCEL | MB_ICONWARNING | MB_SETFOREGROUND,
        ) == IDOK
    }
}

/// A yes or no question. True for yes.
pub fn yes_no(owner: HWND, text: &str) -> bool {
    unsafe {
        MessageBoxW(
            Some(owner),
            &HSTRING::from(text),
            w!("Horadric"),
            MB_YESNO | MB_ICONQUESTION | MB_SETFOREGROUND,
        ) == IDYES
    }
}
