//! Tells the human's own keys and clicks from the ones a session sends.
//!
//! A session presses Escape in the program it drives, with `SendInput`,
//! and that must reach the program and stop nothing. The human holding
//! Esc must stop the session. A hotkey can not tell the two apart, and it
//! swallows the key for every program besides. A low level hook can: each
//! key and click it sees says whether it was injected.
//!
//! The hooks live on a thread of their own that does nothing but pump
//! messages, never on the app's. Windows calls a low level hook before it
//! delivers the input to anyone, and waits for it: on a thread that also
//! draws, one slow frame would stall the mouse and the keyboard of the
//! whole machine. Nothing is swallowed: every key and click goes on to
//! where it was going.
//!
//! - The keyboard hook is in for as long as control is on, and out the
//!   moment it rests. The human's Esc held for [`super::rules::ESC_HOLD`]
//!   posts the stop to the overlay. A tap is the program's own.
//! - The mouse hook is in only while the pointer is on the plate's Bypass
//!   switch or its speed slider, or one of them is held. A mouse hook is
//!   called for every move of the mouse, each a wake of this thread, and
//!   all it is for is the press on those two. It notes the human's left button going down and where, so
//!   the plate can tell a real press from a message any process posted
//!   or a click a script sent.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, KillTimer, PeekMessageW, PostMessageW, PostThreadMessageW,
    SetTimer, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT,
    LLKHF_INJECTED, LLKHF_LOWER_IL_INJECTED, LLMHF_INJECTED, LLMHF_LOWER_IL_INJECTED, MSG,
    MSLLHOOKSTRUCT, PM_NOREMOVE, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER, WM_USER,
};

use super::rules::ESC_HOLD;

/// The overlay's window and the message it is posted when Esc was held.
static NOTIFY: AtomicIsize = AtomicIsize::new(0);
static STOP_MSG: AtomicU32 = AtomicU32::new(0);

/// The keyboard hook is in.
static ARMED: AtomicBool = AtomicBool::new(false);
/// The mouse hook is wanted, and is in.
static MOUSE_WANTED: AtomicBool = AtomicBool::new(false);
static MOUSE_IN: AtomicBool = AtomicBool::new(false);
/// The human has Esc down.
static ESC_DOWN: AtomicBool = AtomicBool::new(false);
/// The human held Esc long enough, and the overlay has not taken it yet.
static STOP: AtomicBool = AtomicBool::new(false);
/// The human has the left button down, and where it went down.
static LEFT_DOWN: AtomicBool = AtomicBool::new(false);
static LEFT_X: AtomicI32 = AtomicI32::new(0);
static LEFT_Y: AtomicI32 = AtomicI32::new(0);

/// The pump's thread, to tell it to end.
static THREAD: Mutex<Option<u32>> = Mutex::new(None);
/// Which run of the hooks is the live one. A pump that is told to end
/// while it is still starting must not say it is armed after the next one
/// began, nor say the next one is not.
static RUN: AtomicU64 = AtomicU64::new(0);

/// What tells the pump to end, and to put the mouse hook in or take it
/// out as [`MOUSE_WANTED`] says.
const END: u32 = WM_USER + 1;
const MOUSE: u32 = WM_USER + 2;

/// Installs the keyboard hook on a thread of its own, unless it is in.
/// The stop is posted to `notify` as `stop_msg`.
pub(super) fn start(notify: HWND, stop_msg: u32) {
    NOTIFY.store(notify.0 as isize, Ordering::Release);
    STOP_MSG.store(stop_msg, Ordering::Release);
    let Ok(mut thread) = THREAD.lock() else {
        return;
    };
    if thread.is_some() {
        return;
    }
    // Nothing held over from the run before.
    ESC_DOWN.store(false, Ordering::Release);
    STOP.store(false, Ordering::Release);
    LEFT_DOWN.store(false, Ordering::Release);
    MOUSE_WANTED.store(false, Ordering::Release);
    let run = RUN.fetch_add(1, Ordering::AcqRel) + 1;
    let (tx, rx) = std::sync::mpsc::channel::<u32>();
    let spawned = std::thread::Builder::new()
        .name("ghost-input".into())
        .spawn(move || pump(tx, run));
    if spawned.is_err() {
        return;
    }
    // Only for the thread's id, sent before it hooks anything: as long as
    // a thread takes to start.
    *thread = rx.recv().ok();
}

/// Takes the hooks out and ends their thread.
pub(super) fn stop() {
    let Ok(mut thread) = THREAD.lock() else {
        return;
    };
    RUN.fetch_add(1, Ordering::AcqRel);
    if let Some(id) = thread.take() {
        unsafe {
            let _ = PostThreadMessageW(id, END, WPARAM(0), LPARAM(0));
        }
    }
    ARMED.store(false, Ordering::Release);
    MOUSE_WANTED.store(false, Ordering::Release);
    MOUSE_IN.store(false, Ordering::Release);
    ESC_DOWN.store(false, Ordering::Release);
    LEFT_DOWN.store(false, Ordering::Release);
}

/// Puts the mouse hook in or takes it out. Asked on every look of the
/// overlay, so it tells the pump only when the answer changed.
pub(super) fn watch_mouse(on: bool) {
    if MOUSE_WANTED.swap(on, Ordering::AcqRel) == on {
        return;
    }
    if !on {
        LEFT_DOWN.store(false, Ordering::Release);
    }
    if let Ok(thread) = THREAD.lock() {
        if let Some(id) = *thread {
            unsafe {
                let _ = PostThreadMessageW(id, MOUSE, WPARAM(0), LPARAM(0));
            }
        }
    }
}

/// Whether the human's Esc is told from a session's now. False when
/// Windows refused the hook, and then nothing here may be promised.
pub(super) fn armed() -> bool {
    ARMED.load(Ordering::Acquire)
}

/// Whether the human has Esc down this moment.
pub(super) fn esc_down() -> bool {
    armed() && ESC_DOWN.load(Ordering::Acquire)
}

/// Whether the human held Esc since this was last asked, once.
pub(super) fn take_stop() -> bool {
    STOP.swap(false, Ordering::AcqRel)
}

/// Where the human's left button went down, in screen pixels, while it is
/// still down and the mouse hook saw it go down. None for a button the
/// human does not hold: a click a process sent or posted never shows
/// here, nor one made before the hook was in.
pub(super) fn left_held() -> Option<(i32, i32)> {
    (MOUSE_IN.load(Ordering::Acquire) && LEFT_DOWN.load(Ordering::Acquire)).then(|| {
        (
            LEFT_X.load(Ordering::Acquire),
            LEFT_Y.load(Ordering::Acquire),
        )
    })
}

thread_local! {
    /// The timer that says the hold is long enough, on the pump's thread.
    static HOLD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn pump(ready: std::sync::mpsc::Sender<u32>, run: u64) {
    unsafe {
        // Makes the thread's queue, so an end posted at once is not lost.
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
        let _ = ready.send(GetCurrentThreadId());
        let module = GetModuleHandleW(None).ok().map(Into::into);
        let live = || RUN.load(Ordering::Acquire) == run;
        let keys = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), module, 0).ok();
        if live() {
            ARMED.store(keys.is_some(), Ordering::Release);
        }
        if keys.is_none() {
            eprintln!("horadric: no keyboard hook, so Esc can not stop desktop control");
        }
        let mut mouse: Option<HHOOK> = None;
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            if !msg.hwnd.is_invalid() {
                continue;
            }
            match msg.message {
                END => break,
                WM_TIMER => held_long_enough(),
                MOUSE => {
                    let want = live() && MOUSE_WANTED.load(Ordering::Acquire);
                    if want && mouse.is_none() {
                        mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_button), module, 0).ok();
                    } else if !want {
                        if let Some(hook) = mouse.take() {
                            let _ = UnhookWindowsHookEx(hook);
                        }
                    }
                    if live() {
                        MOUSE_IN.store(mouse.is_some(), Ordering::Release);
                    }
                }
                _ => {}
            }
        }
        for hook in [keys, mouse].into_iter().flatten() {
            let _ = UnhookWindowsHookEx(hook);
        }
        end_hold();
        if live() {
            ARMED.store(false, Ordering::Release);
            MOUSE_IN.store(false, Ordering::Release);
        }
    }
}

fn end_hold() {
    let id = HOLD.replace(0);
    if id != 0 {
        unsafe {
            let _ = KillTimer(None, id);
        }
    }
}

/// The hold's timer came due with Esc still down.
fn held_long_enough() {
    end_hold();
    if !ESC_DOWN.load(Ordering::Acquire) {
        return;
    }
    STOP.store(true, Ordering::Release);
    let hwnd = NOTIFY.load(Ordering::Acquire);
    let msg = STOP_MSG.load(Ordering::Acquire);
    if hwnd != 0 && msg != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(0), LPARAM(0));
        }
    }
}

unsafe extern "system" fn keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let k = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let injected = (k.flags.0 & (LLKHF_INJECTED.0 | LLKHF_LOWER_IL_INJECTED.0)) != 0;
        if k.vkCode == VK_ESCAPE.0 as u32 && !injected {
            match wparam.0 as u32 {
                // A held key repeats its down: only the first starts the
                // hold.
                WM_KEYDOWN | WM_SYSKEYDOWN => {
                    if !ESC_DOWN.swap(true, Ordering::AcqRel) {
                        end_hold();
                        let ms = ESC_HOLD.as_millis() as u32;
                        HOLD.set(SetTimer(None, 0, ms, None));
                    }
                }
                WM_KEYUP | WM_SYSKEYUP => {
                    ESC_DOWN.store(false, Ordering::Release);
                    end_hold();
                }
                _ => {}
            }
        }
    }
    CallNextHookEx(None::<HHOOK>, code, wparam, lparam)
}

unsafe extern "system" fn mouse_button(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let m = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        let injected = (m.flags & (LLMHF_INJECTED | LLMHF_LOWER_IL_INJECTED)) != 0;
        match wparam.0 as u32 {
            WM_LBUTTONDOWN if !injected => {
                LEFT_X.store(m.pt.x, Ordering::Release);
                LEFT_Y.store(m.pt.y, Ordering::Release);
                LEFT_DOWN.store(true, Ordering::Release);
            }
            WM_LBUTTONUP if !injected => LEFT_DOWN.store(false, Ordering::Release),
            _ => {}
        }
    }
    CallNextHookEx(None::<HHOOK>, code, wparam, lparam)
}
