//! One session inside the stage: a child window that draws its console's
//! grid and takes its keyboard and mouse.
//!
//! The stage lays its panes out in a grid, one per session of the project
//! it shows. With more than one, each pane has a header naming its session,
//! and dragging a header onto another pane swaps the two. The drag itself is
//! the stage's: a pane only says it was grabbed.
//!
//! A pane can also show a file instead of a session (see
//! [`Console::view`]). It is read only: keys scroll it, Ctrl+C copies, and
//! Esc or the cross in its header closes it.
//!
//! Ctrl+Shift+T in any pane opens a plain terminal in the project on the
//! stage.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{InvalidateRect, ScreenToClient, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, VIRTUAL_KEY, VK_CONTROL, VK_DELETE, VK_DOWN,
    VK_END, VK_F1, VK_F12, VK_F4, VK_HOME, VK_INSERT, VK_LEFT, VK_MENU, VK_NEXT, VK_PRIOR,
    VK_RIGHT, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos, GetParent,
    GetWindowLongPtrW, KillTimer, LoadCursorW, RegisterClassW, SendMessageW, SetCursor, SetTimer,
    SetWindowLongPtrW, SetWindowPos, CREATESTRUCTW, CS_DBLCLKS, GWLP_USERDATA, HTCLIENT, IDC_ARROW,
    IDC_IBEAM, SWP_NOACTIVATE, SWP_NOZORDER, WINDOW_EX_STYLE, WM_CHAR, WM_DPICHANGED_AFTERPARENT,
    WM_DROPFILES, WM_ERASEBKGND, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_RBUTTONUP,
    WM_SETCURSOR, WM_SETFOCUS, WM_SIZE, WM_SYSCHAR, WM_SYSKEYDOWN, WM_TIMER, WM_USER, WNDCLASSW,
    WS_CHILD, WS_CLIPSIBLINGS, WS_VISIBLE,
};

use crate::app::{self, Input};
use crate::clipboard;
use crate::console::{Console, GridSize};
use crate::frame;
use crate::glyphs::{CellSize, GridTarget, Header, HEADER_H, PAD};
use crate::keys::{self, CharAction, Key, Mods};
use crate::motion::{self, REVEAL, SPOTLIGHT};
use crate::paste::{self, Source};
use crate::theme::{self, Color};
use crate::window::Shared;

const CLASS: PCWSTR = w!("GlancePane");
const SYNC_TIMER: usize = 1;
/// Asks for the next frame while the pane fades in or steps back.
const ANIM_TIMER: usize = 2;
/// How far a pane without the keyboard steps back: the background laid
/// over it at this strength.
const DIMMED: f32 = 0.32;
const WHEEL_LINES: i32 = 3;

/// Sent to the stage when a pane gets the keyboard. `wparam` is its serial.
pub const WM_PANE_FOCUS: u32 = WM_USER + 1;
/// Sent to the stage when a pane's header is pressed, which may start a
/// drag. `wparam` is its serial.
pub const WM_PANE_GRAB: u32 = WM_USER + 2;

pub struct Pane {
    pub hwnd: HWND,
    console: Arc<Console>,
    shared: Rc<Shared>,
    /// The session's name, for the header.
    name: RefCell<String>,
    header: Cell<bool>,
    lifted: Cell<bool>,
    drop_target: Cell<bool>,
    target: RefCell<Option<GridTarget>>,
    /// The DPI the render target was made for. A child window hears of a
    /// new monitor only through its parent.
    dpi: Cell<u32>,
    focused: Cell<bool>,
    selecting: Cell<bool>,
    /// First half of a character outside the BMP, until the second arrives.
    high_surrogate: Cell<Option<u16>>,
    /// Wheel movement below one notch, from precision touchpads.
    wheel: Cell<i32>,
    /// The project's colour, for the header of the pane with the keyboard.
    accent: Cell<Color>,
    /// How far the pane has stepped back, from 0 to 1, and whether it is
    /// on its way back or forward.
    dim: Cell<f32>,
    dimmed: Cell<bool>,
    /// When the pane was last shown fresh, for the fade in.
    shown: Cell<Option<Instant>>,
    /// The frame before, for how far a fade has got.
    last_frame: Cell<Option<Instant>>,
    animating: Cell<bool>,
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

impl Pane {
    /// A pane for a console inside `parent`, with no size until the stage
    /// lays it out. A size it was born with would reach the agent as a
    /// resize and a redraw.
    pub fn create(
        shared: Rc<Shared>,
        console: Arc<Console>,
        name: String,
        parent: HWND,
    ) -> Result<Box<Self>> {
        let mut pane = Box::new(Pane {
            hwnd: HWND::default(),
            console,
            shared,
            name: RefCell::new(name),
            header: Cell::new(false),
            lifted: Cell::new(false),
            drop_target: Cell::new(false),
            target: RefCell::new(None),
            dpi: Cell::new(0),
            focused: Cell::new(false),
            selecting: Cell::new(false),
            high_surrogate: Cell::new(None),
            wheel: Cell::new(0),
            accent: Cell::new(theme::ACCENTS[0]),
            dim: Cell::new(0.0),
            dimmed: Cell::new(false),
            shown: Cell::new(None),
            last_frame: Cell::new(None),
            animating: Cell::new(false),
        });
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS,
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
                0,
                0,
                0,
                0,
                Some(parent),
                None,
                Some(instance.into()),
                Some(&*pane as *const Pane as *const c_void),
            )?;
            pane.hwnd = hwnd;
            // A dropped path would have nowhere to go in a file.
            DragAcceptFiles(hwnd, !pane.console.is_view());
        }
        Ok(pane)
    }

    pub fn destroy(&self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    pub fn console(&self) -> &Arc<Console> {
        &self.console
    }

    pub fn serial(&self) -> usize {
        self.console.serial
    }

    pub fn session(&self) -> &str {
        &self.console.id
    }

    pub fn name(&self) -> String {
        self.name.borrow().clone()
    }

    pub fn set_name(&self, name: String) {
        if *self.name.borrow() != name {
            *self.name.borrow_mut() = name;
            self.invalidate();
        }
    }

    /// Moves and sizes the pane inside the stage, in client pixels.
    pub fn set_rect(&self, [l, t, r, b]: [i32; 4]) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                l,
                t,
                r - l,
                b - t,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    /// Shows or hides the header. The grid gives up or takes back its rows.
    pub fn set_header(&self, on: bool) {
        if self.header.replace(on) != on {
            self.fit_grid();
            self.invalidate();
        }
    }

    pub fn set_accent(&self, c: Color) {
        if self.accent.replace(c) != c {
            self.invalidate();
        }
    }

    /// Steps the pane back while another has the keyboard, so the one you
    /// type into is the one lit.
    pub fn set_dimmed(&self, on: bool) {
        if self.dimmed.replace(on) != on {
            self.invalidate();
        }
    }

    /// Fades the pane in from the background, for a stage that has just
    /// switched to its project.
    pub fn reveal(&self) {
        self.shown.set(Some(Instant::now()));
        self.invalidate();
    }

    /// Moves the fades on to now and returns how much background to lay
    /// over the pane, asking for another frame while one is under way.
    fn veil(&self) -> f32 {
        let now = Instant::now();
        let dt = self
            .last_frame
            .replace(Some(now))
            .map_or(Duration::ZERO, |t| now.duration_since(t))
            .min(Duration::from_millis(100));
        let target = if self.dimmed.get() { 1.0 } else { 0.0 };
        let dim = motion::fade(self.dim.get(), target, dt, SPOTLIGHT);
        self.dim.set(dim);
        let reveal = self
            .shown
            .get()
            .map_or(0.0, |t| motion::decay(now.duration_since(t), REVEAL));
        if reveal == 0.0 {
            self.shown.set(None);
        }
        let busy = dim != target || reveal > 0.0;
        if busy != self.animating.replace(busy) {
            unsafe {
                if busy {
                    SetTimer(
                        Some(self.hwnd),
                        ANIM_TIMER,
                        motion::FRAME_FAST.as_millis() as u32,
                        None,
                    );
                } else {
                    let _ = KillTimer(Some(self.hwnd), ANIM_TIMER);
                }
            }
        }
        (motion::ease_in_out(dim) * DIMMED).max(reveal)
    }

    pub fn set_lifted(&self, on: bool) {
        if self.lifted.replace(on) != on {
            self.invalidate();
        }
    }

    pub fn set_drop_target(&self, on: bool) {
        if self.drop_target.replace(on) != on {
            self.invalidate();
        }
    }

    pub fn focus(&self) {
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
        }
    }

    pub fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn dpi_now(&self) -> u32 {
        unsafe { GetDpiForWindow(self.hwnd) }.max(96)
    }

    fn cell(&self) -> CellSize {
        self.shared.font.cell(self.dpi_now())
    }

    /// Where the grid starts, below the header when there is one.
    fn top(&self) -> f32 {
        if self.header.get() {
            HEADER_H
        } else {
            0.0
        }
    }

    /// Resizes the grid to fill the pane below its header.
    fn fit_grid(&self) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        if r.right <= 0 || r.bottom <= 0 {
            return;
        }
        let scale = self.dpi_now() as f32 / 96.0;
        let cell = self.cell();
        let cols = ((r.right as f32 / scale - 2.0 * PAD) / cell.w)
            .floor()
            .max(2.0);
        let rows = ((r.bottom as f32 / scale - self.top() - 2.0 * PAD) / cell.h)
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
        let dpi = self.dpi_now();
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            match GridTarget::new(
                &self.shared.gpu,
                self.hwnd,
                r.right as u32,
                r.bottom as u32,
                dpi,
            ) {
                Ok(t) => {
                    *slot = Some(t);
                    self.dpi.set(dpi);
                }
                Err(e) => {
                    eprintln!("glance: pane render target: {e}");
                    return;
                }
            }
        }
        if self.dpi.replace(dpi) != dpi {
            if let Some(t) = slot.as_ref() {
                t.set_dpi(dpi);
            }
        }
        let name = self.name.borrow().clone();
        let detail = match (self.console.exit_code(), self.console.title()) {
            (Some(code), _) => format!("exited {code}"),
            (None, Some(t)) => t.trim().to_string(),
            (None, None) => String::new(),
        };
        let phase = self
            .shared
            .registry
            .lock()
            .ok()
            .and_then(|r| r.get(&self.console.id).map(|s| s.phase.clone()));
        let phase =
            phase.and_then(|p| (theme::edge_strength(&p) > 0.0).then(|| theme::phase_color(&p)));
        let header = self.header.get().then(|| Header {
            name: &name,
            detail: &detail,
            phase,
            accent: self.accent.get(),
            active: self.focused.get(),
            lifted: self.lifted.get(),
            close: self.console.is_view(),
        });
        let veil = self.veil();
        let font = &self.shared.font;
        let cell = font.cell(dpi);
        let frame = match self.console.screen.lock() {
            Ok(s) => frame::build(&s.term, self.focused.get(), |c, style| font.glyph(c, style)),
            Err(_) => return,
        };
        let result = slot.as_ref().map(|t| {
            t.draw(
                &self.shared.gpu,
                font,
                &cell,
                &frame,
                header.as_ref(),
                self.drop_target.get(),
                veil,
            )
        });
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
        let source = paste::choose(
            clipboard::get_text(),
            clipboard::has_image(),
            clipboard::has_files(),
        );
        let text = match source {
            Source::Text(text) => text,
            Source::Image => match clipboard::save_image() {
                Some(path) => paste::quote_paths(&[path.to_string_lossy()]),
                None => return,
            },
            Source::Files => paste::quote_paths(&clipboard::get_files()),
            Source::Nothing => return,
        };
        self.paste_text(&text);
    }

    fn paste_text(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let bracketed = self.mode().contains(TermMode::BRACKETED_PASTE);
        self.send(keys::paste_bytes(text, bracketed));
    }

    /// Files dropped from Explorer arrive as their paths, as in Windows
    /// Terminal, in the pane they were dropped on. It takes the keyboard so
    /// you can type straight after.
    fn on_drop(&self, hdrop: HDROP) {
        let paths = clipboard::drop_paths(hdrop);
        unsafe {
            DragFinish(hdrop);
        }
        self.paste_text(&paste::quote_paths(&paths));
        self.focus();
    }

    /// Copies the selection, if there is one. Returns whether it did.
    fn copy(&self) -> bool {
        let text = self.console.selection_text();
        if let Ok(mut s) = self.console.screen.lock() {
            s.term.selection = None;
        }
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
        if self.console.is_view() {
            match keys::char_action(c, mods) {
                _ if c as u32 == 0x1b => self.close(),
                CharAction::Copy | CharAction::CopyOrInterrupt => {
                    self.copy();
                }
                CharAction::NewShell => app::push(Input::Shell(None)),
                _ => {}
            }
            return;
        }
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
            CharAction::NewShell => app::push(Input::Shell(None)),
        }
    }

    /// Keys that make no character. Returns false to let Windows have it.
    fn on_key(&self, vk: u16, mods: Mods) -> bool {
        let vk = VIRTUAL_KEY(vk);
        // Alt+F4 closes the stage. Windows passes it up from a child.
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
        if self.console.is_view() {
            return self.scroll_view(vk, mods);
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

    /// A file view scrolls with the keys an editor moves its cursor with.
    fn scroll_view(&self, vk: VIRTUAL_KEY, mods: Mods) -> bool {
        let scroll = match vk {
            VK_UP => Scroll::Delta(1),
            VK_DOWN => Scroll::Delta(-1),
            VK_PRIOR => Scroll::PageUp,
            VK_NEXT => Scroll::PageDown,
            VK_HOME if mods.ctrl => Scroll::Top,
            VK_END if mods.ctrl => Scroll::Bottom,
            _ => return false,
        };
        if let Ok(mut s) = self.console.screen.lock() {
            s.term.scroll_display(scroll);
        }
        self.invalidate();
        true
    }

    fn close(&self) {
        app::push(Input::CloseView(self.serial()));
    }

    /// A client point in DIPs.
    fn dip(&self, lparam: LPARAM) -> (f32, f32) {
        let scale = self.dpi_now() as f32 / 96.0;
        let x = (lparam.0 & 0xffff) as i16 as f32 / scale;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / scale;
        (x, y)
    }

    fn in_header(&self, lparam: LPARAM) -> bool {
        self.header.get() && self.dip(lparam).1 < HEADER_H
    }

    /// On the cross that closes a file view, a square at the header's end.
    fn on_close(&self, lparam: LPARAM) -> bool {
        if !self.console.is_view() || !self.in_header(lparam) {
            return false;
        }
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        let width = r.right as f32 * 96.0 / self.dpi_now() as f32;
        self.dip(lparam).0 >= width - HEADER_H
    }

    /// The cell under a client point, and which half of it.
    fn cell_at(&self, lparam: LPARAM) -> (Point, Side) {
        let (x, y) = self.dip(lparam);
        let cell = self.cell();
        let size = self.console.size();
        let colf = ((x - PAD) / cell.w).max(0.0);
        let col = (colf as usize).min(size.cols as usize - 1);
        let side = if colf.fract() < 0.5 && (colf as usize) < size.cols as usize {
            Side::Left
        } else {
            Side::Right
        };
        let row = (((y - self.top() - PAD) / cell.h).max(0.0) as usize).min(size.rows as usize - 1);
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

    /// Tells the console the pane gained or lost the keyboard, for programs
    /// that asked to hear it.
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

    fn tell_stage(&self, msg: u32) {
        unsafe {
            if let Ok(parent) = GetParent(self.hwnd) {
                SendMessageW(parent, msg, Some(WPARAM(self.serial())), None);
            }
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
                if w > 0 && h > 0 {
                    self.fit_grid();
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_DPICHANGED_AFTERPARENT => {
                self.fit_grid();
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == ANIM_TIMER => {
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == SYNC_TIMER => {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), SYNC_TIMER);
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_SETFOCUS => {
                self.set_focus(true);
                self.tell_stage(WM_PANE_FOCUS);
                Some(LRESULT(0))
            }
            WM_KILLFOCUS => {
                self.set_focus(false);
                Some(LRESULT(0))
            }
            WM_SETCURSOR if (lparam.0 & 0xffff) as u32 == HTCLIENT => {
                let mut p = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut p);
                    let _ = ScreenToClient(self.hwnd, &mut p);
                }
                let at = LPARAM(((p.y as u16 as isize) << 16) | p.x as u16 as isize);
                if self.in_header(at) {
                    unsafe {
                        SetCursor(LoadCursorW(None, IDC_ARROW).ok());
                    }
                    Some(LRESULT(1))
                } else {
                    None
                }
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
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                self.focus();
                if self.on_close(lparam) {
                    self.close();
                } else if self.in_header(lparam) {
                    self.tell_stage(WM_PANE_GRAB);
                } else if msg == WM_LBUTTONDOWN {
                    self.start_selection(lparam, SelectionType::Simple);
                } else {
                    self.start_selection(lparam, SelectionType::Semantic);
                }
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
                    let empty = self.console.screen.lock().is_ok_and(|mut s| {
                        let empty = s.term.selection.as_ref().is_none_or(|sel| sel.is_empty());
                        if empty {
                            s.term.selection = None;
                        }
                        empty
                    });
                    let text = (!empty).then(|| self.console.selection_text()).flatten();
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
                if (!self.has_selection() || !self.copy()) && !self.console.is_view() {
                    self.paste();
                }
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                self.on_wheel(wparam);
                Some(LRESULT(0))
            }
            WM_DROPFILES => {
                self.on_drop(HDROP(wparam.0 as *mut c_void));
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
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Pane;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The stage owns the Box and destroys the window before dropping it.
    let pane = &*ptr;
    match pane.handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
