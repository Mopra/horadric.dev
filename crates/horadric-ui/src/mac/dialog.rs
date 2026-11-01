//! The questions the app asks: AppKit's own alerts and folder picker,
//! which look right on a Mac and need nothing drawn.

use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertSecondButtonReturn, NSAlertStyle,
    NSAlertThirdButtonReturn, NSApplication, NSModalResponseOK, NSOpenPanel, NSTextField,
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

/// Says something worth knowing.
pub fn info(title: &str, text: &str) {
    alert(title, text).runModal();
}

/// Asks whether to go on. True for `ok`.
pub fn confirm(title: &str, text: &str, ok: &str) -> bool {
    let a = alert(title, text);
    a.addButtonWithTitle(&NSString::from_str(ok));
    a.addButtonWithTitle(&NSString::from_str("Cancel"));
    a.runModal() == NSAlertFirstButtonReturn
}

/// Asks a yes or no question whose buttons say what each does. True for
/// `yes`. `no` comes first, so Return presses it and a stray key never
/// lets a machine in.
pub fn choose(title: &str, text: &str, yes: &str, no: &str) -> bool {
    let a = alert(title, text);
    a.addButtonWithTitle(&NSString::from_str(no));
    a.addButtonWithTitle(&NSString::from_str(yes));
    a.runModal() == NSAlertSecondButtonReturn
}

/// Asks with up to three buttons, the first the default: which was
/// pressed, None for none.
pub fn buttons(title: &str, text: &str, labels: &[&str]) -> Option<usize> {
    let a = alert(title, text);
    for l in labels.iter().take(3) {
        a.addButtonWithTitle(&NSString::from_str(l));
    }
    match a.runModal() {
        r if r == NSAlertFirstButtonReturn => Some(0),
        r if r == NSAlertSecondButtonReturn => Some(1),
        r if r == NSAlertThirdButtonReturn => Some(2),
        _ => None,
    }
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
    folder_panel(
        mtm,
        start,
        "Start Session",
        "Pick the project folder to start a session in.",
    )
}

/// Where a session a brother hands the Mac goes, for a project it has no
/// checkout of: a folder that holds it, or one to put it in.
pub fn pick_place(start: Option<&Path>) -> Option<PathBuf> {
    folder_panel(
        mtm(),
        start,
        "Put It Here",
        "Pick where the handed off session goes: a folder that holds its project, or one to put it in.",
    )
}

fn folder_panel(
    mtm: MainThreadMarker,
    start: Option<&Path>,
    prompt: &str,
    message: &str,
) -> Option<PathBuf> {
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseDirectories(true);
    panel.setCanChooseFiles(false);
    panel.setAllowsMultipleSelection(false);
    panel.setCanCreateDirectories(true);
    panel.setPrompt(Some(&NSString::from_str(prompt)));
    panel.setMessage(Some(&NSString::from_str(message)));
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

/// A file picked to show read only, starting in `start`.
pub fn pick_file(mtm: MainThreadMarker, start: Option<&Path>) -> Option<PathBuf> {
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseDirectories(false);
    panel.setCanChooseFiles(true);
    panel.setAllowsMultipleSelection(false);
    panel.setPrompt(Some(&NSString::from_str("Open")));
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
