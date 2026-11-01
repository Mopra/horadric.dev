//! The usage window on the Mac: how much of the account's Claude limits is
//! used and when each starts over, and whose account it is.
//!
//! The numbers come as on Windows, from the status line every session
//! Horadric starts passes on (`horadric status`), each reading merged with
//! the last by `Limits::merge`. Nothing is fetched. On Windows the limits
//! are a window in the columns of tiles; a Mac has the menu bar for what
//! is glanced at, so here it is a small window of AppKit's own controls,
//! opened from there, and the session settings it holds on Windows are in
//! Settings.

use std::cell::RefCell;
use std::path::PathBuf;

use horadric_core::usage::format_until;
use horadric_core::Usage;
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSFont, NSProgressIndicator, NSProgressIndicatorStyle,
    NSTextField, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

/// What the window shows, handed in by the app.
#[derive(Clone, Default, PartialEq)]
pub struct Values {
    pub usage: Option<Usage>,
    /// The account Claude Code is logged in to, worded.
    pub account: Option<String>,
    pub now: u64,
}

const WIDTH: f64 = 360.0;
const PAD: f64 = 18.0;
/// One limit: its name and what is left over a bar.
const LIMIT_H: f64 = 48.0;
/// Session, week and spend, the most a plan has.
const LIMITS: usize = 3;

struct Row {
    name: Retained<NSTextField>,
    detail: Retained<NSTextField>,
    bar: Retained<NSProgressIndicator>,
}

struct Window {
    window: Retained<NSWindow>,
    rows: Vec<Row>,
    /// Says why there are no numbers yet, while there are none.
    empty: Retained<NSTextField>,
    account: Retained<NSTextField>,
}

thread_local! {
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
}

/// Opens the window, or brings it forward, showing `v`.
pub fn show(mtm: MainThreadMarker, v: &Values) {
    WINDOW.with(|w| {
        let mut w = w.borrow_mut();
        let win = w.get_or_insert_with(|| build(mtm));
        fill(win, v);
        win.window.makeKeyAndOrderFront(None);
    });
    objc2_app_kit::NSApplication::sharedApplication(mtm).activate();
}

/// Shows `v` if the window is open: the times count down as it stays.
pub fn refresh(v: &Values) {
    WINDOW.with(|w| {
        if let Some(win) = w.borrow().as_ref().filter(|win| win.window.isVisible()) {
            fill(win, v);
        }
    });
}

/// The window's content showing `v`, made but not opened if it was not,
/// for a snapshot.
pub fn content(mtm: MainThreadMarker, v: &Values) -> Retained<NSView> {
    WINDOW.with(|w| {
        let mut w = w.borrow_mut();
        let win = w.get_or_insert_with(|| build(mtm));
        fill(win, v);
        win.window
            .contentView()
            .expect("a window has a content view")
    })
}

/// The account Claude Code is logged in to, from `.claude.json`: the
/// address, and the organization when it is not just the person's own.
/// The token itself is in the Keychain on a Mac, and not needed to say
/// whose it is.
pub fn account() -> Option<String> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))?;
    let text = std::fs::read_to_string(dir.join(".claude.json")).ok()?;
    let config: serde_json::Value = serde_json::from_str(&text).ok()?;
    let profile = config.get("oauthAccount")?;
    let email = profile.get("emailAddress")?.as_str()?.to_string();
    let org = profile
        .get("organizationName")
        .and_then(|o| o.as_str())
        .filter(|o| !o.contains(&email) && !o.ends_with("'s Organization"));
    Some(match org {
        Some(o) => format!("{email}, {o}"),
        None => email,
    })
}

fn fill(win: &Window, v: &Values) {
    let named = v
        .usage
        .as_ref()
        .map(|u| u.limits.named())
        .unwrap_or_default();
    for (i, row) in win.rows.iter().enumerate() {
        let Some((name, limit)) = named.get(i) else {
            row.name.setHidden(true);
            row.detail.setHidden(true);
            row.bar.setHidden(true);
            continue;
        };
        let (used, until) = limit.at(v.now);
        let detail = match until {
            Some(secs) => format!("{used:.0}% used, starts over in {}", format_until(secs)),
            None => format!("{used:.0}% used"),
        };
        row.name.setStringValue(&NSString::from_str(name));
        row.detail.setStringValue(&NSString::from_str(&detail));
        row.bar.setDoubleValue(used.clamp(0.0, 100.0) as f64);
        row.name.setHidden(false);
        row.detail.setHidden(false);
        row.bar.setHidden(false);
    }
    win.empty.setHidden(!named.is_empty());
    let account = match &v.account {
        Some(a) => format!("Account: {a}"),
        None => "Not logged in to a Claude account".to_string(),
    };
    win.account.setStringValue(&NSString::from_str(&account));
}

fn build(mtm: MainThreadMarker) -> Window {
    let height = PAD * 2.0 + LIMIT_H * LIMITS as f64 + 28.0;
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, height));
    let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable;
    let window = NSWindow::initWithContentRect_styleMask_backing_defer(
        NSWindow::alloc(mtm),
        frame,
        style,
        NSBackingStoreType::Buffered,
        false,
    );
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str("Usage"));
    let content = window.contentView().expect("a window has a content view");
    let inner = WIDTH - 2.0 * PAD;
    // AppKit counts y up from the bottom; the rows are laid from the top.
    let at = |top: f64, h: f64| NSPoint::new(PAD, height - top - h);
    let text = |top: f64, h: f64, s: &str| {
        let t = NSTextField::labelWithString(&NSString::from_str(s), mtm);
        t.setFrame(NSRect::new(at(top, h), NSSize::new(inner, h)));
        content.addSubview(&t);
        t
    };

    let rows = (0..LIMITS)
        .map(|i| {
            let top = PAD + i as f64 * LIMIT_H;
            let name = text(top, 18.0, "");
            name.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
            let detail = text(top, 18.0, "");
            detail.setAlignment(objc2_app_kit::NSTextAlignment::Right);
            detail.setTextColor(Some(&NSColor::secondaryLabelColor()));
            let bar_frame = NSRect::new(at(top + 22.0, 12.0), NSSize::new(inner, 12.0));
            let bar = NSProgressIndicator::initWithFrame(NSProgressIndicator::alloc(mtm), bar_frame);
            bar.setStyle(NSProgressIndicatorStyle::Bar);
            bar.setIndeterminate(false);
            bar.setMinValue(0.0);
            bar.setMaxValue(100.0);
            content.addSubview(&bar);
            Row { name, detail, bar }
        })
        .collect();
    let empty = NSTextField::wrappingLabelWithString(
        &NSString::from_str(
            "No limits heard yet. They come with the status line of a session Horadric started, after its first reply.",
        ),
        mtm,
    );
    empty.setTextColor(Some(&NSColor::secondaryLabelColor()));
    empty.setFrame(NSRect::new(at(PAD, 48.0), NSSize::new(inner, 48.0)));
    content.addSubview(&empty);
    let account = text(PAD + LIMIT_H * LIMITS as f64 + 6.0, 18.0, "");
    account.setTextColor(Some(&NSColor::secondaryLabelColor()));
    account.setFont(Some(&NSFont::systemFontOfSize(11.0)));

    window.center();
    Window {
        window,
        rows,
        empty,
        account,
    }
}
