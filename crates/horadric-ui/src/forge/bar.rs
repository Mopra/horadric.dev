//! The browser pane's bar while the shown tab is a file: the file's name
//! where the address would be, and the Forge's buttons at its right end.
//! Laid out here, drawn by `glyphs` with the rest of the header, and
//! clicked through the pane's bar like its other keys.
//!
//! Later steps of the plan add pins, history and export: each is another
//! [`Button`], put before the ones here.

use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

use windows::core::{w, PCWSTR};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// A button of the Forge's bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    /// The program Windows opens the file with.
    OpenWith,
    /// The file's folder in Explorer, the file chosen.
    Explorer,
}

/// The buttons, left to right.
pub const BUTTONS: [Button; 2] = [Button::OpenWith, Button::Explorer];

/// What the Forge's bar holds where the address field would be.
pub struct Layout {
    /// Where the file's name goes, left and right.
    pub name: (f32, f32),
    /// Each button and its left edge, one `square` wide.
    pub buttons: Vec<(Button, f32)>,
    pub square: f32,
}

/// The bar laid out in the address field's place, `field` its left and
/// right, buttons `square` wide at the right end.
pub fn layout(field: (f32, f32), square: f32) -> Layout {
    let (left, right) = field;
    let start = (right - square * BUTTONS.len() as f32).max(left);
    let buttons = BUTTONS
        .iter()
        .enumerate()
        .map(|(i, b)| (*b, start + i as f32 * square))
        .collect();
    Layout {
        name: (left, (start - 4.0).max(left)),
        buttons,
        square,
    }
}

/// The button under `x`, a point in the header.
pub fn hit(l: &Layout, x: f32) -> Option<Button> {
    l.buttons
        .iter()
        .find(|(_, at)| x >= *at && x < at + l.square)
        .map(|(b, _)| *b)
}

/// The button's mark, in Segoe MDL2 Assets as the bar's others are.
pub fn glyph(b: Button) -> &'static str {
    match b {
        Button::OpenWith => "\u{E7AC}",
        Button::Explorer => "\u{E838}",
    }
}

/// What the button does, for its tip.
pub fn tip(b: Button) -> &'static str {
    match b {
        Button::OpenWith => "Open with the program Windows has for it",
        Button::Explorer => "Show in Explorer",
    }
}

/// Does what `b` says to the file `file`.
pub fn press(b: Button, file: &Path) {
    match b {
        Button::OpenWith => {
            let wide: Vec<u16> = file.as_os_str().encode_wide().chain([0]).collect();
            unsafe {
                ShellExecuteW(
                    None,
                    w!("open"),
                    PCWSTR(wide.as_ptr()),
                    None,
                    None,
                    SW_SHOWNORMAL,
                );
            }
        }
        Button::Explorer => {
            // Explorer reads its own command line: the path in quotes,
            // straight after the comma.
            let spawned = Command::new("explorer.exe")
                .raw_arg(format!("/select,\"{}\"", file.display()))
                .spawn();
            if let Err(e) = spawned {
                eprintln!("horadric: cannot start Explorer: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buttons_end_the_field_and_the_name_has_the_rest() {
        let l = layout((100.0, 400.0), 30.0);
        assert_eq!(l.buttons, vec![(Button::OpenWith, 340.0), (Button::Explorer, 370.0)]);
        assert_eq!(l.name, (100.0, 336.0));
        assert_eq!(hit(&l, 345.0), Some(Button::OpenWith));
        assert_eq!(hit(&l, 399.0), Some(Button::Explorer));
        assert_eq!(hit(&l, 200.0), None);
    }
}
