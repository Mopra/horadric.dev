//! The general pasteboard, for copy and paste in the terminals.

use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;

/// Puts `text` on the pasteboard. From any thread: a program's OSC 52 is
/// parsed on its console's reader.
pub fn set_text(text: &str) {
    let text = text.to_string();
    crate::mac::post(move || {
        let board = NSPasteboard::generalPasteboard();
        board.clearContents();
        unsafe {
            board.setString_forType(&NSString::from_str(&text), NSPasteboardTypeString);
        }
    });
}

/// The text on the pasteboard, if it holds any. Main thread only.
pub fn text() -> Option<String> {
    let board = NSPasteboard::generalPasteboard();
    let s = unsafe { board.stringForType(NSPasteboardTypeString) }?;
    Some(s.to_string())
}
