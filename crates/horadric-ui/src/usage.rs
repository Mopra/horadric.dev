//! The usage window: how much of the account's Claude limits is used, and
//! the model, effort and permission mode every session Horadric starts gets.
//!
//! It belongs to no project, so it is a window of its own rather than a tile
//! in a cluster, and it sits at the top of the stack the clusters make. It
//! behaves like a cluster: it never takes the focus, it drags and snaps the
//! same way, and its header folds it. A setting opens a menu, which the app
//! runs, since it owns the defaults.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::time::SystemTime;

use horadric_core::Setting;
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{InvalidateRect, ScreenToClient, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetWindowLongPtrW, GetWindowRect,
    LoadCursorW, RegisterClassW, SetWindowLongPtrW, SetWindowPos, ShowWindow, CREATESTRUCTW,
    CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HWND_NOTOPMOST, HWND_TOPMOST, IDC_ARROW, MA_NOACTIVATE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_SHOWNOACTIVATE, WM_CAPTURECHANGED,
    WM_DPICHANGED, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE,
    WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SIZE, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_POPUP,
};

use crate::app::{self, Input};
use crate::backdrop::{self, Material};
use crate::layout::{self, UsageHit, UsageLayout};
use crate::render::{Target, UsageScene};
use crate::snapping;
use crate::window::Shared;

pub(crate) const CLASS: PCWSTR = w!("HoradricUsage");
const DRAG_THRESHOLD: i32 = 4;
/// The windows crate files this under `Win32_UI_Controls`.
const WM_MOUSELEAVE: u32 = 0x02A3;

pub struct UsageWindow {
    pub hwnd: HWND,
    pub collapsed: Cell<bool>,
    /// Once the user has dragged it, auto layout leaves it alone.
    pub pinned: Cell<bool>,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    layout: RefCell<UsageLayout>,
    drag: RefCell<Option<Drag>>,
    hot: Cell<UsageHit>,
    pressed: Cell<Option<UsageHit>>,
    tracking: Cell<bool>,
    /// Acrylic is behind the window, as behind a cluster.
    glass: Cell<bool>,
}

struct Drag {
    start_cursor: POINT,
    start_window: POINT,
    moved: bool,
    /// The other windows, read once: they cannot move during this drag.
    others: Vec<snapping::Edges>,
}

pub fn register_class() -> Result<()> {
    unsafe {
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
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

impl UsageWindow {
    /// Creates the window at `(x, y)` in physical pixels and shows it
    /// without activating it.
    pub fn create(shared: Rc<Shared>, collapsed: bool, x: i32, y: i32) -> Result<Box<Self>> {
        let initial = layout::usage(&shared.metrics, 0, Setting::ALL.len(), collapsed);
        let mut win = Box::new(UsageWindow {
            hwnd: HWND::default(),
            collapsed: Cell::new(collapsed),
            pinned: Cell::new(false),
            shared,
            target: RefCell::new(None),
            layout: RefCell::new(initial),
            drag: RefCell::new(None),
            hot: Cell::new(UsageHit::Nothing),
            pressed: Cell::new(None),
            tracking: Cell::new(false),
            glass: Cell::new(false),
        });
        unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS,
                w!("Horadric usage"),
                WS_POPUP,
                x,
                y,
                10,
                10,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*win as *const UsageWindow as *const c_void),
            )?;
            win.hwnd = hwnd;
            let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            // The glass draws its own edge.
            backdrop::border(hwnd, None);
            win.glass.set(backdrop::apply(hwnd, Material::Acrylic));
            win.fit();
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        Ok(win)
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

    /// Above other windows without activating, as a cluster does it.
    pub fn raise(&self) {
        for after in [HWND_TOPMOST, HWND_NOTOPMOST] {
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
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

    pub fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn limits(&self) -> usize {
        self.shared
            .usage
            .lock()
            .ok()
            .and_then(|u| u.as_ref().map(|u| u.limits.named().len()))
            .unwrap_or(0)
    }

    /// Lays out again for the limits known now and resizes to fit. True
    /// when the size changed, so the stack needs arranging again.
    pub fn fit(&self) -> bool {
        let l = layout::usage(
            &self.shared.metrics,
            self.limits(),
            Setting::ALL.len(),
            self.collapsed.get(),
        );
        *self.layout.borrow_mut() = l;
        // Folding moves the rows out from under a cursor that has not moved.
        self.refresh_hover();
        let (w, h) = self.size_px();
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut r);
        }
        let changed = (r.right - r.left, r.bottom - r.top) != (w, h);
        if changed {
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
        changed
    }

    fn paint(&self) {
        let (w, h) = self.size_px();
        let dpi = self.dpi();
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            match Target::with_glass(
                &self.shared.gpu,
                self.hwnd,
                w as u32,
                h as u32,
                dpi,
                self.glass.get(),
            ) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for the usage window: {e}");
                    return;
                }
            }
        }
        let usage = self.shared.usage.lock().ok().and_then(|u| u.clone());
        let defaults = self.shared.defaults.borrow();
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let layout = self.layout.borrow();
        let scene = UsageScene {
            layout: &layout,
            collapsed: self.collapsed.get(),
            usage: usage.as_ref(),
            now,
            settings: Setting::ALL
                .iter()
                .map(|&s| (s.label(), s.name_of(defaults.get(s))))
                .collect(),
            hot: self.hot.get(),
            pressed: self.pressed.get(),
        };
        let result = slot
            .as_ref()
            .map(|t| t.draw_usage(&self.shared.gpu, &self.shared.metrics, &scene));
        if let Some(Err(_)) = result {
            *slot = None;
        }
    }

    fn hit(&self, lparam: LPARAM) -> UsageHit {
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32 / s;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
        layout::usage_hit(&self.layout.borrow(), x, y)
    }

    fn hover(&self, hot: UsageHit) {
        if self.hot.replace(hot) != hot {
            self.invalidate();
        }
    }

    fn press(&self, pressed: Option<UsageHit>) {
        if self.pressed.replace(pressed) != pressed {
            self.invalidate();
        }
    }

    fn track(&self) {
        if self.tracking.replace(true) {
            return;
        }
        let mut t = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: self.hwnd,
            dwHoverTime: 0,
        };
        unsafe {
            let _ = TrackMouseEvent(&mut t);
        }
    }

    fn click(&self, hit: UsageHit) {
        match hit {
            UsageHit::Header => {
                self.collapsed.set(!self.collapsed.get());
                self.fit();
                app::push(Input::Arrange);
            }
            UsageHit::Setting(i) => {
                if let Some(&s) = Setting::ALL.get(i) {
                    app::push(Input::SettingMenu(s));
                }
            }
            UsageHit::Nothing => {}
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
            WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
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
                self.raise();
                let mut cursor = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut cursor);
                    SetCapture(self.hwnd);
                }
                self.press(Some(self.hit(lparam)));
                let (x, y) = self.position();
                *self.drag.borrow_mut() = Some(Drag {
                    start_cursor: cursor,
                    start_window: POINT { x, y },
                    moved: false,
                    others: snapping::others(self.hwnd),
                });
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                self.track();
                self.hover(self.hit(lparam));
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
                        self.press(None);
                        let pos = (d.start_window.x + dx, d.start_window.y + dy);
                        let (x, y) = match snapping::frame(self.dpi()) {
                            Some((work, spacing)) => {
                                layout::snap(pos, self.size_px(), work, &d.others, spacing)
                            }
                            None => pos,
                        };
                        self.move_to(x, y);
                    }
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                // Before letting go: releasing capture sends
                // WM_CAPTURECHANGED at once, and that clears the press.
                let pressed = self.pressed.get();
                unsafe {
                    let _ = ReleaseCapture();
                }
                self.press(None);
                let drag = self.drag.borrow_mut().take();
                match drag {
                    Some(d) if d.moved => {
                        self.pinned.set(true);
                        app::push(Input::Arrange);
                    }
                    // Only where the press began, as a button does.
                    Some(_) if pressed == Some(self.hit(lparam)) => self.click(self.hit(lparam)),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking.set(false);
                self.hover(UsageHit::Nothing);
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                self.press(None);
                None
            }
            _ => None,
        }
    }

    /// Notes what the cursor is over now, when it is over the window.
    fn refresh_hover(&self) {
        if !self.tracking.get() {
            return;
        }
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
            let _ = ScreenToClient(self.hwnd, &mut p);
        }
        let s = self.scale();
        let hit = layout::usage_hit(&self.layout.borrow(), p.x as f32 / s, p.y as f32 / s);
        self.hover(hit);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const UsageWindow;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The Box lives in the app for as long as the window exists, and the app
    // destroys the window before dropping the Box.
    let win = &*ptr;
    match win.handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
