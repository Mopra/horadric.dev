//! The gaps between the windows in the columns of tiles.
//!
//! Each cluster is a window of its own, so the gaps between them belong to
//! whatever lies behind: a wheel turned while the cursor crosses one
//! scrolls that instead of the column, and a click there lands on it. One
//! window, shaped to just the gaps, catches the cursor there. It is
//! layered at the least alpha that still takes the mouse, so it cannot be
//! seen, and it stands just below the tiles. Its wheel scrolls the column
//! under the cursor and its clicks do nothing.

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CombineRgn, CreateRectRgn, DeleteObject, SetWindowRgn, ValidateRect, RGN_OR,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, LoadCursorW, RegisterClassW, SetLayeredWindowAttributes,
    SetWindowPos, ShowWindow, HWND_NOTOPMOST, HWND_TOPMOST, IDC_ARROW, LWA_ALPHA, MA_NOACTIVATE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_SHOWNOACTIVATE,
    WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_MOUSEWHEEL, WM_PAINT, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::app::{self, Input};

pub(crate) const CLASS: PCWSTR = w!("HoradricGaps");

pub fn register_class() -> Result<()> {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: GetModuleHandleW(None)?.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        Ok(())
    }
}

/// Makes the window, hidden until [`cover`] gives it gaps to stand in.
pub fn create() -> Result<HWND> {
    unsafe {
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
            CLASS,
            w!("Horadric gaps"),
            WS_POPUP,
            -10_000,
            -10_000,
            1,
            1,
            None,
            None,
            Some(GetModuleHandleW(None)?.into()),
            None,
        )?;
        // At alpha 0 the mouse goes through it, as if it were not there.
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA)?;
        Ok(hwnd)
    }
}

/// Shapes the window to these bands on screen and shows it, or hides it
/// when there are none or `shown` is false.
pub fn cover(hwnd: HWND, bands: &[RECT], shown: bool) {
    let Some(outer) = bands.iter().copied().reduce(|a, b| RECT {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    }) else {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        return;
    };
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            outer.left,
            outer.top,
            outer.right - outer.left,
            outer.bottom - outer.top,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
        let region = CreateRectRgn(0, 0, 0, 0);
        for b in bands {
            let band = CreateRectRgn(
                b.left - outer.left,
                b.top - outer.top,
                b.right - outer.left,
                b.bottom - outer.top,
            );
            CombineRgn(Some(region), Some(region), Some(band), RGN_OR);
            let _ = DeleteObject(band.into());
        }
        // The window owns the region once it is set.
        SetWindowRgn(hwnd, Some(region), true);
        let _ = ShowWindow(hwnd, if shown { SW_SHOWNOACTIVATE } else { SW_HIDE });
    }
}

/// Above other windows without activating, as a cluster does it. Raised
/// before the tiles, so they all come above it again.
pub fn raise(hwnd: HWND) {
    for after in [HWND_TOPMOST, HWND_NOTOPMOST] {
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(after),
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
            );
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEWHEEL => {
            // Screen coordinates, unlike every other mouse message.
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let notches = ((wparam.0 >> 16) & 0xffff) as i16 as i32 / 120;
            app::push(Input::ScrollAt(x, notches));
            LRESULT(0)
        }
        // A click in a gap must not bring the window forward, nor leave
        // the terminal that had the focus.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let _ = ValidateRect(Some(hwnd), None);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
