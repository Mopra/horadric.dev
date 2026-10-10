//! The pages a browser pane's address field suggests, dropped under it as
//! the address is typed, and the latest sites while it is empty.
//!
//! Unlike a setting's list it never takes the focus or the mouse: the
//! field keeps the keyboard, so typing goes on while it is open, and the
//! pane moves the choice with the arrow keys. A click on a row tells the
//! pane, which goes there.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{InvalidateRect, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};

use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW, LoadCursorW,
    PostMessageW, RegisterClassW, SetWindowLongPtrW, SetWindowPos, ShowWindow, CREATESTRUCTW,
    GWLP_USERDATA, IDC_ARROW, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOWNOACTIVATE,
    WM_ERASEBKGND, WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY,
    WM_PAINT, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::backdrop;
use crate::layout::{self, SuggestLayout};
use crate::render::{SuggestScene, Target};
use crate::window::Shared;

pub(crate) const CLASS: PCWSTR = w!("HoradricSuggest");

/// Posted to the pane whose field it hangs from when a row is clicked,
/// the row in `wparam`.
pub const WM_SUGGEST_PICK: u32 = windows::Win32::UI::WindowsAndMessaging::WM_USER + 20;

/// The most pages it lists.
pub const MAX_ROWS: usize = 8;

const WM_MOUSELEAVE: u32 = 0x02A3;

pub struct Suggest {
    hwnd: HWND,
    /// The pane whose address field it hangs from, told of a click.
    pane: HWND,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    layout: RefCell<SuggestLayout>,
    rows: RefCell<Vec<(String, String)>>,
    chosen: Cell<Option<usize>>,
    hot: Cell<Option<usize>>,
    /// Where it hangs: under the field, in screen pixels, its left edge,
    /// top and width.
    at: Cell<(i32, i32, i32)>,
}

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

impl Suggest {
    /// Opens the list of `rows` under the field of `pane`, at `(x, y)` on
    /// screen and `width` pixels wide. None when there is nothing to list.
    pub fn open(
        shared: Rc<Shared>,
        pane: HWND,
        rows: Vec<(String, String)>,
        (x, y, width): (i32, i32, i32),
    ) -> Option<Box<Self>> {
        if rows.is_empty() {
            return None;
        }
        let dpi = unsafe { GetDpiForWindow(pane) }.max(96);
        let s = dpi as f32 / 96.0;
        let layout = layout::suggest(&shared.metrics, width as f32 / s, rows.len());
        let h = (layout.size.1 * s).round() as i32;
        let mut win = Box::new(Suggest {
            hwnd: HWND::default(),
            pane,
            shared,
            target: RefCell::new(None),
            layout: RefCell::new(layout),
            rows: RefCell::new(rows),
            chosen: Cell::new(None),
            hot: Cell::new(None),
            at: Cell::new((x, y, width)),
        });
        unsafe {
            let owner = windows::Win32::UI::WindowsAndMessaging::GetAncestor(
                pane,
                windows::Win32::UI::WindowsAndMessaging::GA_ROOT,
            );
            // Born where it shows and at its size: a window that starts
            // elsewhere and moves has been seen not to paint.
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS,
                w!("Horadric suggestions"),
                WS_POPUP,
                x,
                y,
                width,
                h,
                Some(owner),
                None,
                Some(GetModuleHandleW(None).ok()?.into()),
                Some(&*win as *const Suggest as *const c_void),
            )
            .ok()?;
            win.hwnd = hwnd;
            let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            backdrop::border(hwnd, None);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        Some(win)
    }

    /// Lists `rows` instead, none chosen. False when there are none, so
    /// the caller closes it.
    pub fn set_rows(&self, rows: Vec<(String, String)>) -> bool {
        if rows.is_empty() {
            return false;
        }
        if *self.rows.borrow() == rows {
            return true;
        }
        let s = self.scale();
        let (x, y, width) = self.at.get();
        let layout = layout::suggest(&self.shared.metrics, width as f32 / s, rows.len());
        let h = (layout.size.1 * s).round() as i32;
        *self.layout.borrow_mut() = layout;
        *self.rows.borrow_mut() = rows;
        self.chosen.set(None);
        self.hot.set(None);
        // A target is made at its window's size.
        *self.target.borrow_mut() = None;
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                x,
                y,
                width,
                h,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
        self.invalidate();
        true
    }

    /// Moves the choice `by` rows, from none to the first going down and
    /// to the last going up, and off the list past either end. Says the
    /// address chosen, or None when the choice left the list.
    pub fn step(&self, by: i32) -> Option<String> {
        let n = self.rows.borrow().len() as i32;
        let next = match self.chosen.get() {
            None if by > 0 => 0,
            None => n - 1,
            Some(i) => i as i32 + by,
        };
        let chosen = (0..n).contains(&next).then_some(next as usize);
        self.chosen.set(chosen);
        self.invalidate();
        chosen.and_then(|i| self.url(i))
    }

    /// The row the arrow keys are on.
    pub fn chosen(&self) -> Option<usize> {
        self.chosen.get()
    }

    /// The address of row `i`.
    pub fn url(&self, i: usize) -> Option<String> {
        self.rows.borrow().get(i).map(|(_, u)| u.clone())
    }

    fn scale(&self) -> f32 {
        unsafe { GetDpiForWindow(self.hwnd) }.max(96) as f32 / 96.0
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn paint(&self) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        let dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(96);
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            let (w, h) = (r.right as u32, r.bottom as u32);
            match Target::new(&self.shared.gpu, self.hwnd, w, h, dpi) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for suggestions: {e}");
                    return;
                }
            }
        }
        let layout = self.layout.borrow();
        let rows = self.rows.borrow();
        let scene = SuggestScene {
            layout: &layout,
            rows: &rows,
            chosen: self.chosen.get(),
            hot: self.hot.get(),
        };
        let result = slot
            .as_ref()
            .map(|t| t.draw_suggest(&self.shared.gpu, &self.shared.metrics, &scene));
        if let Some(Err(_)) = result {
            *slot = None;
        }
    }

    fn hit(&self, lparam: LPARAM) -> Option<usize> {
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32 / s;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
        layout::suggest_hit(&self.layout.borrow(), x, y)
    }

    fn set_hot(&self, hot: Option<usize>) {
        if self.hot.replace(hot) != hot {
            self.invalidate();
        }
    }

    fn handle(&self, msg: u32, _wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                self.paint();
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd), None);
                }
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            // The field keeps the keyboard through a click on a row.
            WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
            WM_MOUSEMOVE => {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: self.hwnd,
                    dwHoverTime: 0,
                };
                unsafe {
                    let _ = TrackMouseEvent(&mut track);
                }
                self.set_hot(self.hit(lparam));
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.set_hot(None);
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                if let Some(i) = self.hit(lparam) {
                    unsafe {
                        let _ =
                            PostMessageW(Some(self.pane), WM_SUGGEST_PICK, WPARAM(i), LPARAM(0));
                    }
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

/// The window goes with its Box, whichever of the field or the pane ends
/// first.
impl Drop for Suggest {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Suggest;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The Box destroys the window as it is dropped, so it outlives it.
    let win = &*ptr;
    match win.handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
