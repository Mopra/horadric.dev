//! The Mac face of Horadric: AppKit through `objc2`, drawn with Core
//! Graphics and AppKit's text. It shares the pure halves of this crate
//! with the Windows face (layout, columns, theme, anim, keys) and the
//! console with its grid, and draws its own windows.

mod app;
pub mod autostart;
mod cluster;
mod delegate;
mod dialog;
mod look;
mod menu;
mod paint;
mod path;
mod queue;
mod snapshot;
mod stage;
mod update;

use crate::console::Note;

pub(crate) use queue::post;

/// Who a console tells about output and its end: the main queue, which
/// needs no handle, so this carries nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct Notify;

/// A console's reader thread saying it has output to show, or ended.
pub(crate) fn console_note(note: Note, serial: usize) {
    queue::post(move || app::input(app::Input::Console(note, serial)));
}

/// Runs the app until it quits.
pub fn run(port: u16, reload: bool) -> Result<(), String> {
    app::run(port, reload)
}
