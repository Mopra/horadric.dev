//! A project's browser popped out of the stage into a window of its own.
//!
//! The stage shows one project at a time, so a page beside it went away
//! with every project switch, and could never stand on another screen. A
//! popped out browser is a normal window: moved anywhere, sized, in the
//! taskbar and alt-tab, and still there whichever project the stage shows.
//!
//! It holds the same [`Pane`] the stage would, so the address bar, the tabs
//! and the agents' calls all work as they do there. The page is not made
//! again: [`web`] lends the project's WebViews to whichever pane shows them,
//! and moving them is a `SetParentWindow`, so a page keeps its state and its
//! login.
//!
//! Closing the window puts the browser back with its project on the stage,
//! which is not the same as closing the browser: the cross in its header
//! still does that.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::Arc;

use windows::core::{w, Result, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos, GetWindowLongPtrW,
    GetWindowRect, IsIconic, IsZoomed, LoadCursorW, LoadIconW, PostMessageW, RegisterClassW,
    SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow,
    CREATESTRUCTW, CW_USEDEFAULT, GWLP_USERDATA, HTCAPTION, IDC_ARROW, SWP_NOACTIVATE,
    SWP_NOZORDER, SW_RESTORE, SW_SHOWNORMAL, WINDOW_EX_STYLE, WM_CLOSE, WM_DPICHANGED,
    WM_ERASEBKGND, WM_MOVE, WM_NCCREATE, WM_NCDESTROY, WM_NCLBUTTONDOWN, WM_PAINT, WM_SETFOCUS,
    WM_SIZE, WNDCLASSW, WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW,
};

use crate::app::{self, Input};
use crate::console::Console;
use crate::pane::{self, Pane};
use crate::project::project_name;
use crate::window::Shared;
use crate::{backdrop, theme, web};

const CLASS: PCWSTR = w!("HoradricBrowser");

/// Round the pane, as between the stage's edge and its panes, in DIPs.
const GAP_DIP: f32 = 8.0;

pub fn register_class() -> Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            // Resource 1 is the app icon, so it shows as Horadric.
            hIcon: LoadIconW(
                Some(instance.into()),
                PCWSTR(std::ptr::without_provenance(1)),
            )
            .unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        Ok(())
    }
}

pub struct WebWindow {
    pub hwnd: HWND,
    key: String,
    /// None only while the window is being made. Boxed: its window
    /// procedure holds a pointer to it.
    pane: RefCell<Option<Box<Pane>>>,
    /// Where it was last left unminimized and unmaximized, to save.
    rect: Cell<Option<[i32; 4]>>,
}

impl WebWindow {
    /// Opens the project's browser in a window of its own at `rect`, a
    /// window rectangle in physical pixels, or where Windows likes, and
    /// brings it to the front with the page's keyboard.
    pub fn open(
        shared: Rc<Shared>,
        console: Arc<Console>,
        key: &str,
        rect: Option<[i32; 4]>,
    ) -> Result<Box<Self>> {
        let mut win = Box::new(WebWindow {
            hwnd: HWND::default(),
            key: key.to_string(),
            pane: RefCell::new(None),
            rect: Cell::new(None),
        });
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS,
                &HSTRING::from(project_name(key)),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                None,
                None,
                Some(instance.into()),
                Some(&*win as *const WebWindow as *const c_void),
            )?;
            win.hwnd = hwnd;
            let dark = BOOL(1);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark as *const _ as *const c_void,
                std::mem::size_of::<BOOL>() as u32,
            );
            let accent = theme::accent(key);
            backdrop::border(hwnd, Some(theme::window_bg().mix(accent, 0.35)));
            if let Some([l, t, r, b]) = rect {
                let _ = SetWindowPos(hwnd, None, l, t, r - l, b - t, SWP_NOZORDER);
            }
            // Made once the window has its size, so the page is not laid
            // out at nothing first.
            let pane = Pane::create(shared, console, project_name(key), hwnd)?;
            pane.set_accent(accent);
            *win.pane.borrow_mut() = Some(pane);
            win.layout();
            win.retitle();
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(hwnd);
        }
        win.focus();
        Ok(win)
    }

    /// Brings it to the front, restored, with the keyboard in its page.
    pub fn front(&self) {
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(self.hwnd);
        }
        self.focus();
    }

    fn focus(&self) {
        if let Some(p) = self.pane.borrow().as_ref() {
            p.focus();
        }
    }

    /// Where it stands, as left, top, right, bottom in physical pixels,
    /// as last left unminimized and unmaximized.
    pub fn rect(&self) -> Option<[i32; 4]> {
        self.remember();
        self.rect.get()
    }

    fn remember(&self) {
        unsafe {
            if IsIconic(self.hwnd).as_bool() || IsZoomed(self.hwnd).as_bool() {
                return;
            }
            let mut r = RECT::default();
            if GetWindowRect(self.hwnd, &mut r).is_ok() {
                self.rect.set(Some([r.left, r.top, r.right, r.bottom]));
            }
        }
    }

    /// Takes the pane out first: the page's own window goes back to
    /// [`web`] before this one goes.
    pub fn destroy(&self) {
        self.remember();
        // Out of the cell before it goes: its going sends this window
        // messages, whose handlers read the cell.
        let pane = self.pane.borrow_mut().take();
        if let Some(p) = pane {
            p.destroy();
        }
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    fn gap(&self) -> i32 {
        let dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(96);
        (GAP_DIP * dpi as f32 / 96.0).round() as i32
    }

    fn layout(&self) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        let g = self.gap();
        if let Some(p) = self.pane.borrow().as_ref() {
            p.set_size((r.right - 2 * g).max(0), (r.bottom - 2 * g).max(0));
            p.move_to(g, g);
        }
    }

    /// The window is called what its page is, then its project, as the
    /// taskbar and alt-tab show it.
    fn retitle(&self) {
        let project = project_name(&self.key);
        let title = match web::label(&self.key) {
            Some((t, _)) if !t.is_empty() => format!("{t} - {project}"),
            _ => project,
        };
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &HSTRING::from(title));
        }
    }

    fn paint(&self) {
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        let c = theme::plate_top();
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(self.hwnd, &mut ps);
            let brush = CreateSolidBrush(COLORREF(byte(c.r) | byte(c.g) << 8 | byte(c.b) << 16));
            FillRect(hdc, &ps.rcPaint, brush);
            let _ = DeleteObject(brush.into());
            let _ = EndPaint(self.hwnd, &ps);
        }
    }

    fn handle(&self, msg: u32, _wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_SIZE => {
                self.layout();
                Some(LRESULT(0))
            }
            // A child window gets no WM_MOVE when its top level moves, and
            // WebView2 draws where it last knew its window was.
            WM_MOVE => {
                if let Some(p) = self.pane.borrow().as_ref() {
                    p.moved();
                }
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                let s = unsafe { *(lparam.0 as *const RECT) };
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        s.left,
                        s.top,
                        s.right - s.left,
                        s.bottom - s.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                Some(LRESULT(0))
            }
            WM_SETFOCUS => {
                self.focus();
                Some(LRESULT(0))
            }
            WM_PAINT => {
                self.paint();
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            web::WM_WEB_CHANGED => {
                self.retitle();
                Some(LRESULT(0))
            }
            // The pane's header, between its buttons, moves the window as a
            // title bar does. Posted: the move's loop is not to run inside
            // the pane's click.
            pane::WM_PANE_GRAB => {
                let mut at = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut at);
                    let _ = ReleaseCapture();
                    let packed = (at.x as u16 as u32 | (at.y as u16 as u32) << 16) as isize;
                    let _ = PostMessageW(
                        Some(self.hwnd),
                        WM_NCLBUTTONDOWN,
                        WPARAM(HTCAPTION as usize),
                        LPARAM(packed),
                    );
                }
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                app::push(Input::PopWeb(self.key.clone(), false));
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WebWindow;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The app owns the Box and destroys the window before dropping it.
    let win = &*ptr;
    match win.handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
