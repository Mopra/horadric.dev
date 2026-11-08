//! What dictating into another program shows: a small plate by that
//! program's caret with three runes that move with the voice and the end
//! of the words heard so far, as Wispr Flow shows its pill.
//!
//! The caret is the one Windows keeps for the foreground thread
//! (`GetGUIThreadInfo`). Programs that draw their own (browsers, Electron,
//! terminals) keep none, and there the plate goes by the mouse pointer.
//! It is placed once, when the dictation starts: the caret does not move
//! until the words are typed, and a plate that chased the pointer would
//! be a distraction.
//!
//! Its own window, layered so it can fade, topmost, never activated, no
//! taskbar button, and the mouse goes through it.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::time::Instant;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::DirectWrite::{IDWriteTextLayout, DWRITE_WORD_WRAPPING_NO_WRAP};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, InvalidateRect, MonitorFromPoint, ValidateRect, HMONITOR,
    MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetForegroundWindow,
    GetGUIThreadInfo, GetWindowLongPtrW, GetWindowThreadProcessId, KillTimer, RegisterClassW,
    SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW, ShowWindow, CREATESTRUCTW,
    GUITHREADINFO, GWLP_USERDATA, HTTRANSPARENT, LWA_ALPHA, MA_NOACTIVATE, SW_SHOWNOACTIVATE,
    WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_NCCREATE, WM_NCDESTROY, WM_NCHITTEST, WM_PAINT, WM_TIMER,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    WS_POPUP,
};

use super::Shown;
use crate::render::{self, DictationScene, Target};
use crate::window::Shared;

const CLASS: PCWSTR = w!("HoradricDictationPlate");

/// In DIPs: wide enough for a clause, low enough to sit under a line.
const SIZE: (f32, f32) = (380.0, 34.0);
/// Where the words start and end on the plate, in DIPs.
const TEXT_X: (f32, f32) = (67.0, 12.0);
/// Between the caret and the plate, in DIPs.
const GAP: f32 = 6.0;
/// Below the pointer, clear of its arrow, when there is no caret.
const BELOW_POINTER: f32 = 24.0;
const FRAME: usize = 1;
/// The runes' frame, a little over 30 a second.
const FRAME_MS: u32 = 33;
const FADE_IN_MS: f32 = 120.0;
const FADE_OUT_MS: f32 = 180.0;

thread_local! {
    static PLATE: RefCell<Option<Box<Plate>>> = const { RefCell::new(None) };
}

struct Plate {
    hwnd: Cell<HWND>,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    /// The window dictation's notes come to, whose state is shown.
    source: HWND,
    scale: f32,
    since: Instant,
    ticked: Cell<Instant>,
    alpha: Cell<f32>,
    /// Fading out: nothing is left to show.
    leaving: Cell<bool>,
    /// Faded out, to be taken down once the message is handled.
    gone: Cell<bool>,
    /// What is shown and its layout, made again only when it changes.
    text: RefCell<(String, Option<IDWriteTextLayout>)>,
}

/// Shows the plate for the dictation `source` hears, by the caret of the
/// program in front. One at a time: a plate already up stays.
pub fn open(shared: &Rc<Shared>, source: HWND) {
    let up = PLATE.with(|p| {
        p.borrow()
            .as_ref()
            .map(|plate| plate.leaving.set(false))
            .is_some()
    });
    if up {
        return;
    }
    match Plate::open(Rc::clone(shared), source) {
        Ok(plate) => PLATE.with(|p| *p.borrow_mut() = Some(plate)),
        Err(e) => eprintln!("horadric: dictation plate: {e}"),
    }
}

/// Where the plate goes for a dictation starting now, in screen pixels:
/// the caret's rectangle if the program in front has one, else a point
/// rectangle at the pointer.
fn anchor() -> (RECT, bool) {
    unsafe {
        let thread = GetWindowThreadProcessId(GetForegroundWindow(), None);
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if thread != 0
            && GetGUIThreadInfo(thread, &mut info).is_ok()
            && !info.hwndCaret.is_invalid()
        {
            let r = info.rcCaret;
            // A caret of no height is one a program keeps hidden.
            if r.bottom > r.top {
                let mut top = POINT {
                    x: r.left,
                    y: r.top,
                };
                let mut bottom = POINT {
                    x: r.left,
                    y: r.bottom,
                };
                if ClientToScreen(info.hwndCaret, &mut top).as_bool()
                    && ClientToScreen(info.hwndCaret, &mut bottom).as_bool()
                {
                    return (
                        RECT {
                            left: top.x,
                            top: top.y,
                            right: top.x + 1,
                            bottom: bottom.y,
                        },
                        true,
                    );
                }
            }
        }
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        (
            RECT {
                left: p.x,
                top: p.y,
                right: p.x + 1,
                bottom: p.y,
            },
            false,
        )
    }
}

/// Where a plate `size` goes for `anchor`, in screen pixels: its left
/// edge on the anchor's, `gap` under it, or `gap` over it when there is
/// no room under, kept inside the work area `work` (left, top, right,
/// bottom).
fn place(anchor: RECT, size: (i32, i32), work: [i32; 4], gap: i32) -> (i32, i32) {
    let [wl, wt, wr, wb] = work;
    let x = anchor.left.min(wr - size.0).max(wl);
    let under = anchor.bottom + gap;
    let y = if under + size.1 <= wb {
        under
    } else {
        anchor.top - gap - size.1
    };
    (x, y.clamp(wt, (wb - size.1).max(wt)))
}

fn work_area(monitor: HMONITOR) -> [i32; 4] {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let w = info.rcWork;
            [w.left, w.top, w.right, w.bottom]
        } else {
            [0, 0, 1280, 720]
        }
    }
}

fn register_class() {
    thread_local!(static DONE: Cell<bool> = const { Cell::new(false) });
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    unsafe {
        let Ok(instance) = GetModuleHandleW(None) else {
            return;
        };
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassW(&wc);
    }
}

/// What the plate says for `source` now, and whether it is words heard
/// rather than a hint. None once the dictation is over and nothing it
/// left to say is still standing.
fn says(source: HWND) -> Option<(String, bool, Option<Shown>)> {
    let shown = super::shown(source);
    let draft = super::draft(source);
    let (text, heard) = match (shown, draft) {
        (Some(Shown::Listening(_) | Shown::Writing), Some(d)) => (d, true),
        // Not `to_stop`, which names the stage's chord.
        (Some(Shown::Listening(_)), None) => ("Listening. Esc drops it.".to_string(), false),
        (Some(Shown::Writing), None) => ("Writing it down.".to_string(), false),
        (Some(Shown::Fetching(_)), _) | (None, _) => (super::notice(source)?, false),
    };
    Some((text, heard, shown))
}

impl Plate {
    fn open(shared: Rc<Shared>, source: HWND) -> Result<Box<Self>> {
        register_class();
        let (anchor, caret) = anchor();
        let monitor = unsafe {
            MonitorFromPoint(
                POINT {
                    x: anchor.left,
                    y: anchor.bottom,
                },
                MONITOR_DEFAULTTONEAREST,
            )
        };
        let (mut dx, mut dy) = (96u32, 96u32);
        unsafe {
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        }
        let s = dx.max(96) as f32 / 96.0;
        let px = ((SIZE.0 * s).round() as i32, (SIZE.1 * s).round() as i32);
        let gap = if caret { GAP } else { BELOW_POINTER };
        let (x, y) = place(anchor, px, work_area(monitor), (gap * s).round() as i32);
        let plate = Box::new(Plate {
            hwnd: Cell::new(HWND::default()),
            shared,
            target: RefCell::new(None),
            source,
            scale: s,
            since: Instant::now(),
            ticked: Cell::new(Instant::now()),
            alpha: Cell::new(0.0),
            leaving: Cell::new(false),
            gone: Cell::new(false),
            text: RefCell::new((String::new(), None)),
        });
        unsafe {
            // Born where it shows and at its size: a window that starts
            // elsewhere and moves has been seen not to paint.
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW
                    | WS_EX_TOPMOST
                    | WS_EX_NOACTIVATE
                    | WS_EX_TRANSPARENT
                    | WS_EX_LAYERED,
                CLASS,
                w!(""),
                WS_POPUP,
                x,
                y,
                px.0,
                px.1,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*plate as *const Plate as *const c_void),
            )?;
            plate.hwnd.set(hwnd);
            let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            crate::backdrop::border(hwnd, None);
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            SetTimer(Some(hwnd), FRAME, FRAME_MS, None);
        }
        Ok(plate)
    }

    fn set_alpha(&self, a: f32) {
        self.alpha.set(a.clamp(0.0, 1.0));
        let byte = (self.alpha.get() * 255.0).round() as u8;
        unsafe {
            let _ = SetLayeredWindowAttributes(self.hwnd.get(), COLORREF(0), byte, LWA_ALPHA);
        }
    }

    /// One frame: the fade, and the runes moved on. False once it has
    /// faded out and should go.
    fn tick(&self) -> bool {
        let now = Instant::now();
        let dt = (now.duration_since(self.ticked.replace(now)).as_secs_f32() * 1000.0).min(50.0);
        if says(self.source).is_none() {
            self.leaving.set(true);
        }
        if self.leaving.get() {
            self.set_alpha(self.alpha.get() - dt / FADE_OUT_MS);
            if self.alpha.get() <= 0.0 {
                return false;
            }
        } else if self.alpha.get() < 1.0 {
            self.set_alpha(self.alpha.get() + dt / FADE_IN_MS);
        }
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd.get()), None, false);
        }
        true
    }

    /// `text` laid out on one line. Words heard lose words from their
    /// front until they fit, so the newest are the ones in sight; a hint
    /// is cut by the plate's edge, its start being what matters.
    fn layout(&self, text: &str, heard: bool) -> Option<IDWriteTextLayout> {
        let gpu = &self.shared.gpu;
        let room = SIZE.0 - TEXT_X.0 - TEXT_X.1;
        let mut from = 0;
        loop {
            let shown = if from == 0 {
                text.to_string()
            } else {
                format!("…{}", &text[from..])
            };
            let l = render::wrapped(gpu, &gpu.small, &shown, room).ok()?;
            unsafe {
                let _ = l.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            }
            let next = text[from..].find(' ').map(|i| from + i + 1);
            match next {
                Some(n) if heard && render::text_size(&l).0 > room => from = n,
                _ => return Some(l),
            }
        }
    }

    fn paint(&self) {
        let Some((text, heard, shown)) = says(self.source) else {
            return;
        };
        let mut kept = self.text.borrow_mut();
        if kept.0 != text || kept.1.is_none() {
            *kept = (text.clone(), self.layout(&text, heard));
        }
        let Some(layout) = kept.1.as_ref() else {
            return;
        };
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            let s = self.scale;
            let (w, h) = ((SIZE.0 * s).round() as u32, (SIZE.1 * s).round() as u32);
            match Target::new(
                &self.shared.gpu,
                self.hwnd.get(),
                w,
                h,
                (s * 96.0).round() as u32,
            ) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for the dictation plate: {e}");
                    return;
                }
            }
        }
        let scene = DictationScene {
            size: SIZE,
            text: layout,
            heard,
            level: match shown {
                Some(Shown::Listening(l)) => l,
                _ => 0.0,
            },
            writing: matches!(shown, Some(Shown::Writing | Shown::Fetching(_))),
            // Held still when the human turned animations off.
            t: if crate::backdrop::animations_on() {
                self.since.elapsed().as_secs_f32()
            } else {
                0.0
            },
        };
        let failed = slot
            .as_ref()
            .map(|t| t.draw_dictation(&self.shared.metrics, &scene))
            .is_some_and(|r| r.is_err());
        if failed {
            *slot = None;
        }
    }

    fn handle(&self, msg: u32, wparam: WPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                self.paint();
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd.get()), None);
                }
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_TIMER if wparam.0 == FRAME => {
                if !self.tick() {
                    unsafe {
                        let _ = KillTimer(Some(self.hwnd.get()), FRAME);
                    }
                    self.gone.set(true);
                }
                Some(LRESULT(0))
            }
            WM_NCHITTEST => Some(LRESULT(HTTRANSPARENT as isize)),
            WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
            _ => None,
        }
    }
}

/// Takes the plate down at once.
fn close() {
    let plate = PLATE.with(|p| p.borrow_mut().take());
    if let Some(plate) = plate {
        unsafe {
            let _ = DestroyWindow(plate.hwnd.get());
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Plate;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let (r, gone) = {
        let plate = &*ptr;
        (plate.handle(msg, wparam), plate.gone.get())
    };
    // The Box lives in PLATE, so it is dropped only here, with no
    // reference to it left.
    if gone {
        close();
    }
    match r {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caret(left: i32, top: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right: left + 1,
            bottom,
        }
    }

    #[test]
    fn the_plate_sits_under_the_caret_or_over_it_at_the_bottom() {
        let work = [0, 0, 1920, 1040];
        assert_eq!(place(caret(100, 200, 220), (380, 34), work, 6), (100, 226));
        // No room under: over the caret's top, not over its line.
        assert_eq!(
            place(caret(100, 1000, 1020), (380, 34), work, 6),
            (100, 960)
        );
        // Kept on the screen at the right edge.
        assert_eq!(place(caret(1800, 200, 220), (380, 34), work, 6).0, 1540);
    }
}
