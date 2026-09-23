//! The terminal window: a session expanded from its tile.
//!
//! Unlike a cluster, this window takes focus, because you type into it. It
//! is a normal window: resizable, in the taskbar and in alt-tab. Closing it
//! only collapses the session back into its tile; the agent keeps running.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::Arc;

use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use windows::core::{w, Result, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_CAPTION_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
};
use windows::Win32::Graphics::Gdi::{InvalidateRect, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, VIRTUAL_KEY, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END,
    VK_F1, VK_F12, VK_F4, VK_HOME, VK_INSERT, VK_LEFT, VK_MENU, VK_NEXT, VK_PRIOR, VK_RIGHT,
    VK_SHIFT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetForegroundWindow,
    GetWindowLongPtrW, GetWindowRect, IsIconic, KillTimer, LoadCursorW, RegisterClassW,
    SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow,
    CREATESTRUCTW, CS_DBLCLKS, CW_USEDEFAULT, GWLP_USERDATA, IDC_IBEAM, SWP_NOACTIVATE,
    SWP_NOZORDER, SW_RESTORE, SW_SHOWNORMAL, WINDOW_EX_STYLE, WM_CHAR, WM_CLOSE, WM_DPICHANGED,
    WM_ERASEBKGND, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_RBUTTONUP, WM_SETFOCUS,
    WM_SIZE, WM_SYSCHAR, WM_SYSKEYDOWN, WM_TIMER, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

use crate::app::{self, Input};
use crate::console::{Console, GridSize};
use crate::frame;
use crate::glyphs::{CellSize, GridTarget, PAD};
use crate::keys::{self, CharAction, Key, Mods};
use crate::window::Shared;
use crate::{clipboard, palette};

const CLASS: PCWSTR = w!("GlanceTerminal");
const SYNC_TIMER: usize = 1;
const WHEEL_LINES: i32 = 3;

pub struct TerminalWindow {
    pub hwnd: HWND,
    pub console: Arc<Console>,
    shared: Rc<Shared>,
    /// Session name and project, the part of the title Glance owns.
    label: String,
    target: RefCell<Option<GridTarget>>,
    /// False until the window has its real size. The size it is born with
    /// would otherwise reach the agent as a resize and a redraw.
    placed: Cell<bool>,
    focused: Cell<bool>,
    selecting: Cell<bool>,
    /// First half of a character outside the BMP, until the second arrives.
    high_surrogate: Cell<Option<u16>>,
    /// Wheel movement below one notch, from precision touchpads.
    wheel: Cell<i32>,
    title: RefCell<String>,
}

pub fn register_class() -> Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_IBEAM)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        Ok(())
    }
}

impl TerminalWindow {
    /// Opens a window on a console and brings it to the front.
    pub fn open(shared: Rc<Shared>, console: Arc<Console>, label: String) -> Result<Box<Self>> {
        let mut win = Box::new(TerminalWindow {
            hwnd: HWND::default(),
            console,
            shared,
            label,
            target: RefCell::new(None),
            placed: Cell::new(false),
            focused: Cell::new(false),
            selecting: Cell::new(false),
            high_surrogate: Cell::new(None),
            wheel: Cell::new(0),
            title: RefCell::new(String::new()),
        });
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS,
                &HSTRING::from(win.label.as_str()),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                None,
                None,
                Some(instance.into()),
                Some(&*win as *const TerminalWindow as *const c_void),
            )?;
            win.hwnd = hwnd;

            let dark = BOOL(1);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark as *const _ as *const c_void,
                std::mem::size_of::<BOOL>() as u32,
            );
            let bg = palette::BACKGROUND;
            let caption = COLORREF(bg.r as u32 | (bg.g as u32) << 8 | (bg.b as u32) << 16);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_CAPTION_COLOR,
                &caption as *const _ as *const c_void,
                std::mem::size_of::<COLORREF>() as u32,
            );

            win.place();
            win.placed.set(true);
            win.fit_grid();
            win.refresh_title();
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(hwnd);
        }
        Ok(win)
    }

    /// Where the window was last time, or centred at the console's size.
    fn place(&self) {
        let saved = self.console.placement.lock().ok().and_then(|p| *p);
        let (x, y, w, h) = match saved {
            Some((l, t, r, b)) => (l, t, r - l, b - t),
            None => {
                let dpi = self.dpi();
                let scale = dpi as f32 / 96.0;
                let cell = self.cell();
                let size = self.console.size();
                let client_w = size.cols as f32 * cell.w + 2.0 * PAD;
                let client_h = size.rows as f32 * cell.h + 2.0 * PAD;
                let mut r = RECT {
                    left: 0,
                    top: 0,
                    right: (client_w * scale).ceil() as i32,
                    bottom: (client_h * scale).ceil() as i32,
                };
                unsafe {
                    let _ = AdjustWindowRectExForDpi(
                        &mut r,
                        WS_OVERLAPPEDWINDOW,
                        false,
                        WINDOW_EX_STYLE(0),
                        dpi,
                    );
                }
                let (w, h) = (r.right - r.left, r.bottom - r.top);
                let (left, top, right, bottom) = app::work_area();
                let x = left + ((right - left - w) / 2).max(0);
                let y = top + ((bottom - top - h) / 2).max(0);
                (x, y, w, h)
            }
        };
        unsafe {
            let _ = SetWindowPos(self.hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }

    /// Remembers where the window is, then destroys it. The console lives on.
    pub fn destroy(&self) {
        unsafe {
            if !IsIconic(self.hwnd).as_bool() {
                let mut r = RECT::default();
                if GetWindowRect(self.hwnd, &mut r).is_ok() {
                    if let Ok(mut p) = self.console.placement.lock() {
                        *p = Some((r.left, r.top, r.right, r.bottom));
                    }
                }
            }
            let _ = DestroyWindow(self.hwnd);
        }
    }

    pub fn bring_to_front(&self) {
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    pub fn is_foreground(&self) -> bool {
        unsafe { GetForegroundWindow() == self.hwnd }
    }

    pub fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// "what the agent says it is doing · session name, project".
    pub fn refresh_title(&self) {
        let title = match self.console.title() {
            Some(t) if !t.trim().is_empty() => format!("{} \u{00B7} {}", t.trim(), self.label),
            _ => self.label.clone(),
        };
        let title = match self.console.exit_code() {
            Some(code) => format!("{title} (exited {code})"),
            None => title,
        };
        if *self.title.borrow() != title {
            unsafe {
                let _ = SetWindowTextW(self.hwnd, &HSTRING::from(title.as_str()));
            }
            *self.title.borrow_mut() = title;
        }
    }

    fn dpi(&self) -> u32 {
        unsafe { GetDpiForWindow(self.hwnd) }.max(96)
    }

    fn cell(&self) -> CellSize {
        self.shared.font.cell(self.dpi())
    }

    /// Resizes the grid to fill the client area.
    fn fit_grid(&self) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        if r.right <= 0 || r.bottom <= 0 {
            return;
        }
        let scale = self.dpi() as f32 / 96.0;
        let cell = self.cell();
        let cols = ((r.right as f32 / scale - 2.0 * PAD) / cell.w)
            .floor()
            .max(2.0);
        let rows = ((r.bottom as f32 / scale - 2.0 * PAD) / cell.h)
            .floor()
            .max(1.0);
        self.console.resize(GridSize {
            cols: cols as u16,
            rows: rows as u16,
        });
    }

    fn paint(&self) {
        if let Some(wait) = self.console.flush_sync() {
            unsafe {
                SetTimer(
                    Some(self.hwnd),
                    SYNC_TIMER,
                    wait.as_millis() as u32 + 1,
                    None,
                );
            }
        }
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        let dpi = self.dpi();
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            match GridTarget::new(
                &self.shared.gpu,
                self.hwnd,
                r.right as u32,
                r.bottom as u32,
                dpi,
            ) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("glance: terminal render target: {e}");
                    return;
                }
            }
        }
        let font = &self.shared.font;
        let cell = font.cell(dpi);
        let started = std::time::Instant::now();
        let frame = match self.console.screen.lock() {
            Ok(s) => frame::build(&s.term, self.focused.get(), |c, style| font.glyph(c, style)),
            Err(_) => return,
        };
        let result = slot.as_ref().map(|t| t.draw(font, &cell, &frame));
        if std::env::var_os("GLANCE_DEBUG").is_some() {
            eprintln!(
                "paint terminal {} runs {} loose {} in {:?}",
                self.console.id,
                frame.runs.len(),
                frame.loose.len(),
                started.elapsed()
            );
        }
        if let Some(Err(_)) = result {
            *slot = None;
        }
    }

    fn mods() -> Mods {
        let down = |vk: VIRTUAL_KEY| unsafe { GetKeyState(vk.0 as i32) } < 0;
        Mods {
            shift: down(VK_SHIFT),
            ctrl: down(VK_CONTROL),
            alt: down(VK_MENU),
        }
    }

    /// Sends typed bytes: the view jumps back to the live screen and any
    /// selection goes, as in every terminal.
    fn send(&self, bytes: Vec<u8>) {
        if let Ok(mut s) = self.console.screen.lock() {
            s.term.selection = None;
            if s.term.grid().display_offset() != 0 {
                s.term.scroll_display(Scroll::Bottom);
            }
        }
        self.console.write(bytes);
        self.invalidate();
    }

    fn mode(&self) -> TermMode {
        self.console
            .screen
            .lock()
            .map(|s| *s.term.mode())
            .unwrap_or_default()
    }

    fn paste(&self) {
        match clipboard::get_text() {
            Some(text) => {
                let bracketed = self.mode().contains(TermMode::BRACKETED_PASTE);
                self.send(keys::paste_bytes(&text, bracketed));
            }
            // No text: pass Ctrl+V on, so the agent can read an image from
            // the clipboard itself.
            None => self.send(vec![0x16]),
        }
    }

    /// Copies the selection, if there is one. Returns whether it did.
    fn copy(&self) -> bool {
        let text = self.console.screen.lock().ok().and_then(|mut s| {
            let t = s.term.selection_to_string();
            s.term.selection = None;
            t
        });
        self.invalidate();
        match text {
            Some(t) if !t.is_empty() => {
                clipboard::set_text(&t);
                true
            }
            _ => false,
        }
    }

    fn has_selection(&self) -> bool {
        self.console
            .screen
            .lock()
            .map(|s| s.term.selection.as_ref().is_some_and(|sel| !sel.is_empty()))
            .unwrap_or(false)
    }

    fn on_char(&self, unit: u16, mods: Mods) {
        let c = if (0xD800..0xDC00).contains(&unit) {
            self.high_surrogate.set(Some(unit));
            return;
        } else if (0xDC00..0xE000).contains(&unit) {
            let Some(high) = self.high_surrogate.take() else {
                return;
            };
            char::decode_utf16([high, unit]).next().and_then(|r| r.ok())
        } else {
            char::from_u32(unit as u32)
        };
        let Some(c) = c else { return };
        match keys::char_action(c, mods) {
            CharAction::Send(bytes) => self.send(bytes),
            CharAction::Paste => self.paste(),
            CharAction::Copy => {
                self.copy();
            }
            CharAction::CopyOrInterrupt => {
                if !self.has_selection() || !self.copy() {
                    self.send(vec![0x03]);
                }
            }
        }
    }

    /// Keys that make no character. Returns false to let Windows have it.
    fn on_key(&self, vk: u16, mods: Mods) -> bool {
        let vk = VIRTUAL_KEY(vk);
        // Alt+F4 closes, which collapses the session. Never swallow it.
        if mods.alt && vk == VK_F4 {
            return false;
        }
        if mods.shift && (vk == VK_PRIOR || vk == VK_NEXT) {
            let scroll = if vk == VK_PRIOR {
                Scroll::PageUp
            } else {
                Scroll::PageDown
            };
            if let Ok(mut s) = self.console.screen.lock() {
                s.term.scroll_display(scroll);
            }
            self.invalidate();
            return true;
        }
        if vk == VK_INSERT && mods.shift {
            self.paste();
            return true;
        }
        if vk == VK_INSERT && mods.ctrl {
            self.copy();
            return true;
        }
        let key = match vk {
            VK_UP => Key::Up,
            VK_DOWN => Key::Down,
            VK_LEFT => Key::Left,
            VK_RIGHT => Key::Right,
            VK_HOME => Key::Home,
            VK_END => Key::End,
            VK_PRIOR => Key::PageUp,
            VK_NEXT => Key::PageDown,
            VK_INSERT => Key::Insert,
            VK_DELETE => Key::Delete,
            v if (VK_F1.0..=VK_F12.0).contains(&v.0) => Key::F((v.0 - VK_F1.0 + 1) as u8),
            _ => return false,
        };
        let app_cursor = self.mode().contains(TermMode::APP_CURSOR);
        self.send(keys::key_bytes(key, mods, app_cursor));
        true
    }

    /// The cell under a client point, and which half of it.
    fn cell_at(&self, lparam: LPARAM) -> (Point, Side) {
        let scale = self.dpi() as f32 / 96.0;
        let x = (lparam.0 & 0xffff) as i16 as f32 / scale;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / scale;
        let cell = self.cell();
        let size = self.console.size();
        let colf = ((x - PAD) / cell.w).max(0.0);
        let col = (colf as usize).min(size.cols as usize - 1);
        let side = if colf.fract() < 0.5 && (colf as usize) < size.cols as usize {
            Side::Left
        } else {
            Side::Right
        };
        let row = (((y - PAD) / cell.h).max(0.0) as usize).min(size.rows as usize - 1);
        let offset = self
            .console
            .screen
            .lock()
            .map(|s| s.term.grid().display_offset())
            .unwrap_or(0);
        (
            Point::new(Line(row as i32 - offset as i32), Column(col)),
            side,
        )
    }

    fn start_selection(&self, lparam: LPARAM, ty: SelectionType) {
        let (point, side) = self.cell_at(lparam);
        if let Ok(mut s) = self.console.screen.lock() {
            s.term.selection = Some(Selection::new(ty, point, side));
        }
        self.selecting.set(true);
        unsafe {
            SetCapture(self.hwnd);
        }
        self.invalidate();
    }

    fn on_wheel(&self, wparam: WPARAM) {
        let delta = ((wparam.0 >> 16) & 0xffff) as i16 as i32 + self.wheel.get();
        let notches = delta / 120;
        self.wheel.set(delta % 120);
        if notches == 0 {
            return;
        }
        let lines = notches * WHEEL_LINES;
        let mode = self.mode();
        if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            // Full screen programs scroll themselves. Give them arrow keys.
            let key = if lines > 0 { Key::Up } else { Key::Down };
            let one = keys::key_bytes(key, Mods::NONE, mode.contains(TermMode::APP_CURSOR));
            self.console
                .write(one.repeat(lines.unsigned_abs() as usize));
        } else if let Ok(mut s) = self.console.screen.lock() {
            s.term.scroll_display(Scroll::Delta(lines));
        }
        self.invalidate();
    }

    fn set_focus(&self, focused: bool) {
        self.focused.set(focused);
        let mode = self.mode();
        if let Ok(mut s) = self.console.screen.lock() {
            s.term.is_focused = focused;
        }
        if mode.contains(TermMode::FOCUS_IN_OUT) {
            self.console.write(if focused {
                &b"\x1b[I"[..]
            } else {
                &b"\x1b[O"[..]
            });
        }
        self.invalidate();
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
                if self.placed.get() && w > 0 && h > 0 {
                    self.fit_grid();
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                let dpi = (wparam.0 & 0xffff) as u32;
                if let Some(t) = self.target.borrow().as_ref() {
                    t.set_dpi(dpi);
                }
                let r = unsafe { *(lparam.0 as *const RECT) };
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == SYNC_TIMER => {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), SYNC_TIMER);
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                app::push(Input::Collapse(self.console.serial));
                Some(LRESULT(0))
            }
            WM_SETFOCUS => {
                self.set_focus(true);
                Some(LRESULT(0))
            }
            WM_KILLFOCUS => {
                self.set_focus(false);
                Some(LRESULT(0))
            }
            WM_CHAR => {
                // AltGr is Ctrl+Alt, so Alt here is never Meta. Meta comes as
                // WM_SYSCHAR.
                let mods = Mods {
                    alt: false,
                    ..Self::mods()
                };
                self.on_char(wparam.0 as u16, mods);
                Some(LRESULT(0))
            }
            WM_SYSCHAR => {
                // Alt+Space is the window menu.
                if wparam.0 == ' ' as usize {
                    return None;
                }
                let mods = Mods {
                    alt: true,
                    ..Self::mods()
                };
                self.on_char(wparam.0 as u16, mods);
                Some(LRESULT(0))
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                if self.on_key(wparam.0 as u16, Self::mods()) {
                    Some(LRESULT(0))
                } else {
                    None
                }
            }
            WM_LBUTTONDOWN => {
                self.start_selection(lparam, SelectionType::Simple);
                Some(LRESULT(0))
            }
            WM_LBUTTONDBLCLK => {
                self.start_selection(lparam, SelectionType::Semantic);
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                if self.selecting.get() {
                    let (point, side) = self.cell_at(lparam);
                    if let Ok(mut s) = self.console.screen.lock() {
                        if let Some(sel) = s.term.selection.as_mut() {
                            sel.update(point, side);
                        }
                    }
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                if self.selecting.replace(false) {
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    // Copy on select, like Windows Terminal can. A click
                    // without a drag selects nothing and clears.
                    let text = self.console.screen.lock().ok().and_then(|mut s| {
                        let empty = s.term.selection.as_ref().is_none_or(|sel| sel.is_empty());
                        if empty {
                            s.term.selection = None;
                            None
                        } else {
                            s.term.selection_to_string()
                        }
                    });
                    if let Some(t) = text.filter(|t| !t.is_empty()) {
                        clipboard::set_text(&t);
                    }
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONUP => {
                // The console convention: right click copies a selection,
                // otherwise pastes.
                if !self.has_selection() || !self.copy() {
                    self.paste();
                }
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                self.on_wheel(wparam);
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
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TerminalWindow;
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
