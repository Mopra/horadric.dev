//! The Settings window on the Mac: the settings Windows has that mean
//! something here, in one window that closes. Unlike Windows it is made of
//! AppKit's own controls, pop up buttons and checkboxes, as a Mac's
//! settings window is; nothing in it is a tile to be drawn like one.
//!
//! As on Windows there is no Save button: a control changes its setting
//! the moment it is used, and the app hands the values back in after every
//! change so the window and the app never disagree.

use std::cell::RefCell;

use horadric_core::saved::Discord;
use horadric_core::{Defaults, Setting};
use objc2::rc::Retained;
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSControlStateValueOff, NSControlStateValueOn, NSFont,
    NSPopUpButton, NSTextField, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSObject, NSPoint, NSRect, NSSize, NSString};

use crate::theme::Theme;

use super::app::{self, Input};

/// What the window shows, handed in by the app.
#[derive(Clone, PartialEq)]
pub struct Values {
    pub theme: Theme,
    pub discord: Discord,
    pub login: bool,
    pub defaults: Defaults,
}

/// A control used, for the app to apply.
#[derive(Clone, Debug)]
pub enum Pick {
    Theme(Theme),
    Discord(Discord),
    Login(bool),
    Default(Setting, Option<String>),
}

const WIDTH: f64 = 460.0;
const ROW: f64 = 30.0;
const HEAD: f64 = 34.0;
const PAD: f64 = 20.0;
const LABEL_W: f64 = 190.0;

// Tags say which control spoke.
const THEME: isize = 1;
const DISCORD: isize = 2;
const LOGIN: isize = 3;
/// The defaults run on from here, one per setting in [`SESSION_ROWS`].
const DEFAULT: isize = 10;

/// The session settings shown, in order: the three a session takes as
/// flags, and the workflow agents' model, which only Settings shows.
const SESSION_ROWS: [Setting; 4] = [
    Setting::Model,
    Setting::Effort,
    Setting::Permissions,
    Setting::Workflow,
];

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this does not
    // implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    pub struct SettingsTarget;

    impl SettingsTarget {
        #[unsafe(method(changed:))]
        fn changed(&self, sender: &NSView) {
            if let Some(pick) = read(sender) {
                app::input(Input::Setting(pick));
            }
        }
    }
);

impl SettingsTarget {
    fn new(mtm: MainThreadMarker) -> Retained<SettingsTarget> {
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

/// The window, its controls by tag, and the target they all reach.
struct Window {
    window: Retained<NSWindow>,
    theme: Retained<NSPopUpButton>,
    discord: Retained<NSPopUpButton>,
    login: Retained<NSButton>,
    defaults: Vec<Retained<NSPopUpButton>>,
    _target: Retained<SettingsTarget>,
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

/// Shows `v` in the window if it is open, after the app changed one.
pub fn refresh(v: &Values) {
    WINDOW.with(|w| {
        if let Some(win) = w.borrow().as_ref() {
            fill(win, v);
        }
    });
}

/// The rows the list of a session setting has: Default first, which
/// leaves it to Claude Code, then its choices.
fn choices(s: Setting) -> Vec<(Option<&'static str>, &'static str)> {
    std::iter::once((None, "Default"))
        .chain(s.choices().iter().map(|(v, name)| (Some(*v), *name)))
        .collect()
}

fn read(sender: &NSView) -> Option<Pick> {
    let tag: isize = unsafe { msg_send![sender, tag] };
    let index = || -> usize {
        let i: isize = unsafe { msg_send![sender, indexOfSelectedItem] };
        i.max(0) as usize
    };
    match tag {
        THEME => Theme::ALL.get(index()).copied().map(Pick::Theme),
        DISCORD => Discord::ALL.get(index()).copied().map(Pick::Discord),
        LOGIN => {
            let state: isize = unsafe { msg_send![sender, state] };
            Some(Pick::Login(state == NSControlStateValueOn))
        }
        t if t >= DEFAULT => {
            let s = *SESSION_ROWS.get((t - DEFAULT) as usize)?;
            let (value, _) = *choices(s).get(index())?;
            Some(Pick::Default(s, value.map(str::to_string)))
        }
        _ => None,
    }
}

fn fill(win: &Window, v: &Values) {
    let at = |i: Option<usize>| i.map_or(0, |i| i as isize);
    win.theme
        .selectItemAtIndex(at(Theme::ALL.iter().position(|t| *t == v.theme)));
    win.discord
        .selectItemAtIndex(at(Discord::ALL.iter().position(|d| *d == v.discord)));
    win.login.setState(if v.login {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });
    for (popup, s) in win.defaults.iter().zip(SESSION_ROWS) {
        let now = v.defaults.get(s);
        let i = choices(s).iter().position(|(value, _)| *value == now);
        popup.selectItemAtIndex(at(i));
    }
}

/// Lays the window out top down, sections with a heading over their rows,
/// each row a label at the left and its control at the right.
fn build(mtm: MainThreadMarker) -> Window {
    let sessions = SESSION_ROWS.len() as f64;
    let height = PAD * 2.0 + HEAD * 3.0 + ROW * (1.0 + 2.0 + sessions) + 24.0;
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
    window.setTitle(&NSString::from_str("Horadric Settings"));
    let content = window.contentView().expect("a window has a content view");
    let target = SettingsTarget::new(mtm);

    // AppKit counts y up from the bottom; the rows are laid from the top.
    let mut top = PAD;
    let y = |top: f64, h: f64| height - top - h;
    let head = |top: &mut f64, text: &str| {
        let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
        label.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
        label.setFrame(NSRect::new(
            NSPoint::new(PAD, y(*top, 20.0)),
            NSSize::new(WIDTH - 2.0 * PAD, 20.0),
        ));
        content.addSubview(&label);
        *top += HEAD;
    };
    let label = |top: f64, text: &str| {
        let l = NSTextField::labelWithString(&NSString::from_str(text), mtm);
        l.setTextColor(Some(&NSColor::secondaryLabelColor()));
        l.setFrame(NSRect::new(
            NSPoint::new(PAD + 8.0, y(top, 20.0) - 2.0),
            NSSize::new(LABEL_W - 8.0, 20.0),
        ));
        content.addSubview(&l);
    };
    let control_frame = |top: f64| {
        NSRect::new(
            NSPoint::new(PAD + LABEL_W, y(top, 24.0)),
            NSSize::new(WIDTH - 2.0 * PAD - LABEL_W, 24.0),
        )
    };
    let popup = |top: f64, tag: isize, items: &[&str]| {
        let p = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            control_frame(top),
            false,
        );
        for item in items {
            p.addItemWithTitle(&NSString::from_str(item));
        }
        p.setTag(tag);
        unsafe {
            p.setTarget(Some(&target));
            p.setAction(Some(sel!(changed:)));
        }
        content.addSubview(&p);
        p
    };

    head(&mut top, "Appearance");
    label(top, "Theme");
    let names: Vec<&str> = Theme::ALL.iter().map(|t| t.label()).collect();
    let theme = popup(top, THEME, &names);
    top += ROW;

    head(&mut top, "General");
    label(top, "Open at login");
    let login = unsafe {
        NSButton::checkboxWithTitle_target_action(
            &NSString::from_str(""),
            Some(&target),
            Some(sel!(changed:)),
            mtm,
        )
    };
    login.setTag(LOGIN);
    login.setFrame(control_frame(top));
    content.addSubview(&login);
    top += ROW;
    label(top, "Show on Discord");
    let names: Vec<&str> = Discord::ALL.iter().map(|d| d.label()).collect();
    let discord = popup(top, DISCORD, &names);
    top += ROW;

    head(&mut top, "Sessions");
    let mut defaults = Vec::new();
    for (i, s) in SESSION_ROWS.iter().enumerate() {
        label(top, s.label());
        let names: Vec<&str> = choices(*s).iter().map(|(_, n)| *n).collect();
        defaults.push(popup(top, DEFAULT + i as isize, &names));
        top += ROW;
    }
    let note = NSTextField::wrappingLabelWithString(
        &NSString::from_str(
            "What every session Horadric starts gets, unless its own arguments say otherwise.",
        ),
        mtm,
    );
    note.setTextColor(Some(&NSColor::tertiaryLabelColor()));
    note.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    note.setFrame(NSRect::new(
        NSPoint::new(PAD + 8.0, y(top, 30.0)),
        NSSize::new(WIDTH - 2.0 * PAD - 8.0, 30.0),
    ));
    content.addSubview(&note);

    window.center();
    Window {
        window,
        theme,
        discord,
        login,
        defaults,
        _target: target,
    }
}
