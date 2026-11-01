//! The box a phone's ask (pairing v4, `horadric_core::asking`) puts to
//! this PC's human: on a thread of its own, so nothing waits on the
//! click. The answer waits for the UI thread, which `wake` rouses.
//!
//! It is OK and Cancel, Cancel the default, never Yes and No: a Yes/No
//! box answers Yes to a bare "y", and a box opened while the human types
//! in a session takes the keys. OK has no letter, so stray typing can
//! only press Cancel, a no.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use windows::core::{BOOL, HSTRING};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumThreadWindows, MessageBoxW, PostMessageW, IDCANCEL, IDOK, MB_DEFBUTTON2, MB_ICONQUESTION,
    MB_OKCANCEL, MB_TOPMOST, WM_COMMAND,
};

/// The boxes up and the ones taken back, under one lock, so a box whose
/// ask went before its thread showed it is never shown.
#[derive(Default)]
struct Boxes {
    /// The thread of each box still up, by its token.
    open: Vec<(u64, u32)>,
    withdrawn: HashSet<u64>,
}

static BOXES: Mutex<Option<Boxes>> = Mutex::new(None);
/// Answers given and not yet taken: channel, token, yes.
static ANSWERS: Mutex<Vec<(u32, u64, bool)>> = Mutex::new(Vec::new());

fn boxes<T>(f: impl FnOnce(&mut Boxes) -> T) -> T {
    let mut b = BOXES.lock().unwrap_or_else(|e| e.into_inner());
    f(b.get_or_insert_with(Boxes::default))
}

/// Puts `text` to the human for the ask on `ch` whose box is `token`.
pub fn ask(ch: u32, token: u64, text: String, wake: Box<dyn FnOnce() + Send>) {
    std::thread::Builder::new()
        .name("portal-ask".into())
        .spawn(move || {
            let me = unsafe { GetCurrentThreadId() };
            let gone = boxes(|b| {
                let gone = b.withdrawn.remove(&token);
                if !gone {
                    b.open.push((token, me));
                }
                gone
            });
            let yes = !gone
                && unsafe {
                    MessageBoxW(
                        None,
                        &HSTRING::from(text),
                        &HSTRING::from("Town Portal"),
                        MB_OKCANCEL | MB_ICONQUESTION | MB_DEFBUTTON2 | MB_TOPMOST,
                    )
                } == IDOK;
            boxes(|b| {
                b.open.retain(|(t, _)| *t != token);
                b.withdrawn.remove(&token);
            });
            ANSWERS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((ch, token, yes));
            wake();
        })
        .ok();
}

/// Takes the box with `token` off the screen, as a no, which the ask it
/// was for, already gone, does not hear. A box whose thread has not
/// shown it yet is never shown; one shown a moment after is cancelled
/// on a second try.
pub fn withdraw(token: u64) {
    let thread = boxes(|b| match b.open.iter().find(|(t, _)| *t == token) {
        Some(&(_, thread)) => Some(thread),
        None => {
            b.withdrawn.insert(token);
            None
        }
    });
    let Some(thread) = thread else {
        return;
    };
    cancel(thread);
    std::thread::spawn(move || {
        for _ in 0..5 {
            std::thread::sleep(Duration::from_millis(200));
            if !boxes(|b| b.open.iter().any(|(t, _)| *t == token)) {
                return;
            }
            cancel(thread);
        }
    });
}

/// Presses Cancel on every window of thread `thread`: its box.
fn cancel(thread: u32) {
    unsafe extern "system" fn press(hwnd: HWND, _: LPARAM) -> BOOL {
        let _ = PostMessageW(Some(hwnd), WM_COMMAND, WPARAM(IDCANCEL.0 as usize), LPARAM(0));
        true.into()
    }
    unsafe {
        let _ = EnumThreadWindows(thread, Some(press), LPARAM(0));
    }
}

/// The answers given since the last take.
pub fn take() -> Vec<(u32, u64, bool)> {
    std::mem::take(&mut *ANSWERS.lock().unwrap_or_else(|e| e.into_inner()))
}
