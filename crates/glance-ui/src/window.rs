//! The cluster window: a frameless, rounded, always-on-top Win32 window that
//! never takes focus.
//!
//! The rules that make it feel like part of Windows rather than an app:
//! `WS_EX_NOACTIVATE` so clicking it does not steal focus from the editor,
//! `WS_EX_TOOLWINDOW` so it stays out of alt-tab and the taskbar, DWM
//! rounded corners so it matches Windows 11, and every move or resize done
//! with `SWP_NOACTIVATE`. Dragging is handled by hand for the same reason:
//! the system move loop would activate the window.

use std::cell::RefCell;
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use glance_core::{Registry, Session};
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{InvalidateRect, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetWindowLongPtrW, GetWindowRect,
    LoadCursorW, RegisterClassW, SetWindowLongPtrW, SetWindowPos, ShowWindow, CREATESTRUCTW,
    CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HWND_TOPMOST, IDC_ARROW, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_NOZORDER, SW_SHOWNOACTIVATE, WM_DPICHANGED, WM_ERASEBKGND, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SIZE, WNDCLASSW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::layout::{self, ClusterLayout, Hit, Metrics};
use crate::render::{Gpu, Scene, Target};

const CLASS: PCWSTR = w!("GlanceCluster");
const DRAG_THRESHOLD: i32 = 4;

/// What the window shares with the app: GPU objects and the session store.
pub struct Shared {
    pub gpu: Gpu,
    pub metrics: Metrics,
    pub registry: Arc<Mutex<Registry>>,
}

/// One project cluster on screen.
pub struct Cluster {
    pub hwnd: HWND,
    /// Project key: sessions whose project key matches are shown here.
    pub key: String,
    pub name: String,
    pub collapsed: bool,
    /// Once the user has dragged it, auto layout leaves it alone.
    pub pinned: bool,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    drag: RefCell<Option<Drag>>,
    layout: RefCell<ClusterLayout>,
}

struct Drag {
    start_cursor: POINT,
    start_window: POINT,
    moved: bool,
}

pub fn register_class() -> Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        // Zero means failure. Registering twice is the only realistic cause
        // and is harmless, so no error is surfaced.
        RegisterClassW(&wc);
        Ok(())
    }
}

impl Cluster {
    /// Creates the window hidden at `(x, y)` in physical pixels, then shows it
    /// without activating it.
    pub fn create(
        shared: Rc<Shared>,
        key: String,
        name: String,
        x: i32,
        y: i32,
    ) -> Result<Box<Self>> {
        let n = shared
            .registry
            .lock()
            .map(|r| r.all().filter(|s| project_key(s) == key).count())
            .unwrap_or(0);
        let initial = layout::cluster(&shared.metrics, n, false);

        let mut cluster = Box::new(Cluster {
            hwnd: HWND::default(),
            key,
            name,
            collapsed: false,
            pinned: false,
            shared,
            target: RefCell::new(None),
            drag: RefCell::new(None),
            layout: RefCell::new(initial),
        });

        unsafe {
            let instance = GetModuleHandleW(None)?;
            // Size is corrected for DPI right after creation, once the
            // window knows which monitor it is on.
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS,
                w!("Glance"),
                WS_POPUP,
                x,
                y,
                10,
                10,
                None,
                None,
                Some(instance.into()),
                Some(&*cluster as *const Cluster as *const c_void),
            )?;
            cluster.hwnd = hwnd;

            let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            // Hide the 1px system border, the rounded fill is the edge.
            let none = COLORREF(0xFFFF_FFFE);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR,
                &none as *const _ as *const c_void,
                std::mem::size_of::<COLORREF>() as u32,
            );

            cluster.fit();
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        Ok(cluster)
    }

    pub fn destroy(&self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    pub fn dpi(&self) -> u32 {
        unsafe { GetDpiForWindow(self.hwnd) }.max(96)
    }

    fn scale(&self) -> f32 {
        self.dpi() as f32 / 96.0
    }

    /// Window size in physical pixels for the current layout.
    pub fn size_px(&self) -> (i32, i32) {
        let l = self.layout.borrow();
        let s = self.scale();
        ((l.size.0 * s).round() as i32, (l.size.1 * s).round() as i32)
    }

    pub fn position(&self) -> (i32, i32) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut r);
        }
        (r.left, r.top)
    }

    pub fn move_to(&self, x: i32, y: i32) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                x,
                y,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOSIZE,
            );
        }
    }

    /// Keeps the window above everything without activating it. Called after
    /// a state change so a tile that lights up is not hidden behind an editor.
    pub fn raise(&self) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
            );
        }
    }

    pub fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn sessions(&self) -> Vec<Session> {
        self.shared
            .registry
            .lock()
            .map(|r| {
                r.all()
                    .filter(|s| project_key(s) == self.key)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Recomputes layout from the registry and resizes the window to fit.
    pub fn fit(&self) {
        let n = self.sessions().len();
        *self.layout.borrow_mut() = layout::cluster(&self.shared.metrics, n, self.collapsed);
        // Compare with the real window, not the previous layout: a window is
        // born 10 by 10 and must grow even when its layout never changes.
        let (w, h) = self.size_px();
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut r);
        }
        if (r.right - r.left, r.bottom - r.top) != (w, h) {
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    0,
                    0,
                    w,
                    h,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOMOVE,
                );
            }
        }
        self.invalidate();
    }

    fn paint(&self) {
        let (w, h) = self.size_px();
        let dpi = self.dpi();
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            match Target::new(&self.shared.gpu, self.hwnd, w as u32, h as u32, dpi) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("glance: render target for {}: {e}", self.name);
                    return;
                }
            }
        }
        if std::env::var_os("GLANCE_DEBUG").is_some() {
            eprintln!("paint {} {}x{} dpi {dpi}", self.name, w, h);
        }
        let sessions = self.sessions();
        let refs: Vec<&Session> = sessions.iter().collect();
        let layout = self.layout.borrow();
        let scene = Scene {
            layout: &layout,
            name: &self.name,
            collapsed: self.collapsed,
            sessions: &refs,
            now: SystemTime::now(),
        };
        let result = slot
            .as_ref()
            .map(|t| t.draw(&self.shared.gpu, &self.shared.metrics, &scene));
        // Any EndDraw failure, including D2DERR_RECREATE_TARGET, drops the
        // target. The next paint makes a fresh one.
        if let Some(Err(_)) = result {
            *slot = None;
        }
    }

    fn handle(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                self.paint();
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd), None);
                }
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_SIZE => {
                let w = (lparam.0 & 0xffff) as u32;
                let h = ((lparam.0 >> 16) & 0xffff) as u32;
                if let Some(t) = self.target.borrow().as_ref() {
                    let _ = t.resize(w, h);
                }
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                let dpi = (wparam.0 & 0xffff) as u32;
                if let Some(t) = self.target.borrow().as_ref() {
                    t.set_dpi(dpi);
                }
                let suggested = unsafe { *(lparam.0 as *const RECT) };
                let (w, h) = self.size_px();
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        suggested.left,
                        suggested.top,
                        w,
                        h,
                        SWP_NOACTIVATE | SWP_NOZORDER,
                    );
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                let mut cursor = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut cursor);
                    SetCapture(self.hwnd);
                }
                let (x, y) = self.position();
                *self.drag.borrow_mut() = Some(Drag {
                    start_cursor: cursor,
                    start_window: POINT { x, y },
                    moved: false,
                });
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                let mut drag = self.drag.borrow_mut();
                if let Some(d) = drag.as_mut() {
                    let mut cursor = POINT::default();
                    unsafe {
                        let _ = GetCursorPos(&mut cursor);
                    }
                    let dx = cursor.x - d.start_cursor.x;
                    let dy = cursor.y - d.start_cursor.y;
                    if d.moved || dx.abs() > DRAG_THRESHOLD || dy.abs() > DRAG_THRESHOLD {
                        d.moved = true;
                        self.move_to(d.start_window.x + dx, d.start_window.y + dy);
                    }
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                unsafe {
                    let _ = ReleaseCapture();
                }
                let drag = self.drag.borrow_mut().take();
                match drag {
                    Some(d) if d.moved => {
                        // Pinning is recorded by the app on the next reconcile,
                        // via `take_pinned`.
                        self.set_pinned();
                    }
                    Some(_) => self.click(lparam),
                    None => {}
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    fn set_pinned(&self) {
        // `pinned` is plain data owned by the app through the Box; a click
        // handler only has `&self`, so it goes through a cell.
        PINNED.with(|p| p.borrow_mut().push(self.hwnd.0 as isize));
    }

    /// Drains the set of windows the user dragged since the last call.
    pub fn take_pinned() -> Vec<isize> {
        PINNED.with(|p| std::mem::take(&mut *p.borrow_mut()))
    }

    fn click(&self, lparam: LPARAM) {
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32 / s;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
        let hit = layout::hit(&self.layout.borrow(), x, y);
        match hit {
            Hit::Header => TOGGLES.with(|t| t.borrow_mut().push(self.hwnd.0 as isize)),
            Hit::Tile(i) => {
                let _ = i; // Expanding into a terminal comes in step 3.
            }
            Hit::Nothing => {}
        }
    }

    /// Drains header clicks since the last call. The app flips `collapsed`
    /// and calls `fit`, since it owns the Box.
    pub fn take_toggles() -> Vec<isize> {
        TOGGLES.with(|t| std::mem::take(&mut *t.borrow_mut()))
    }
}

thread_local! {
    static PINNED: RefCell<Vec<isize>> = const { RefCell::new(Vec::new()) };
    static TOGGLES: RefCell<Vec<isize>> = const { RefCell::new(Vec::new()) };
}

/// The project a session belongs to. For now its working directory,
/// normalised. Worktrees will map back to their repository in step 4.
pub fn project_key(s: &Session) -> String {
    s.cwd
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

/// A readable name for a project key.
pub fn project_name(key: &str) -> String {
    key.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(key)
        .to_string()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Cluster;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The Box lives in the app for as long as the window exists, and the app
    // destroys the window before dropping the Box.
    let cluster = &*ptr;
    match cluster.handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
