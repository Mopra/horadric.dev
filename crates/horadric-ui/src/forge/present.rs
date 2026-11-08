//! Presenting a deck (docs/plan/the-forge.md, Getting it out): the deck's
//! page lent to a borderless window filling a chosen screen, its keys and
//! a clicker going to the deck as they do in the pane, and, when there is
//! a second screen, the presenter's notes there with the slide number and
//! the time since the start.
//!
//! The page cannot go full screen by itself when the host asked (no user
//! gesture), and in the pane it would only fill the pane, so the window is
//! ours. The page is not made again: `web::lend` moves the project's
//! WebViews into the window and `web::give_back` puts them back.

use std::cell::RefCell;
use std::sync::Once;
use std::time::Instant;

use serde_json::{json, Value};
use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2;
use webview2_com::WebMessageReceivedEventHandler;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, ID2D1HwndRenderTarget, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_WORD_WRAPPING_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, EnumDisplayMonitors, GetMonitorInfoW, GetStockObject, InvalidateRect,
    MonitorFromRect, BLACK_BRUSH, HBRUSH, HDC, HMONITOR, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
    PAINTSTRUCT,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_DOWN, VK_ESCAPE, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RIGHT, VK_SPACE, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, KillTimer, LoadCursorW,
    RegisterClassW, SetForegroundWindow, SetTimer, ShowWindow, IDC_ARROW, MONITORINFOF_PRIMARY,
    SW_SHOWMAXIMIZED, SW_SHOWNORMAL, WINDOW_EX_STYLE, WM_CLOSE, WM_ERASEBKGND, WM_KEYDOWN,
    WM_PAINT, WM_SIZE, WM_TIMER, WNDCLASSW, WS_CLIPCHILDREN, WS_EX_APPWINDOW, WS_OVERLAPPEDWINDOW,
    WS_POPUP,
};

use crate::menu::{self, Item};
use crate::screens::{self, Screen};
use crate::web;

const STAGE_CLASS: PCWSTR = w!("HoradricForgePresent");
const NOTES_CLASS: PCWSTR = w!("HoradricForgeNotes");
const TICK: usize = 1;

/// A deck being presented. One at a time: there is one audience.
struct Presenting {
    key: String,
    tab: u64,
    stage: HWND,
    notes: Option<Notes>,
    /// The slide shown, of how many, and its notes, as the page last said.
    slide: (u64, u64, String),
    started: Instant,
}

/// The presenter's window and what it draws with.
struct Notes {
    hwnd: HWND,
    d2d: ID2D1Factory,
    rt: Option<ID2D1HwndRenderTarget>,
    big: IDWriteTextFormat,
    body: IDWriteTextFormat,
    small: IDWriteTextFormat,
    small_right: IDWriteTextFormat,
}

thread_local! {
    static NOW: RefCell<Option<Presenting>> = const { RefCell::new(None) };
}

/// Whether the project's deck is being presented.
pub fn presenting(key: &str) -> bool {
    NOW.with(|n| n.borrow().as_ref().is_some_and(|p| p.key == key))
}

/// Hears the page of a file's tab say a slide changed or presenting
/// started or ended, beside the Forge's own message routing. Only a page of
/// the tab's own host is heard.
pub(super) fn listen(key: &str, id: u64, view: &ICoreWebView2, host: &str) {
    let (owned, host) = (key.to_string(), host.to_ascii_lowercase());
    let said = WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        let source = read(|p| unsafe { args.Source(p) });
        if super::serve::host_of(&source).is_none_or(|h| !h.eq_ignore_ascii_case(&host)) {
            return Ok(());
        }
        let json = read(|p| unsafe { args.WebMessageAsJson(p) });
        if let Ok(msg) = serde_json::from_str::<Value>(&json) {
            heard(&owned, id, &msg);
        }
        Ok(())
    }));
    let mut token = Default::default();
    let _ = unsafe { view.add_WebMessageReceived(&said, &mut token) };
}

fn heard(key: &str, tab: u64, msg: &Value) {
    match msg.get("forge").and_then(Value::as_str) {
        Some("slide") => {
            let n = msg.get("n").and_then(Value::as_u64).unwrap_or(0);
            let of = msg.get("of").and_then(Value::as_u64).unwrap_or(0);
            let notes: String = msg
                .get("notes")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .chars()
                .take(20_000)
                .collect();
            let notes_hwnd = NOW.with(|now| {
                let mut now = now.borrow_mut();
                let p = now.as_mut().filter(|p| p.key == key && p.tab == tab)?;
                p.slide = (n, of, notes);
                p.notes.as_ref().map(|n| n.hwnd)
            });
            if let Some(h) = notes_hwnd {
                unsafe {
                    let _ = InvalidateRect(Some(h), None, false);
                }
            }
        }
        Some("present") => {
            let on = msg.get("on").and_then(Value::as_bool).unwrap_or(false);
            if on && !presenting(key) {
                // The deck's own F key: shown where the pane is.
                start(key, tab, false);
            } else if !on && presenting(key) {
                end(false);
            }
        }
        _ => {}
    }
}

fn register() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let Ok(instance) = GetModuleHandleW(None) else {
            return;
        };
        let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        for (class, proc) in [
            (
                STAGE_CLASS,
                stage_proc as unsafe extern "system" fn(_, _, _, _) -> _,
            ),
            (NOTES_CLASS, notes_proc),
        ] {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(proc),
                hInstance: instance.into(),
                lpszClassName: class,
                hCursor: cursor,
                hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
                ..Default::default()
            };
            RegisterClassW(&wc);
        }
    });
}

/// Every screen plugged in, left to right.
fn monitors() -> Vec<Screen> {
    unsafe extern "system" fn each(
        m: HMONITOR,
        _: HDC,
        _: *mut RECT,
        out: LPARAM,
    ) -> windows::core::BOOL {
        let out = unsafe { &mut *(out.0 as *mut Vec<Screen>) };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetMonitorInfoW(m, &mut info) }.as_bool() {
            let rect = |r: RECT| [r.left, r.top, r.right, r.bottom];
            out.push(Screen {
                name: format!("{:?}", m.0),
                bounds: rect(info.rcMonitor),
                work: rect(info.rcWork),
                primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
        true.into()
    }
    let mut found: Vec<Screen> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(each),
            LPARAM(&mut found as *mut Vec<Screen> as isize),
        );
    }
    screens::in_order(found)
}

/// The screen the project's pane is on, by its bounds.
fn stage_screen(key: &str, all: &[Screen]) -> Option<usize> {
    let r = web::pane_rect(key)?;
    let m = unsafe { MonitorFromRect(&r, MONITOR_DEFAULTTOPRIMARY) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetMonitorInfoW(m, &mut info) }
        .as_bool()
        .then_some(())?;
    let b = info.rcMonitor;
    all.iter()
        .position(|s| s.bounds == [b.left, b.top, b.right, b.bottom])
}

/// Presents the deck in the tab `tab`, the project's shown one, full screen:
/// on a screen asked for when `choose` and there are several, else on the
/// pane's.
pub fn start(key: &str, tab: u64, choose: bool) {
    if NOW.with(|n| n.borrow().is_some()) {
        end(true);
    }
    if web::shown_tab(key) != Some(tab) {
        web::select_quiet(key, tab);
    }
    let all = monitors();
    if all.is_empty() {
        return;
    }
    let here = stage_screen(key, &all).unwrap_or(0);
    let on = if choose && all.len() > 1 {
        // Most often a projector or a second screen: the one the stage is
        // not on is offered first.
        let guess = (0..all.len()).find(|&i| i != here).unwrap_or(here);
        let items: Vec<Item> = all
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let item = Item::action(i + 1, screens::label(i + 1, s));
                if i == guess {
                    item.default()
                } else {
                    item
                }
            })
            .collect();
        match menu::popup(&items) {
            Some(n) if n >= 1 && n <= all.len() => n - 1,
            _ => return,
        }
    } else {
        here
    };
    register();
    let [l, t, r, b] = all[on].bounds;
    let stage = unsafe {
        CreateWindowExW(
            WS_EX_APPWINDOW,
            STAGE_CLASS,
            w!("Presenting"),
            WS_POPUP | WS_CLIPCHILDREN,
            l,
            t,
            r - l,
            b - t,
            None,
            None,
            GetModuleHandleW(None).ok().map(|h| h.into()),
            None,
        )
    };
    let Ok(stage) = stage else { return };
    unsafe {
        let _ = ShowWindow(stage, SW_SHOWNORMAL);
    }
    let bounds = RECT {
        left: 0,
        top: 0,
        right: r - l,
        bottom: b - t,
    };
    if !web::lend(key, stage, bounds) {
        unsafe {
            let _ = DestroyWindow(stage);
        }
        return;
    }
    // The stage's screen holds the notes when it is not the audience's;
    // otherwise any other.
    let notes_on = if here != on {
        Some(here)
    } else {
        (0..all.len()).find(|&i| i != on)
    };
    let notes = notes_on.and_then(|i| Notes::open(&all[i]));
    NOW.with(|n| {
        *n.borrow_mut() = Some(Presenting {
            key: key.to_string(),
            tab,
            stage,
            notes,
            slide: (0, 0, String::new()),
            started: Instant::now(),
        })
    });
    super::post(key, tab, &json!({"forge": "present", "on": true}));
    // The notes window took the front as it opened; the clicker's keys
    // belong to the deck.
    unsafe {
        let _ = SetForegroundWindow(stage);
    }
    web::focus(key);
}

/// Ends presenting: the page back in its pane, the windows gone. `tell`
/// says the page must be told, when it was not the page that ended it.
pub fn end(tell: bool) {
    let Some(p) = NOW.with(|n| n.borrow_mut().take()) else {
        return;
    };
    if tell {
        super::post(&p.key, p.tab, &json!({"forge": "present", "on": false}));
    }
    web::give_back(&p.key);
    unsafe {
        if let Some(n) = &p.notes {
            let _ = KillTimer(Some(n.hwnd), TICK);
            let _ = DestroyWindow(n.hwnd);
        }
        let _ = DestroyWindow(p.stage);
    }
}

/// A presenter's key, from either window: a step through the slides, or
/// the end.
fn key_down(vk: u16) -> bool {
    let step: i64 = match vk {
        k if k == VK_ESCAPE.0 => {
            end(true);
            return true;
        }
        k if [VK_RIGHT.0, VK_DOWN.0, VK_NEXT.0, VK_SPACE.0].contains(&k) => 1,
        k if [VK_LEFT.0, VK_UP.0, VK_PRIOR.0].contains(&k) => -1,
        _ => return false,
    };
    let to = NOW.with(|n| {
        let n = n.borrow();
        let p = n.as_ref()?;
        let (at, of, _) = p.slide;
        let to = (at as i64 + step).clamp(1, of.max(1) as i64);
        Some((p.key.clone(), p.tab, to))
    });
    if let Some((key, tab, to)) = to {
        super::post(&key, tab, &json!({"forge": "goto", "slide": to}));
    }
    true
}

unsafe extern "system" fn stage_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        // The page has the keyboard; this only hears keys while it does not.
        WM_KEYDOWN if key_down(wp.0 as u16) => LRESULT(0),
        WM_CLOSE => {
            end(true);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

impl Notes {
    fn open(screen: &Screen) -> Option<Notes> {
        let [l, t, r, b] = screen.work;
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                NOTES_CLASS,
                w!("Presenter notes"),
                WS_OVERLAPPEDWINDOW,
                l,
                t,
                r - l,
                b - t,
                None,
                None,
                GetModuleHandleW(None).ok().map(|h| h.into()),
                None,
            )
        }
        .ok()?;
        let made = (|| unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let format = |size: f32, weight: DWRITE_FONT_WEIGHT| {
                dw.CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("en-us"),
                )
            };
            let big = format(40.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
            let body = format(30.0, DWRITE_FONT_WEIGHT_NORMAL)?;
            body.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            let small = format(16.0, DWRITE_FONT_WEIGHT_NORMAL)?;
            let small_right = format(16.0, DWRITE_FONT_WEIGHT_NORMAL)?;
            small_right.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
            windows::core::Result::Ok(Notes {
                hwnd,
                d2d,
                rt: None,
                big,
                body,
                small,
                small_right,
            })
        })();
        let notes = match made {
            Ok(n) => n,
            Err(e) => {
                eprintln!("horadric: no presenter notes: {e}");
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return None;
            }
        };
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWMAXIMIZED);
            SetTimer(Some(hwnd), TICK, 1000, None);
        }
        Some(notes)
    }

    fn paint(&mut self, slide: &(u64, u64, String), since: Instant) -> windows::core::Result<()> {
        let mut r = RECT::default();
        unsafe { GetClientRect(self.hwnd, &mut r)? };
        let (wpx, hpx) = (
            (r.right - r.left).max(1) as u32,
            (r.bottom - r.top).max(1) as u32,
        );
        let dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(96) as f32;
        if self.rt.is_none() {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 0.0,
                dpiY: 0.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: self.hwnd,
                pixelSize: D2D_SIZE_U {
                    width: wpx,
                    height: hpx,
                },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            self.rt = Some(unsafe { self.d2d.CreateHwndRenderTarget(&props, &hwnd_props)? });
        }
        let Some(rt) = &self.rt else { return Ok(()) };
        unsafe {
            rt.Resize(&D2D_SIZE_U {
                width: wpx,
                height: hpx,
            })?;
            rt.SetDpi(dpi, dpi);
            let (w, h) = (wpx as f32 * 96.0 / dpi, hpx as f32 * 96.0 / dpi);
            let ink = |r: f32, g: f32, b: f32| D2D1_COLOR_F { r, g, b, a: 1.0 };
            let text = rt.CreateSolidColorBrush(&ink(0.91, 0.89, 0.86), None)?;
            let dim = rt.CreateSolidColorBrush(&ink(0.55, 0.53, 0.49), None)?;
            rt.BeginDraw();
            rt.Clear(Some(&ink(0.08, 0.075, 0.07)));
            let pad = 40.0;
            let draw = |s: &str, f: &IDWriteTextFormat, at: D2D_RECT_F, brush| {
                let wide: Vec<u16> = s.encode_utf16().collect();
                rt.DrawText(
                    &wide,
                    f,
                    &at,
                    brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            };
            let (n, of, notes) = slide;
            let head = if *of > 0 {
                format!("Slide {n} of {of}")
            } else {
                "Starting".to_string()
            };
            draw(&head, &self.big, rect(pad, pad, w - pad, pad + 56.0), &text);
            let secs = since.elapsed().as_secs();
            let clock = format!("{}:{:02}:{:02}", secs / 3600, secs % 3600 / 60, secs % 60);
            draw(
                &clock,
                &self.small_right,
                rect(pad, pad + 8.0, w - pad, pad + 32.0),
                &dim,
            );
            let next = if *n > 0 && n < of {
                format!("Next: slide {}", n + 1)
            } else if *of > 0 {
                "This is the last slide".to_string()
            } else {
                String::new()
            };
            draw(
                &next,
                &self.small,
                rect(pad, pad + 60.0, w - pad, pad + 84.0),
                &dim,
            );
            let body = if notes.trim().is_empty() {
                "No notes for this slide."
            } else {
                notes
            };
            let brush = if notes.trim().is_empty() { &dim } else { &text };
            draw(
                body,
                &self.body,
                rect(pad, pad + 110.0, w - pad, h - pad - 30.0),
                brush,
            );
            draw(
                "Right, Space or Page Down for the next slide, Left for the one before, Esc to end",
                &self.small,
                rect(pad, h - pad - 22.0, w - pad, h - pad),
                &dim,
            );
            if rt.EndDraw(None, None).is_err() {
                // The device was lost: made again on the next paint.
                self.rt = None;
            }
        }
        Ok(())
    }
}

fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left,
        top,
        right: right.max(left),
        bottom: bottom.max(top),
    }
}

unsafe extern "system" fn notes_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe { BeginPaint(hwnd, &mut ps) };
            NOW.with(|n| {
                if let Some(p) = n.borrow_mut().as_mut() {
                    let (slide, since) = (p.slide.clone(), p.started);
                    if let Some(notes) = p.notes.as_mut().filter(|n| n.hwnd == hwnd) {
                        let _ = notes.paint(&slide, since);
                    }
                }
            });
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE | WM_TIMER => {
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_KEYDOWN if key_down(wp.0 as u16) => LRESULT(0),
        WM_CLOSE => {
            end(true);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// A string WebView2 hands out, freed after it is copied.
fn read(get: impl FnOnce(*mut PWSTR) -> windows::core::Result<()>) -> String {
    let mut p = PWSTR::null();
    if get(&mut p).is_err() || p.is_null() {
        return String::new();
    }
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    s
}
