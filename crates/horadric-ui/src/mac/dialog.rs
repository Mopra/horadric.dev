//! The questions the app asks: AppKit's own alerts and folder picker,
//! which look right on a Mac and need nothing drawn.

use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertSecondButtonReturn, NSAlertStyle, NSApplication,
    NSModalResponseOK, NSOpenPanel, NSTextField,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSURL};

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("dialogs run on the main thread")
}

fn alert(title: &str, text: &str) -> Retained<NSAlert> {
    let mtm = mtm();
    let a = NSAlert::new(mtm);
    a.setMessageText(&NSString::from_str(title));
    a.setInformativeText(&NSString::from_str(text));
    NSApplication::sharedApplication(mtm).activate();
    a
}

/// Says something went wrong.
pub fn error(title: &str, text: &str) {
    let a = alert(title, text);
    a.setAlertStyle(NSAlertStyle::Warning);
    a.runModal();
}

/// Asks whether to go on. True for `ok`.
pub fn confirm(title: &str, text: &str, ok: &str) -> bool {
    let a = alert(title, text);
    a.addButtonWithTitle(&NSString::from_str(ok));
    a.addButtonWithTitle(&NSString::from_str("Cancel"));
    a.runModal() == NSAlertFirstButtonReturn
}

/// Asks for a line of text, starting from `initial`. None for Cancel.
pub fn ask(title: &str, text: &str, initial: &str) -> Option<String> {
    let mtm = mtm();
    let a = alert(title, text);
    a.addButtonWithTitle(&NSString::from_str("OK"));
    a.addButtonWithTitle(&NSString::from_str("Cancel"));
    let field = NSTextField::initWithFrame(
        NSTextField::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(280.0, 24.0)),
    );
    field.setStringValue(&NSString::from_str(initial));
    a.setAccessoryView(Some(&field));
    a.window().setInitialFirstResponder(Some(&field));
    (a.runModal() == NSAlertFirstButtonReturn).then(|| field.stringValue().to_string())
}

/// When sessions run: whether quitting ends them (Some(true)), keeps them
/// running in their hosts (Some(false)), or does not happen (None).
pub fn quit(running: usize) -> Option<bool> {
    let what = if running == 1 {
        "1 session is running".to_string()
    } else {
        format!("{running} sessions are running")
    };
    let a = alert(
        &what,
        "Kept running, they carry on without Horadric and come back when it starts again.",
    );
    a.addButtonWithTitle(&NSString::from_str("Keep Them Running"));
    a.addButtonWithTitle(&NSString::from_str("End Them"));
    a.addButtonWithTitle(&NSString::from_str("Cancel"));
    match a.runModal() {
        r if r == NSAlertFirstButtonReturn => Some(false),
        r if r == NSAlertSecondButtonReturn => Some(true),
        _ => None,
    }
}

/// A folder picked for a new session, starting in `start`.
pub fn pick_folder(mtm: MainThreadMarker, start: Option<&Path>) -> Option<PathBuf> {
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseDirectories(true);
    panel.setCanChooseFiles(false);
    panel.setAllowsMultipleSelection(false);
    panel.setPrompt(Some(&NSString::from_str("Start Session")));
    panel.setMessage(Some(&NSString::from_str(
        "Pick the project folder to start a session in.",
    )));
    if let Some(dir) = start {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&dir.to_string_lossy()));
        panel.setDirectoryURL(Some(&url));
    }
    NSApplication::sharedApplication(mtm).activate();
    if panel.runModal() != NSModalResponseOK {
        return None;
    }
    let url = panel.URL()?;
    Some(PathBuf::from(url.path()?.to_string()))
}

pub fn about() {
    let a = alert(
        &format!("Horadric {}", env!("CARGO_PKG_VERSION")),
        "Every coding agent session as a tile on your desktop.\n\nhttps://horadric.dev",
    );
    a.runModal();
}

/// A word in passing. The menu bar shows the same, so nothing is lost
/// where notifications are off.
pub fn notify(title: &str, text: &str) {
    eprintln!("horadric: {title}: {text}");
}
