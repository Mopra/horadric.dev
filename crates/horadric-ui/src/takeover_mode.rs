//! Whether takeover is on: the human's switch in the tray menu, beside the
//! agent's desk. The desk is opened to be used, and closed it refuses.
//! Takeover had no such state, so any session could put a question on the
//! human's own screen. Off refuses every call before anything is asked and
//! keeps the tools out of the sessions' lists.
//!
//! Turning it off ends nothing a session was granted: the grants are not
//! looked at while it is off, and hold again when it is back on, within
//! the same session of the program as before.

use std::sync::atomic::{AtomicBool, Ordering};

use horadric_core::desktop_programs::Mode;

use crate::menu::Item;

/// Read where calls are decided and where the state is saved, which need
/// not be one thread.
static OFF: AtomicBool = AtomicBool::new(false);

/// The tray menu's ids for takeover, between the brothers' and the desk's.
const TRAY: usize = 9800;
const TRAY_OFF: usize = TRAY;
const TRAY_ASK: usize = TRAY + 1;

pub(crate) fn mode() -> Mode {
    if OFF.load(Ordering::Relaxed) {
        Mode::Off
    } else {
        Mode::Ask
    }
}

/// The mode from the state file at the app's start, and the human's pick.
pub(crate) fn set_mode(mode: Mode) {
    OFF.store(mode == Mode::Off, Ordering::Relaxed);
}

/// The tray menu's line for takeover, with the mode in force ticked.
pub(crate) fn tray_item() -> Item {
    let line = |id, label: &str, of: Mode| Item::Action {
        id,
        label: label.into(),
        checked: mode() == of,
        bold: false,
    };
    Item::Submenu(
        "Takeover".into(),
        vec![
            line(TRAY_OFF, "Off", Mode::Off),
            line(TRAY_ASK, "Ask before each program", Mode::Ask),
        ],
    )
}

/// Whether a pick from the tray menu is one of takeover's lines.
pub(crate) fn tray_owns(id: usize) -> bool {
    matches!(id, TRAY_OFF | TRAY_ASK)
}

/// Does what a line of [`tray_item`] says. The app's next save keeps it.
pub(crate) fn tray_pick(id: usize) {
    match id {
        TRAY_OFF => set_mode(Mode::Off),
        TRAY_ASK => set_mode(Mode::Ask),
        _ => {}
    }
}
