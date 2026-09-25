//! A question the app asks in words: a session's name, a host, a new task
//! and its notes.
//!
//! Drawn like the rest of the app, on a plate of its own beside the
//! cluster it was asked from, rather than as a Windows dialog. Enter
//! answers, Esc and a click anywhere else cancel. Like the menus it runs a
//! modal loop until it is answered, so the caller must not hold anything
//! the message handlers need. It holds the mouse while open, as the
//! setting lists do, so it hears the click outside that cancels it.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::DirectWrite::IDWriteTextLayout;
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MonitorFromRect, ValidateRect, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, GetKeyState, ReleaseCapture, SetCapture, VIRTUAL_KEY, VK_BACK, VK_CONTROL,
    VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB,
    VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCaretBlinkTime,
    GetClientRect, GetCursorPos, GetForegroundWindow, GetMessageW, GetWindowLongPtrW,
    GetWindowRect, IsWindow, IsWindowVisible, KillTimer, LoadCursorW, PostQuitMessage,
    RegisterClassW, SetCursor, SetForegroundWindow, SetTimer, SetWindowLongPtrW, ShowWindow,
    TranslateMessage, CREATESTRUCTW, CS_DBLCLKS, GWLP_USERDATA, IDC_ARROW, IDC_IBEAM, MSG, SW_HIDE,
    SW_SHOW, WA_INACTIVE, WM_ACTIVATE, WM_CAPTURECHANGED, WM_CHAR, WM_CLOSE, WM_ERASEBKGND,
    WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MOUSEMOVE,
    WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_RBUTTONDOWN, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use crate::backdrop;
use crate::clipboard;
use crate::field::{self, Field};
use crate::layout::{self, AskLayout, Rect};
use crate::render::{self, AskScene, FieldLook, Target};
use crate::window::Shared;

pub(crate) const CLASS: PCWSTR = w!("HoradricAsk");

/// Longer than any name worth reading on a tile.
const MAX_LEN: usize = 200;
/// Notes go to the agent with the task, so they may run to a few
/// paragraphs.
const MAX_NOTES: usize = 4000;
/// Between the cluster and the input, in DIPs.
const GAP: f32 = 8.0;
const BLINK: usize = 1;

/// What to ask.
pub struct Ask<'a> {
    pub title: &'a str,
    pub prompt: &'a str,
    /// Filled in and selected, so typing replaces it.
    pub initial: &'a str,
    /// Shown faintly in the field while it is empty.
    pub placeholder: &'a str,
    /// What Enter does, for the line under the field: "rename", "add".
    pub verb: &'a str,
    /// Ask for notes under it too, as a new task takes.
    pub notes: bool,
}

/// What was answered. `notes` is empty unless asked for.
pub struct Answer {
    pub text: String,
    pub notes: String,
}

pub fn register_class() -> Result<()> {
    unsafe {
        let wc = WNDCLASSW {
            style: CS_DBLCLKS,
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

/// Asks beside `beside`, the cluster it was asked from, at the height of
/// the mouse. With no cluster on screen it goes by the mouse alone. None
/// when cancelled.
pub fn ask(shared: Rc<Shared>, beside: Option<HWND>, a: &Ask) -> Option<Answer> {
    let popup = Popup::open(shared, beside, a)
        .map_err(|e| eprintln!("horadric: cannot ask {}: {e}", a.title))
        .ok()?;
    let mut msg = MSG::default();
    while popup.outcome.get().is_none() {
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 == 0 {
            // The app is quitting: the main loop has to see it too.
            unsafe { PostQuitMessage(msg.wParam.0 as i32) };
            break;
        }
        if got.0 == -1 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    popup.close(false);
    let answered = popup.outcome.get() == Some(true);
    unsafe {
        let _ = DestroyWindow(popup.hwnd.get());
    }
    let mut fields = popup.fields.take().into_iter();
    answered.then(|| Answer {
        text: fields.next().map(|f| f.text).unwrap_or_default(),
        notes: fields.next().map(|f| f.text).unwrap_or_default(),
    })
}

struct Popup {
    hwnd: Cell<HWND>,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    layout: AskLayout,
    title: String,
    prompt: IDWriteTextLayout,
    placeholder: String,
    hint: String,
    fields: RefCell<Vec<Field>>,
    /// How far each field's text is scrolled, across and down.
    scroll: RefCell<Vec<(f32, f32)>>,
    /// The field with the keyboard.
    focus: Cell<usize>,
    /// The caret's blink is on.
    lit: Cell<bool>,
    /// The left button went down in a field and is still held.
    dragging: Cell<bool>,
    /// The first half of a character typed as a surrogate pair.
    high: Cell<Option<u16>>,
    /// The window that had the focus before, which gets it back.
    before: HWND,
    /// Some once closed, true when answered.
    outcome: Cell<Option<bool>>,
}

impl Popup {
    fn open(shared: Rc<Shared>, beside: Option<HWND>, a: &Ask) -> Result<Box<Self>> {
        let beside = beside.filter(|h| unsafe { IsWindowVisible(*h) }.as_bool());
        let dpi = match beside {
            Some(h) => unsafe { GetDpiForWindow(h) },
            None => unsafe { GetDpiForSystem() },
        }
        .max(96);
        let s = dpi as f32 / 96.0;
        let gpu = &shared.gpu;
        let prompt = render::wrapped(gpu, &gpu.small, a.prompt, layout::ask_text_w())?;
        let layout = layout::ask(render::text_size(&prompt).1.ceil(), a.notes);
        let mut fields = vec![Field::new(a.initial, false, MAX_LEN)];
        if a.notes {
            fields.push(Field::new("", true, MAX_NOTES));
        }
        let hint = if a.notes {
            format!(
                "Enter to {}, Shift+Enter for a new line, Esc to cancel",
                a.verb
            )
        } else {
            format!("Enter to {}, Esc to cancel", a.verb)
        };

        let mut cursor = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut cursor);
        }
        let mut owner = RECT {
            left: cursor.x,
            top: cursor.y,
            right: cursor.x,
            bottom: cursor.y,
        };
        if let Some(h) = beside {
            unsafe {
                let _ = GetWindowRect(h, &mut owner);
            }
        }
        let size = (
            (layout.size.0 * s).round() as i32,
            (layout.size.1 * s).round() as i32,
        );
        let gap = (GAP * s).round() as i32;
        let (x, y) = layout::ask_place(
            [owner.left, owner.top, owner.right, owner.bottom],
            cursor.y,
            size,
            gap,
            work_area(&owner),
        );

        let n = fields.len();
        let popup = Box::new(Popup {
            hwnd: Cell::new(HWND::default()),
            shared,
            target: RefCell::new(None),
            layout,
            title: a.title.to_string(),
            prompt,
            placeholder: a.placeholder.to_string(),
            hint,
            fields: RefCell::new(fields),
            scroll: RefCell::new(vec![(0.0, 0.0); n]),
            focus: Cell::new(0),
            lit: Cell::new(true),
            dragging: Cell::new(false),
            high: Cell::new(None),
            before: unsafe { GetForegroundWindow() },
            outcome: Cell::new(None),
        });
        let title: Vec<u16> = a.title.encode_utf16().chain([0]).collect();
        unsafe {
            // Born where it shows and at its size: a window that starts
            // elsewhere and moves has been seen not to paint.
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                CLASS,
                PCWSTR(title.as_ptr()),
                WS_POPUP,
                x,
                y,
                size.0,
                size.1,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*popup as *const Popup as *const c_void),
            )?;
            popup.hwnd.set(hwnd);
            let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            backdrop::border(hwnd, None);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            SetCapture(hwnd);
            SetTimer(Some(hwnd), BLINK, GetCaretBlinkTime(), None);
        }
        Ok(popup)
    }

    fn scale(&self) -> f32 {
        unsafe { GetDpiForWindow(self.hwnd.get()) }.max(96) as f32 / 96.0
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, false);
        }
    }

    fn field_rect(&self, i: usize) -> Rect {
        match (i, self.layout.notes) {
            (1, Some(r)) => r,
            _ => self.layout.field,
        }
    }

    /// Field `i`'s text laid out as it is drawn.
    fn text_layout(&self, i: usize, f: &Field) -> Option<IDWriteTextLayout> {
        let inner = layout::ask_inner(&self.field_rect(i));
        render::field_layout(&self.shared.gpu, &f.text, &inner, f.multiline).ok()
    }

    fn paint(&self) {
        let hwnd = self.hwnd.get();
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(hwnd, &mut r);
        }
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            let (w, h) = (r.right as u32, r.bottom as u32);
            match Target::new(&self.shared.gpu, hwnd, w, h, dpi) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for a question: {e}");
                    return;
                }
            }
        }
        let fields = self.fields.borrow();
        let mut scroll = self.scroll.borrow_mut();
        let texts: Vec<Option<IDWriteTextLayout>> = fields
            .iter()
            .enumerate()
            .map(|(i, f)| self.text_layout(i, f))
            .collect();
        let mut looks = Vec::new();
        for (i, (f, text)) in fields.iter().zip(&texts).enumerate() {
            let Some(text) = text else { continue };
            let rect = self.field_rect(i);
            let inner = layout::ask_inner(&rect);
            let caret = render::caret_at(text, field::utf16_at(&f.text, f.caret));
            let (w, h) = render::text_size(text);
            let sc = &mut scroll[i];
            if f.multiline {
                sc.1 = field::follow(sc.1, caret.y, caret.h, inner.h, h);
            } else {
                sc.0 = field::follow(sc.0, caret.x, 2.0, inner.w, w + 2.0);
            }
            let (a, b) = f.selection();
            let (a, b) = (field::utf16_at(&f.text, a), field::utf16_at(&f.text, b));
            let focused = i == self.focus.get();
            looks.push(FieldLook {
                rect,
                text,
                placeholder: (f.text.is_empty() && i == 0 && !self.placeholder.is_empty())
                    .then_some(self.placeholder.as_str()),
                multiline: f.multiline,
                scroll: *sc,
                selection: render::range_rects(text, a, b - a),
                caret: (focused && self.lit.get()).then_some(caret),
                focused,
            });
        }
        let scene = AskScene {
            layout: &self.layout,
            title: &self.title,
            prompt: &self.prompt,
            notes_label: "NOTES FOR THE AGENT",
            hint: &self.hint,
            fields: looks,
        };
        let result = slot
            .as_ref()
            .map(|t| t.draw_ask(&self.shared.gpu, &self.shared.metrics, &scene));
        if let Some(Err(_)) = result {
            *slot = None;
        }
    }

    /// A point in client pixels, in DIPs.
    fn point(&self, lparam: LPARAM) -> (f32, f32) {
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32 / s;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
        (x, y)
    }

    fn inside(&self, (x, y): (f32, f32)) -> bool {
        let (w, h) = self.layout.size;
        x >= 0.0 && y >= 0.0 && x < w && y < h
    }

    /// The byte offset in field `i` nearest a point in DIPs.
    fn offset_at(&self, i: usize, (x, y): (f32, f32)) -> usize {
        let fields = self.fields.borrow();
        let f = &fields[i];
        let Some(text) = self.text_layout(i, f) else {
            return f.caret;
        };
        let inner = layout::ask_inner(&self.field_rect(i));
        let (sx, sy) = self.scroll.borrow()[i];
        let at = render::offset_at(&text, x - inner.x + sx, y - inner.y + sy);
        field::byte_at(&f.text, at)
    }

    /// Something changed: the caret shows at once and starts its blink
    /// over.
    fn changed(&self) {
        self.lit.set(true);
        unsafe {
            SetTimer(Some(self.hwnd.get()), BLINK, GetCaretBlinkTime(), None);
        }
        self.invalidate();
    }

    fn edit(&self, f: impl FnOnce(&mut Field)) {
        let i = self.focus.get();
        if let Some(field) = self.fields.borrow_mut().get_mut(i) {
            f(field);
        }
        self.changed();
    }

    /// Up or down a line of the notes, or to either end of one line.
    fn line(&self, down: bool, extend: bool) {
        let i = self.focus.get();
        let multiline = self.fields.borrow()[i].multiline;
        if !multiline {
            return self.edit(|f| {
                if down {
                    f.end(true, extend)
                } else {
                    f.home(true, extend)
                }
            });
        }
        let at = {
            let fields = self.fields.borrow();
            let f = &fields[i];
            let Some(text) = self.text_layout(i, f) else {
                return;
            };
            let c = render::caret_at(&text, field::utf16_at(&f.text, f.caret));
            let y = if down {
                c.y + c.h * 1.5
            } else {
                c.y - c.h * 0.5
            };
            if y < 0.0 {
                0
            } else {
                field::byte_at(&f.text, render::offset_at(&text, c.x, y))
            }
        };
        self.edit(|f| f.set_caret(at, extend));
    }

    fn key(&self, vk: u16) {
        let down = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) } < 0;
        let (ctrl, shift) = (down(VK_CONTROL), down(VK_SHIFT));
        let multiline = self.fields.borrow()[self.focus.get()].multiline;
        match VIRTUAL_KEY(vk) {
            VK_ESCAPE => self.close(false),
            VK_RETURN if multiline && shift => self.edit(|f| f.insert("\n")),
            VK_RETURN => self.close(true),
            VK_TAB => {
                let n = self.fields.borrow().len();
                if n > 1 {
                    let next = if shift {
                        (self.focus.get() + n - 1) % n
                    } else {
                        (self.focus.get() + 1) % n
                    };
                    self.focus.set(next);
                    self.edit(Field::select_all);
                }
            }
            VK_LEFT => self.edit(|f| f.left(ctrl, shift)),
            VK_RIGHT => self.edit(|f| f.right(ctrl, shift)),
            VK_HOME => self.edit(|f| f.home(ctrl || !f.multiline, shift)),
            VK_END => self.edit(|f| f.end(ctrl || !f.multiline, shift)),
            VK_UP => self.line(false, shift),
            VK_DOWN => self.line(true, shift),
            VK_BACK => self.edit(|f| f.backspace(ctrl)),
            VK_DELETE if shift => self.edit(|f| clipboard::set_text(&f.cut())),
            VK_DELETE => self.edit(|f| f.delete(ctrl)),
            _ if !ctrl => {}
            k => match char::from_u32(k.0 as u32) {
                Some('A') => self.edit(Field::select_all),
                Some('C') => {
                    let fields = self.fields.borrow();
                    let s = fields[self.focus.get()].selected();
                    if !s.is_empty() {
                        clipboard::set_text(s);
                    }
                }
                Some('X') => self.edit(|f| {
                    if f.caret != f.anchor {
                        clipboard::set_text(&f.cut());
                    }
                }),
                Some('V') => {
                    if let Some(s) = clipboard::get_text() {
                        self.edit(|f| f.insert(&s));
                    }
                }
                _ => {}
            },
        }
    }

    /// A character typed. The keys that edit came as key downs, so every
    /// control character here is dropped.
    fn typed(&self, unit: u16) {
        let s = match unit {
            0xD800..=0xDBFF => {
                self.high.set(Some(unit));
                return;
            }
            0xDC00..=0xDFFF => match self.high.take() {
                Some(h) => String::from_utf16_lossy(&[h, unit]),
                None => return,
            },
            u if u < 0x20 || u == 0x7F => return,
            u => String::from_utf16_lossy(&[u]),
        };
        self.edit(|f| f.insert(&s));
    }

    /// Hands the focus back and ends the loop. `answer` is whether Enter
    /// closed it.
    fn close(&self, answer: bool) {
        if self.outcome.get().is_some() {
            return;
        }
        self.outcome.set(Some(answer));
        let hwnd = self.hwnd.get();
        unsafe {
            let _ = KillTimer(Some(hwnd), BLINK);
            if GetCapture() == hwnd {
                let _ = ReleaseCapture();
            }
            // Before hiding: hiding the active window hands the focus to
            // whatever Windows picks.
            if !self.before.is_invalid() && IsWindow(Some(self.before)).as_bool() {
                let _ = SetForegroundWindow(self.before);
            }
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }

    fn handle(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                self.paint();
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd.get()), None);
                }
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_TIMER if wparam.0 == BLINK => {
                self.lit.set(!self.lit.get());
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                let p = self.point(lparam);
                // The mouse is held, so no WM_SETCURSOR comes.
                let over = layout::ask_hit(&self.layout, p.0, p.1).is_some();
                unsafe {
                    let _ = LoadCursorW(None, if over { IDC_IBEAM } else { IDC_ARROW })
                        .map(|c| SetCursor(Some(c)));
                }
                if self.dragging.get() {
                    let at = self.offset_at(self.focus.get(), p);
                    self.edit(|f| f.set_caret(at, true));
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                let p = self.point(lparam);
                if !self.inside(p) {
                    self.close(false);
                    return Some(LRESULT(0));
                }
                if let Some(i) = layout::ask_hit(&self.layout, p.0, p.1) {
                    let at = self.offset_at(i, p);
                    let extend = i == self.focus.get() && (wparam.0 & 0x4) != 0;
                    self.focus.set(i);
                    if msg == WM_LBUTTONDBLCLK {
                        self.edit(|f| f.select_word(at));
                    } else {
                        self.dragging.set(true);
                        self.edit(|f| f.set_caret(at, extend));
                    }
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                self.dragging.set(false);
                Some(LRESULT(0))
            }
            WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
                if !self.inside(self.point(lparam)) {
                    self.close(false);
                }
                Some(LRESULT(0))
            }
            WM_KEYDOWN => {
                self.key(wparam.0 as u16);
                Some(LRESULT(0))
            }
            WM_CHAR => {
                self.typed(wparam.0 as u16);
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                self.close(false);
                Some(LRESULT(0))
            }
            WM_ACTIVATE => {
                if (wparam.0 & 0xffff) as u32 == WA_INACTIVE {
                    self.close(false);
                }
                None
            }
            WM_CAPTURECHANGED => {
                // Someone else took the mouse: the input would no longer
                // hear the click that cancels it.
                if HWND(lparam.0 as *mut c_void) != self.hwnd.get() {
                    self.close(false);
                }
                None
            }
            _ => None,
        }
    }
}

/// The work area of the screen `r` is on, as left, top, right, bottom.
fn work_area(r: &RECT) -> [i32; 4] {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        let monitor = MonitorFromRect(r, MONITOR_DEFAULTTONEAREST);
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let w = info.rcWork;
            [w.left, w.top, w.right, w.bottom]
        } else {
            [i32::MIN / 2, i32::MIN / 2, i32::MAX / 2, i32::MAX / 2]
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Popup;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The Box lives in `ask` until after the window is destroyed.
    let popup = &*ptr;
    match popup.handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
