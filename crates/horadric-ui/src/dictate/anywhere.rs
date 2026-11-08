//! Dictation into whatever program has the keyboard, as Wispr Flow does:
//! a global chord (Ctrl+Alt+D unless Settings set another), tapped to
//! listen until the next tap or held to listen until it is let go, and
//! the words typed where the caret is once they are written down.
//!
//! The chord is a `RegisterHotKey` like the app's other shortcuts, so no
//! keyboard hook sees every key the human types all day. A hotkey says
//! only when it went down; while the microphone is open its key is looked
//! at every 30 ms to tell a hold from a tap, and Esc is taken as a hotkey
//! of its own to drop what was heard. Both are given back when it ends.
//!
//! The words go in as typed characters (`SendInput` with
//! `KEYEVENTF_UNICODE`), not through the clipboard, which stays the
//! human's. A new line is Shift+Enter, which is a new line in a chat box
//! where Enter would send. Beside the ticks, a plate by the caret shows
//! the voice and the words so far ([`super::plate`]).

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, SendInput, UnregisterHotKey, INPUT, INPUT_0, INPUT_KEYBOARD,
    KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOD_NOREPEAT, VIRTUAL_KEY,
    VK_CONTROL, VK_ESCAPE, VK_LWIN, VK_MENU, VK_RETURN, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, KillTimer, RegisterClassW, SetTimer, HWND_MESSAGE,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_HOTKEY, WM_TIMER, WNDCLASSW,
};

use super::WM_DICTATE;
use crate::window::Shared;

/// The timer that looks at the chord's key while the microphone is open.
const POLL: usize = 1;
/// The hotkey id Esc has while the microphone is open.
const ESC: i32 = 1;

thread_local! {
    /// The window the dictation's notes come to: message only, unseen.
    static WINDOW: Cell<Option<HWND>> = const { Cell::new(None) };
    /// The chord's key, looked at to tell when it is let go.
    static KEY: Cell<u16> = const { Cell::new(0) };
}

/// The global chord went down, its key `key`: starts listening, or stops
/// a tapped dictation already listening.
pub fn press(shared: &Rc<Shared>, key: u16) {
    let Some(hwnd) = window() else { return };
    KEY.with(|k| k.set(key));
    super::press(hwnd, None, "");
    if super::shown(hwnd).is_some() {
        super::plate::open(shared, hwnd);
        unsafe {
            let _ = RegisterHotKey(Some(hwnd), ESC, MOD_NOREPEAT, u32::from(VK_ESCAPE.0));
            SetTimer(Some(hwnd), POLL, 30, None);
        }
    }
}

fn window() -> Option<HWND> {
    if let Some(h) = WINDOW.with(Cell::get) {
        return Some(h);
    }
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let class = w!("HoradricDictateAnywhere");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        )
        .ok()?;
        WINDOW.with(|w| w.set(Some(hwnd)));
        Some(hwnd)
    }
}

/// Gives back Esc and stops looking at the key, once nothing is heard.
fn settle(hwnd: HWND) {
    if super::shown(hwnd).is_none() {
        unsafe {
            let _ = UnregisterHotKey(Some(hwnd), ESC);
            let _ = KillTimer(Some(hwnd), POLL);
        }
    }
}

fn is_down(vk: u16) -> bool {
    unsafe { GetAsyncKeyState(i32::from(vk)) < 0 }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wparam.0 == POLL => {
            if !is_down(KEY.with(Cell::get)) {
                // A hold ends here; a tap waits for the next press.
                super::release(hwnd);
                let _ = KillTimer(Some(hwnd), POLL);
            }
            LRESULT(0)
        }
        WM_HOTKEY if wparam.0 as i32 == ESC => {
            super::cancel(hwnd);
            settle(hwnd);
            LRESULT(0)
        }
        WM_DICTATE => {
            match super::take(hwnd) {
                Some(Ok(text)) if !text.is_empty() => {
                    std::thread::spawn(move || type_text(&text));
                }
                Some(Err(e)) => eprintln!("horadric: dictation anywhere: {e}"),
                _ => {}
            }
            settle(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Types `text` into the program with the keyboard, once the chord's
/// modifiers are up: typed under a held Ctrl, a letter is a shortcut.
fn type_text(text: &str) {
    let held = [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN];
    let began = Instant::now();
    while held.iter().any(|k| is_down(k.0)) && began.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let inputs = keystrokes(text);
    if inputs.is_empty() {
        return;
    }
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn key(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// The key presses that type `text`: each UTF-16 unit as a character of
/// its own, and a new line as Shift+Enter.
fn keystrokes(text: &str) -> Vec<INPUT> {
    let mut out = Vec::new();
    for c in text.chars() {
        match c {
            '\r' => {}
            '\n' => {
                out.push(key(VK_SHIFT, 0, KEYBD_EVENT_FLAGS(0)));
                out.push(key(VK_RETURN, 0, KEYBD_EVENT_FLAGS(0)));
                out.push(key(VK_RETURN, 0, KEYEVENTF_KEYUP));
                out.push(key(VK_SHIFT, 0, KEYEVENTF_KEYUP));
            }
            c => {
                let mut units = [0u16; 2];
                for &u in c.encode_utf16(&mut units).iter() {
                    out.push(key(VIRTUAL_KEY(0), u, KEYEVENTF_UNICODE));
                    out.push(key(VIRTUAL_KEY(0), u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_letter_is_a_press_and_a_release_and_a_new_line_is_shift_enter() {
        let k = keystrokes("a\u{1F600}\r\nb");
        // a: 2, the emoji's two units: 4, the new line: 4, b: 2.
        assert_eq!(k.len(), 12);
        let ki = |i: usize| unsafe { k[i].Anonymous.ki };
        assert_eq!(ki(0).wScan, u16::from(b'a'));
        assert_eq!(ki(1).dwFlags, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
        assert_eq!(ki(6).wVk, VK_SHIFT);
        assert_eq!(ki(7).wVk, VK_RETURN);
    }
}
