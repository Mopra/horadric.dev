//! Menus: the menu bar's own (the app menu, Edit, Session, Window), the
//! status item's in the menu bar, and the ones that pop up on a right
//! click. Every item reaches one target object, which tells the app by
//! the item's tag.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSControlStateValueOff, NSControlStateValueOn, NSEventModifierFlags, NSMenu,
    NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSObject, NSPoint, NSString};

use super::app::{self, Input};

/// One line of a menu.
pub enum Item {
    Pick {
        title: String,
        tag: usize,
        enabled: bool,
        checked: bool,
        /// A key equivalent with Cmd, and Shift too when upper case.
        key: Option<&'static str>,
    },
    Separator,
    Sub(String, Vec<Item>),
}

impl Item {
    pub fn pick(title: impl Into<String>, tag: usize) -> Item {
        Item::Pick {
            title: title.into(),
            tag,
            enabled: true,
            checked: false,
            key: None,
        }
    }

    pub fn checked(title: impl Into<String>, tag: usize, on: bool) -> Item {
        Item::Pick {
            title: title.into(),
            tag,
            enabled: true,
            checked: on,
            key: None,
        }
    }

    pub fn keyed(title: impl Into<String>, tag: usize, key: &'static str) -> Item {
        Item::Pick {
            title: title.into(),
            tag,
            enabled: true,
            checked: false,
            key: Some(key),
        }
    }
}

thread_local! {
    /// The tag of the item picked from a pop up menu, read once it closes.
    static PICKED: Cell<Option<usize>> = const { Cell::new(None) };
}

/// Where an item's pick goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// A pop up: remember the tag for [`popup`] to return.
    Popup,
    /// The status item's menu or the menu bar's: tell the app.
    Bar,
}

pub struct TargetIvars {
    kind: Cell<Kind>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this does not
    // implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = TargetIvars]
    pub struct Target;

    impl Target {
        #[unsafe(method(pick:))]
        fn pick(&self, sender: &NSMenuItem) {
            let tag = sender.tag() as usize;
            match self.ivars().kind.get() {
                Kind::Popup => PICKED.with(|p| p.set(Some(tag))),
                Kind::Bar => app::input(Input::Menu(tag)),
            }
        }
    }
);

impl Target {
    fn new(mtm: MainThreadMarker, kind: Kind) -> Retained<Target> {
        let this = Self::alloc(mtm).set_ivars(TargetIvars {
            kind: Cell::new(kind),
        });
        unsafe { msg_send![super(this), init] }
    }
}

thread_local! {
    static POPUP_TARGET: std::cell::OnceCell<Retained<Target>> = const { std::cell::OnceCell::new() };
    static BAR_TARGET: std::cell::OnceCell<Retained<Target>> = const { std::cell::OnceCell::new() };
}

fn target(mtm: MainThreadMarker, kind: Kind) -> Retained<Target> {
    let cell = match kind {
        Kind::Popup => &POPUP_TARGET,
        Kind::Bar => &BAR_TARGET,
    };
    cell.with(|c| c.get_or_init(|| Target::new(mtm, kind)).clone())
}

fn build(mtm: MainThreadMarker, title: &str, items: &[Item], kind: Kind) -> Retained<NSMenu> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
    menu.setAutoenablesItems(false);
    let target = target(mtm, kind);
    for item in items {
        match item {
            Item::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
            Item::Pick {
                title,
                tag,
                enabled,
                checked,
                key,
            } => {
                let equivalent = key.unwrap_or("");
                let mi = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(mtm),
                        &NSString::from_str(title),
                        Some(sel!(pick:)),
                        &NSString::from_str(&equivalent.to_lowercase()),
                    )
                };
                if equivalent.chars().any(|c| c.is_uppercase()) {
                    mi.setKeyEquivalentModifierMask(
                        NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
                    );
                }
                unsafe { mi.setTarget(Some(&target)) };
                mi.setTag(*tag as isize);
                mi.setEnabled(*enabled);
                if *checked {
                    mi.setState(NSControlStateValueOn);
                } else {
                    mi.setState(NSControlStateValueOff);
                }
                menu.addItem(&mi);
            }
            Item::Sub(title, sub) => {
                let mi = NSMenuItem::new(mtm);
                mi.setTitle(&NSString::from_str(title));
                let sub = build(mtm, title, sub, kind);
                mi.setSubmenu(Some(&sub));
                menu.addItem(&mi);
            }
        }
    }
    menu
}

/// Shows `items` at `at`, a point on the screen, and returns the tag of the
/// one picked. Runs until the menu closes.
pub fn popup(mtm: MainThreadMarker, items: &[Item], at: NSPoint) -> Option<usize> {
    let menu = build(mtm, "", items, Kind::Popup);
    PICKED.with(|p| p.set(None));
    let _ = menu.popUpMenuPositioningItem_atLocation_inView(None, at, None);
    PICKED.with(|p| p.take())
}

/// The status item, by the clock in the menu bar.
pub struct Status {
    item: Retained<NSStatusItem>,
}

impl Status {
    pub fn new(mtm: MainThreadMarker) -> Status {
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = item.button(mtm) {
            let name = if horadric_hooks::dev() {
                "square.grid.2x2.fill"
            } else {
                "square.grid.2x2"
            };
            if let Some(image) =
                objc2_app_kit::NSImage::imageWithSystemSymbolName_accessibilityDescription(
                    &NSString::from_str(name),
                    Some(&NSString::from_str("Horadric")),
                )
            {
                image.setTemplate(true);
                button.setImage(Some(&image));
            } else {
                button.setTitle(&NSString::from_str("H"));
            }
            if horadric_hooks::dev() {
                button.setTitle(&NSString::from_str("dev"));
            }
        }
        Status { item }
    }

    pub fn set_menu(&self, mtm: MainThreadMarker, items: &[Item]) {
        let menu = build(mtm, "Horadric", items, Kind::Bar);
        self.item.setMenu(Some(&menu));
    }
}

/// The menu bar's menus. `app_items` go in the app menu, `session_items`
/// in Session. Edit's items go to the first responder, the stage's
/// terminal, by their standard actions.
pub fn set_main(mtm: MainThreadMarker, app_items: &[Item], session_items: &[Item]) {
    let app = NSApplication::sharedApplication(mtm);
    let bar = NSMenu::new(mtm);
    let add = |title: &str, menu: Retained<NSMenu>| {
        let mi = NSMenuItem::new(mtm);
        mi.setTitle(&NSString::from_str(title));
        mi.setSubmenu(Some(&menu));
        bar.addItem(&mi);
    };
    add("Horadric", build(mtm, "Horadric", app_items, Kind::Bar));

    let edit = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Edit"));
    for (title, action, key) in [
        ("Copy", sel!(copy:), "c"),
        ("Paste", sel!(paste:), "v"),
        ("Select All", sel!(selectAll:), "a"),
    ] {
        edit.addItem(&responder_item(mtm, title, action, key));
    }
    add("Edit", edit);
    add("Session", build(mtm, "Session", session_items, Kind::Bar));

    let window = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Window"));
    window.addItem(&responder_item(
        mtm,
        "Minimize",
        sel!(performMiniaturize:),
        "m",
    ));
    window.addItem(&responder_item(mtm, "Zoom", sel!(performZoom:), ""));
    add("Window", window.clone());
    app.setWindowsMenu(Some(&window));
    app.setMainMenu(Some(&bar));
}

/// An item that sends `action` down the responder chain.
fn responder_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Sel,
    key: &str,
) -> Retained<NSMenuItem> {
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key),
        )
    }
}
