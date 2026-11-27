//! A question the app asks with buttons: quit and keep the sessions going,
//! end them, merge a branch, or an error there is nothing to do about but
//! read.
//!
//! Drawn like the rest of the app, on a plate in the middle of the screen
//! under the mouse, rather than as a Windows message box. Each answer is a
//! key named for what it does, not Yes and No. Enter presses the ringed
//! one, the arrows and Tab move the ring, and Esc or a click anywhere else
//! answers nothing. Like the menus it runs a modal loop until answered, so
//! the caller must not hold anything the message handlers need.
//!
//! A question about another program, such as a session's desktop action,
//! is put aside instead ([`show_aside`]): in a corner of the screen clear
//! of that program's windows, without taking the focus or the mouse, and
//! it stays until a button is pressed or its time runs out. The program
//! and its own dialogs stay readable and clickable meanwhile, by the
//! human and by the session's tools. It is always on top while it waits:
//! it has no taskbar button and is not in Alt+Tab, so one that the
//! program came to cover could not be found again, and for a maximised
//! program every corner is over the program.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::time::Duration;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::DirectWrite::IDWriteTextLayout;
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MonitorFromPoint, ValidateRect, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, GetKeyState, ReleaseCapture, SetCapture, VIRTUAL_KEY, VK_ESCAPE, VK_LEFT,
    VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetForegroundWindow, GetMessageW, GetWindowLongPtrW, IsWindow, LoadCursorW, PostMessageW,
    PostQuitMessage, RegisterClassW, SetForegroundWindow, SetTimer, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, TranslateMessage, CREATESTRUCTW, GWLP_USERDATA, HWND_TOPMOST,
    IDC_ARROW, MSG, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOW, SW_SHOWNOACTIVATE,
    WA_INACTIVE, WM_ACTIVATE, WM_CAPTURECHANGED, WM_CLOSE, WM_ERASEBKGND, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY,
    WM_PAINT, WM_RBUTTONDOWN, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::appear;
use crate::backdrop;
use crate::layout::{self, DialogHit, DialogLayout, Metrics};
use crate::render::{self, DialogScene, Gpu, Target};
use crate::theme::{self, Color};

pub(crate) const CLASS: PCWSTR = w!("HoradricDialog");

/// The timer that ends a question put aside when nobody answers.
const NO_ANSWER_TIMER: usize = 0x4e41;

/// How a question about another program is put.
pub struct Aside {
    /// That program's windows, as left, top, right, bottom in screen
    /// pixels, which the question keeps clear of when the screen allows.
    pub avoid: Option<[i32; 4]>,
    /// How long it waits for an answer.
    pub within: Duration,
}

/// What became of a question put aside.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Pressed(usize),
    /// Put away by the human: Esc, or its window closed.
    Dismissed,
    /// Nobody answered in time.
    Unanswered,
    /// Not asked: its text would run off the screen even at the widest.
    TooTall,
}

/// What kind of question, for the lamp by its title.
#[derive(Clone, Copy)]
pub enum Tone {
    Question,
    /// Something will stop or be lost.
    Warning,
    Error,
}

impl Tone {
    fn colour(self) -> Color {
        match self {
            Tone::Question => theme::working(),
            Tone::Warning => theme::waiting(),
            Tone::Error => theme::error(),
        }
    }
}

pub struct Dialog<'a> {
    pub tone: Tone,
    pub title: &'a str,
    pub text: &'a str,
    /// Left to right. Each says what it does.
    pub buttons: &'a [&'a str],
    /// The one Enter presses at first.
    pub default: usize,
    /// A check beside the buttons, such as "Do not ask again", and where
    /// it is kept: ticked as it was, and as the human left it after.
    pub check: Option<(&'a str, &'a Cell<bool>)>,
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

/// How wide the text of a question put aside wraps: the usual width, or
/// for a text too tall for its screen at that, wider. None when it is too
/// tall even then: a question whose text runs off the screen must not be
/// answered, since some of what it asks would go unread.
fn aside_text_w(gpu: &Gpu, d: &Dialog, aside: &Aside) -> Result<Option<f32>> {
    let (at, s) = screen_of(Some(aside));
    let [l, t, r, b] = work_area(at);
    let (work_w, work_h) = ((r - l) as f32 / s, (b - t) as f32 / s);
    // Less the gap a question aside keeps from the screen's edges.
    let room = work_h - 32.0 - layout::dialog_chrome_h();
    let fits = |w: f32| -> Result<bool> {
        let text = render::wrapped(gpu, &gpu.body, d.text, w)?;
        Ok(render::text_size(&text).1.ceil() <= room)
    };
    let usual = layout::dialog_text_w();
    if fits(usual)? {
        return Ok(Some(usual));
    }
    let widest = (work_w - 32.0).clamp(layout::DIALOG_W, layout::DIALOG_W_MOST);
    let wide = layout::dialog_text_w_of(widest);
    Ok(fits(wide)?.then_some(wide))
}

/// The point whose screen a question goes on, and that screen's scale.
fn screen_of(aside: Option<&Aside>) -> (POINT, f32) {
    let mut cursor = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut cursor);
    }
    // Aside, it goes on the screen of the program it is about.
    if let Some([l, t, r, b]) = aside.and_then(|a| a.avoid) {
        cursor = POINT {
            x: l + (r - l) / 2,
            y: t + (b - t) / 2,
        };
    }
    let monitor = unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) };
    let (mut dx, mut dy) = (96u32, 96u32);
    unsafe {
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    }
    (cursor, dx.max(96) as f32 / 96.0)
}

/// Asks, and returns the button pressed. None when dismissed.
pub fn show(gpu: &Gpu, metrics: &Metrics, d: &Dialog) -> Option<usize> {
    let popup = Popup::open(gpu, metrics, d, None, layout::dialog_text_w())
        .map_err(|e| eprintln!("horadric: cannot ask \"{}\": {e}", d.title))
        .ok()?;
    run(&popup, d)
}

/// Asks about another program, aside: see the module's notes.
pub fn show_aside(gpu: &Gpu, metrics: &Metrics, d: &Dialog, aside: &Aside) -> Answer {
    let text_w = match aside_text_w(gpu, d, aside) {
        Ok(Some(w)) => w,
        Ok(None) => return Answer::TooTall,
        Err(e) => {
            eprintln!("horadric: cannot ask \"{}\": {e}", d.title);
            return Answer::Unanswered;
        }
    };
    let Ok(popup) = Popup::open(gpu, metrics, d, Some(aside), text_w)
        .map_err(|e| eprintln!("horadric: cannot ask \"{}\": {e}", d.title))
    else {
        return Answer::Unanswered;
    };
    match run(&popup, d) {
        Some(i) => Answer::Pressed(i),
        None if popup.no_answer.get() => Answer::Unanswered,
        None => Answer::Dismissed,
    }
}

thread_local! {
    /// The question on screen that [`release_aside`] may put away.
    static RELEASABLE: Cell<Option<HWND>> = const { Cell::new(None) };
    /// It was put away that way.
    static RELEASED: Cell<bool> = const { Cell::new(false) };
}

/// As [`show_aside`], for a caller that can decide the call again: None
/// when [`release_aside`] put the question away unanswered, which is
/// neither a yes nor a no. The caller then decides the call it asked
/// about afresh, as if it had just come.
pub fn show_aside_releasable(
    gpu: &Gpu,
    metrics: &Metrics,
    d: &Dialog,
    aside: &Aside,
) -> Option<Answer> {
    let text_w = match aside_text_w(gpu, d, aside) {
        Ok(Some(w)) => w,
        Ok(None) => return Some(Answer::TooTall),
        Err(e) => {
            eprintln!("horadric: cannot ask \"{}\": {e}", d.title);
            return Some(Answer::Unanswered);
        }
    };
    let Ok(popup) = Popup::open(gpu, metrics, d, Some(aside), text_w)
        .map_err(|e| eprintln!("horadric: cannot ask \"{}\": {e}", d.title))
    else {
        return Some(Answer::Unanswered);
    };
    RELEASED.set(false);
    RELEASABLE.set(Some(popup.hwnd.get()));
    let pressed = run(&popup, d);
    RELEASABLE.set(None);
    let released = RELEASED.replace(false);
    match pressed {
        Some(i) => Some(Answer::Pressed(i)),
        None if released => None,
        None if popup.no_answer.get() => Some(Answer::Unanswered),
        None => Some(Answer::Dismissed),
    }
}

/// Puts away the question [`show_aside_releasable`] has on screen, as
/// neither answered nor refused: what it asked is no longer a question,
/// since the human turned bypass on. False when no such question shows,
/// and one put with [`show_aside`] is left alone, since its caller would
/// read its going as a no.
pub fn release_aside() -> bool {
    let Some(hwnd) = RELEASABLE.get() else {
        return false;
    };
    RELEASED.set(true);
    unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }.is_ok()
}

/// Runs the popup's loop until it is answered, and takes it down.
fn run(popup: &Popup, d: &Dialog) -> Option<usize> {
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
    popup.close(None);
    unsafe {
        let _ = DestroyWindow(popup.hwnd.get());
    }
    if let (Some((_, kept)), Some((_, ticked))) = (d.check, &popup.check) {
        kept.set(ticked.get());
    }
    popup.outcome.get().flatten()
}

/// Says what went wrong, for a process that has no app to draw with, such
/// as `horadricw`. Falls silent only if even Direct2D cannot start.
pub fn error_alone(title: &str, text: &str) {
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    let Ok(gpu) = Gpu::new() else {
        eprintln!("horadric: {title}: {text}");
        return;
    };
    let _ = register_class();
    show(
        &gpu,
        &Metrics::default(),
        &Dialog {
            tone: Tone::Error,
            title,
            text,
            buttons: &["Close"],
            default: 0,
            check: None,
        },
    );
}

struct Popup<'a> {
    hwnd: Cell<HWND>,
    gpu: &'a Gpu,
    metrics: &'a Metrics,
    target: RefCell<Option<Target>>,
    layout: DialogLayout,
    tone: Tone,
    title: String,
    text: IDWriteTextLayout,
    buttons: Vec<String>,
    /// The check's label and whether it is ticked.
    check: Option<(String, Cell<bool>)>,
    focus: Cell<usize>,
    hot: Cell<Option<DialogHit>>,
    pressed: Cell<Option<DialogHit>>,
    /// The window that had the focus before, which gets it back.
    before: HWND,
    /// Some once closed, holding the button pressed if one was.
    outcome: Cell<Option<Option<usize>>>,
    /// Put aside: it neither takes the focus nor goes when it loses it.
    aside: bool,
    /// Its time ran out with nobody answering.
    no_answer: Cell<bool>,
}

impl<'a> Popup<'a> {
    fn open(
        gpu: &'a Gpu,
        metrics: &'a Metrics,
        d: &Dialog,
        aside: Option<&Aside>,
        text_w: f32,
    ) -> Result<Box<Self>> {
        let (cursor, s) = screen_of(aside);
        let text = render::wrapped(gpu, &gpu.body, d.text, text_w)?;
        let widths: Vec<f32> = d
            .buttons
            .iter()
            .map(|b| {
                render::wrapped(gpu, &gpu.small_centre, b, 10_000.0)
                    .map(|l| render::text_size(&l).0)
                    .unwrap_or(60.0)
            })
            .collect();
        let check_w = d.check.map(|(label, _)| {
            render::wrapped(gpu, &gpu.small, label, 10_000.0)
                .map(|l| render::text_size(&l).0)
                .unwrap_or(120.0)
        });
        let layout = layout::dialog_wide(
            layout::dialog_w_of(text_w),
            render::text_size(&text).1.ceil(),
            &widths,
            check_w,
        );
        let size = (
            (layout.size.0 * s).round() as i32,
            (layout.size.1 * s).round() as i32,
        );
        let (x, y) = match aside {
            Some(a) => layout::dialog_aside(size, work_area(cursor), a.avoid),
            None => layout::dialog_place(size, work_area(cursor)),
        };

        let popup = Box::new(Popup {
            hwnd: Cell::new(HWND::default()),
            gpu,
            metrics,
            target: RefCell::new(None),
            layout,
            tone: d.tone,
            title: d.title.to_string(),
            text,
            buttons: d.buttons.iter().map(|b| b.to_string()).collect(),
            check: d
                .check
                .map(|(label, ticked)| (label.to_string(), Cell::new(ticked.get()))),
            focus: Cell::new(d.default.min(d.buttons.len().saturating_sub(1))),
            hot: Cell::new(None),
            pressed: Cell::new(None),
            before: unsafe { GetForegroundWindow() },
            outcome: Cell::new(None),
            aside: aside.is_some(),
            no_answer: Cell::new(false),
        });
        // Aside too it is always on top: its corner keeps it off the
        // program it asks about where the screen allows, and one the
        // program covered could not be brought back, having neither a
        // taskbar button nor a place in Alt+Tab.
        let ex = WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
        let title: Vec<u16> = d.title.encode_utf16().chain([0]).collect();
        unsafe {
            // Born where it shows and at its size: a window that starts
            // elsewhere and moves has been seen not to paint.
            let hwnd = CreateWindowExW(
                ex,
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
            appear::begin(hwnd, appear::DIALOG, (appear::RISE * s).round() as i32);
            match aside {
                Some(a) => {
                    // In front of every window, but without the focus,
                    // so keys and clicks meant for the program still
                    // reach it.
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    let _ = SetWindowPos(
                        hwnd,
                        Some(HWND_TOPMOST),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                    let ms = u32::try_from(a.within.as_millis()).unwrap_or(u32::MAX);
                    SetTimer(Some(hwnd), NO_ANSWER_TIMER, ms.max(1), None);
                }
                None => {
                    let _ = ShowWindow(hwnd, SW_SHOW);
                    let _ = SetForegroundWindow(hwnd);
                    SetCapture(hwnd);
                }
            }
        }
        Ok(popup)
    }

    fn scale(&self) -> f32 {
        unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(self.hwnd.get()) }.max(96) as f32 / 96.0
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, false);
        }
    }

    fn paint(&self) {
        let hwnd = self.hwnd.get();
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            let s = self.scale();
            let (w, h) = (
                (self.layout.size.0 * s).round() as u32,
                (self.layout.size.1 * s).round() as u32,
            );
            match Target::new(self.gpu, hwnd, w, h, (s * 96.0).round() as u32) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for a dialog: {e}");
                    return;
                }
            }
        }
        let scene = DialogScene {
            layout: &self.layout,
            title: &self.title,
            text: &self.text,
            tone: self.tone.colour(),
            buttons: &self.buttons,
            focus: self.focus.get(),
            hot: self.hot.get(),
            pressed: self.pressed.get(),
            check: self.check.as_ref().map(|(l, t)| (l.as_str(), t.get())),
        };
        let failed = slot
            .as_ref()
            .map(|t| t.draw_dialog(self.gpu, self.metrics, &scene))
            .is_some_and(|r| r.is_err());
        if failed {
            *slot = None;
        }
    }

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

    fn hit(&self, lparam: LPARAM) -> Option<DialogHit> {
        let (x, y) = self.point(lparam);
        layout::dialog_hit(&self.layout, x, y)
    }

    fn move_focus(&self, forward: bool) {
        let n = self.buttons.len();
        let f = self.focus.get();
        self.focus.set(if forward {
            (f + 1) % n
        } else {
            (f + n - 1) % n
        });
        self.invalidate();
    }

    /// Hands the focus back and ends the loop, with the button pressed if
    /// one was.
    fn close(&self, pressed: Option<usize>) {
        if self.outcome.get().is_some() {
            return;
        }
        self.outcome.set(Some(pressed));
        let hwnd = self.hwnd.get();
        unsafe {
            if GetCapture() == hwnd {
                let _ = ReleaseCapture();
            }
            // Before hiding: hiding the active window hands the focus to
            // whatever Windows picks. One put aside never took it.
            if !self.aside && !self.before.is_invalid() && IsWindow(Some(self.before)).as_bool() {
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
            WM_MOUSEMOVE => {
                let hot = self.hit(lparam);
                if self.hot.replace(hot) != hot {
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                if self.inside(self.point(lparam)) {
                    let hit = self.hit(lparam);
                    if self.pressed.replace(hit) != hit {
                        self.invalidate();
                    }
                } else {
                    self.close(None);
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                let pressed = self.pressed.take();
                match (pressed, self.hit(lparam)) {
                    (Some(DialogHit::Check), Some(DialogHit::Check)) => {
                        if let Some((_, ticked)) = &self.check {
                            ticked.set(!ticked.get());
                        }
                        self.invalidate();
                    }
                    (Some(p), Some(DialogHit::Button(h))) if p == DialogHit::Button(h) => {
                        self.close(Some(h))
                    }
                    _ => self.invalidate(),
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
                if !self.inside(self.point(lparam)) {
                    self.close(None);
                }
                Some(LRESULT(0))
            }
            WM_KEYDOWN => {
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                match VIRTUAL_KEY(wparam.0 as u16) {
                    VK_ESCAPE => self.close(None),
                    VK_RETURN | VK_SPACE => self.close(Some(self.focus.get())),
                    VK_LEFT => self.move_focus(false),
                    VK_RIGHT => self.move_focus(true),
                    VK_TAB => self.move_focus(!shift),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                self.close(None);
                Some(LRESULT(0))
            }
            // Aside, losing the focus or the mouse is not an answer: the
            // session's own clicks in the program would otherwise refuse
            // its question for it.
            WM_ACTIVATE if self.aside => None,
            WM_CAPTURECHANGED if self.aside => None,
            WM_ACTIVATE => {
                if (wparam.0 & 0xffff) as u32 == WA_INACTIVE {
                    self.close(None);
                }
                None
            }
            WM_CAPTURECHANGED => {
                // Someone else took the mouse: the dialog would no longer
                // hear the click that dismisses it.
                if HWND(lparam.0 as *mut c_void) != self.hwnd.get() {
                    self.close(None);
                }
                None
            }
            WM_TIMER if wparam.0 == NO_ANSWER_TIMER => {
                self.no_answer.set(true);
                self.close(None);
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

/// The work area of the screen `at` is on, as left, top, right, bottom.
fn work_area(at: POINT) -> [i32; 4] {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        let monitor = MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST);
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let w = info.rcWork;
            [w.left, w.top, w.right, w.bottom]
        } else {
            [0, 0, 1280, 720]
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_TIMER && wparam.0 == appear::TIMER {
        appear::tick(hwnd);
        return LRESULT(0);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Popup;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The Box lives in `show` until after the window is destroyed.
    let popup = &*ptr;
    match popup.handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
