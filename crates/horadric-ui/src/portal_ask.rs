//! The box a phone's ask (pairing v4, `horadric_core::asking`) puts to
//! this PC's human: on a thread of its own, so nothing waits on the
//! click, with No the default, since a yes lets a phone type here. The
//! answer waits for the UI thread, which `wake` rouses.

use std::sync::Mutex;

use windows::core::HSTRING;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_DEFBUTTON2, MB_ICONQUESTION, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO,
};

/// The thread of each box still up, by its token, so one whose ask went
/// can be taken off the screen.
static OPEN: Mutex<Vec<(u64, u32)>> = Mutex::new(Vec::new());
/// Answers given and not yet taken: channel, token, yes.
static ANSWERS: Mutex<Vec<(u32, u64, bool)>> = Mutex::new(Vec::new());

/// Puts `text` to the human for the ask on `ch` whose box is `token`.
pub fn ask(ch: u32, token: u64, text: String, wake: Box<dyn FnOnce() + Send>) {
    std::thread::Builder::new()
        .name("portal-ask".into())
        .spawn(move || {
            let me = unsafe { GetCurrentThreadId() };
            OPEN.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((token, me));
            let style = MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2 | MB_SETFOREGROUND | MB_TOPMOST;
            let yes = unsafe {
                MessageBoxW(
                    None,
                    &HSTRING::from(text),
                    &HSTRING::from("Town Portal"),
                    style,
                )
            } == IDYES;
            OPEN.lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|(t, _)| *t != token);
            ANSWERS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((ch, token, yes));
            wake();
        })
        .ok();
}

/// Takes the box with `token` off the screen, as a No, which the ask it
/// was for, already gone, does not hear.
pub fn withdraw(token: u64) {
    let open = OPEN.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for (t, thread) in open {
        if t == token {
            crate::brothers::dismiss(thread);
        }
    }
}

/// The answers given since the last take.
pub fn take() -> Vec<(u32, u64, bool)> {
    std::mem::take(&mut *ANSWERS.lock().unwrap_or_else(|e| e.into_inner()))
}
