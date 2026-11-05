//! The browser pane's bar while the shown tab is a file: the file's name
//! where the address would be, and the Forge's buttons at its right end.
//! Laid out here, drawn by `glyphs` with the rest of the header, and
//! clicked through the pane's bar like its other keys.
//!
//! The buttons change with what the tab shows: the file as it is gets the
//! history, export and, for a deck, present; a version from the history
//! gets the steps through it, compare, restore and the way back.

use std::cell::RefCell;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use horadric_core::forge::Kind;
use windows::core::{w, PCWSTR};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::web;

/// A button of the Forge's bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    /// Pin mode on or off: click or drag on the page to say what to change.
    Pin,
    /// Into the history: the version kept before this one.
    History,
    /// PDF, pictures, the clipboard, a real deck, or another program.
    Export,
    /// A deck full screen on a chosen screen.
    Present,
    /// The file as text, in the editor.
    Source,
    /// In the history: one version further back.
    Older,
    /// In the history: one version nearer now, then the file itself.
    Newer,
    /// In the history: this version beside another.
    Compare,
    /// In the history: write this version back over the file.
    Restore,
    /// Out of the history, back to the file as it is.
    Live,
    /// The Forge's page of the folder the file is in, to pick another.
    Files,
    /// On a folder's page: the project's top folder.
    Home,
}

/// What the bar shows for a file's tab: its name, with which version when
/// the tab shows one, and its buttons left to right.
pub struct Shown {
    pub name: String,
    pub buttons: Vec<Button>,
    /// Buttons drawn as on: pin mode while it is.
    pub lit: Vec<Button>,
}

/// A tab's bar as last laid out, and what it was laid out from.
struct Laid {
    key: String,
    tab: u64,
    file: PathBuf,
    generation: u64,
    name: String,
    buttons: Vec<Button>,
}

thread_local! {
    /// The bars laid out, one per project: `shown` runs on every paint and
    /// mouse move, and laying out lists the versions kept and stats the
    /// file. A change, a step through the history or another tab or file
    /// lays it out again.
    static LAID: RefCell<Vec<Laid>> = const { RefCell::new(Vec::new()) };
}

/// The bar of the project's shown tab, None when it shows a web page.
pub fn shown(key: &str) -> Option<Shown> {
    let tab = web::shown_tab(key)?;
    if let Some((root, dir)) = super::folder_of(key, tab) {
        return Some(Shown {
            name: folder_name(&root, &dir),
            buttons: vec![Button::Home],
            lit: Vec::new(),
        });
    }
    let file = super::file_of(key, tab)?;
    let generation = super::history::generation();
    let known = LAID.with(|l| {
        l.borrow()
            .iter()
            .find(|l| l.key == key && l.tab == tab && l.generation == generation && l.file == file)
            .map(|l| (l.name.clone(), l.buttons.clone()))
    });
    let (name, buttons) = match known {
        Some(laid) => laid,
        None => {
            let (name, buttons) = lay_out(key, tab, &file)?;
            LAID.with(|l| {
                let mut l = l.borrow_mut();
                l.retain(|l| l.key != key);
                l.push(Laid {
                    key: key.to_string(),
                    tab,
                    file: file.clone(),
                    generation,
                    name: name.clone(),
                    buttons: buttons.clone(),
                });
            });
            (name, buttons)
        }
    };
    // Pin mode is a flag in memory, read fresh so a toggle shows at once.
    let lit = if buttons.contains(&Button::Pin) && super::pins::pinning(key) {
        vec![Button::Pin]
    } else {
        Vec::new()
    };
    Some(Shown { name, buttons, lit })
}

/// The bar's name and buttons for `file` in the tab, read from disk.
fn lay_out(key: &str, tab: u64, file: &Path) -> Option<(String, Vec<Button>)> {
    let name = file.file_name()?.to_string_lossy().into_owned();
    if let Some(label) = super::history::label(key, tab) {
        let buttons = vec![
            Button::Older,
            Button::Newer,
            Button::Compare,
            Button::Restore,
            Button::Live,
        ];
        return Some((format!("{name}, {label}"), buttons));
    }
    let mut buttons = vec![Button::Files];
    if can_pin(file) {
        buttons.push(Button::Pin);
    }
    buttons.push(Button::Source);
    buttons.push(Button::History);
    buttons.push(Button::Export);
    if is_deck(file) {
        buttons.push(Button::Present);
    }
    Some((name, buttons))
}

/// A folder's page's name in the bar: the project's folder, then the way
/// down to it.
fn folder_name(root: &Path, dir: &Path) -> String {
    let top = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    match dir.strip_prefix(root) {
        Ok(rest) if !rest.as_os_str().is_empty() => {
            format!("{top}/{}", rest.to_string_lossy().replace('\\', "/"))
        }
        _ => top,
    }
}

/// Whether the page side can point at what the tab shows: a PDF shows in
/// WebView2's own viewer, which runs no script of ours.
fn can_pin(file: &Path) -> bool {
    !matches!(super::kind(file), Some(Kind::Pdf) | None)
}

thread_local! {
    /// Whether a file is a deck, by when it was last written: the bar is
    /// laid out on every paint, and telling means reading the file.
    static DECKS: RefCell<Vec<(PathBuf, Option<SystemTime>, bool)>> =
        const { RefCell::new(Vec::new()) };
}

pub fn is_deck(file: &Path) -> bool {
    let modified = std::fs::metadata(file).and_then(|m| m.modified()).ok();
    let known = DECKS.with(|d| {
        d.borrow()
            .iter()
            .find(|(f, m, _)| f == file && *m == modified)
            .map(|(_, _, deck)| *deck)
    });
    if let Some(deck) = known {
        return deck;
    }
    let deck = horadric_core::forge::kind_of(file) == Some(Kind::Deck);
    DECKS.with(|d| {
        let mut d = d.borrow_mut();
        d.retain(|(f, _, _)| f != file);
        if d.len() > 32 {
            d.remove(0);
        }
        d.push((file.to_path_buf(), modified, deck));
    });
    deck
}

/// What the Forge's bar holds where the address field would be.
pub struct Layout {
    /// Where the file's name goes, left and right.
    pub name: (f32, f32),
    /// Each button and its left edge, one `square` wide.
    pub buttons: Vec<(Button, f32)>,
    pub square: f32,
}

/// The bar laid out in the address field's place, `field` its left and
/// right, `buttons` `square` wide at the right end.
pub fn layout(field: (f32, f32), square: f32, buttons: &[Button]) -> Layout {
    let (left, right) = field;
    let start = (right - square * buttons.len() as f32).max(left);
    let buttons = buttons
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
        Button::Pin => "\u{E718}",
        Button::History => "\u{E81C}",
        Button::Export => "\u{E72D}",
        Button::Present => "\u{E786}",
        Button::Source => "\u{E943}",
        Button::Older => "\u{E76B}",
        Button::Newer => "\u{E76C}",
        Button::Compare => "\u{E8AB}",
        Button::Restore => "\u{E777}",
        Button::Live => "\u{E711}",
        Button::Files => "\u{E8B7}",
        Button::Home => "\u{E80F}",
    }
}

/// What the button does, for its tip.
pub fn tip(b: Button) -> &'static str {
    match b {
        Button::Pin => "Point at what to change (P)",
        Button::History => "Earlier versions",
        Button::Export => "PDF, pictures, copy, To .pptx, open with",
        Button::Present => "Present full screen",
        Button::Source => "Edit the file as text",
        Button::Older => "The version before this one",
        Button::Newer => "The version after this one",
        Button::Compare => "Compare",
        Button::Restore => "Restore this version",
        Button::Live => "Back to the file as it is",
        Button::Files => "The files beside this one, to open another or start one",
        Button::Home => "The project's top folder (Ctrl+Shift+O opens the Forge)",
    }
}

/// Does what `b` says to the project's shown file tab.
pub fn press(b: Button, key: &str) {
    let Some(tab) = web::shown_tab(key) else {
        return;
    };
    if b == Button::Home {
        return super::browse(key, tab, None);
    }
    let Some(file) = super::file_of(key, tab) else {
        return;
    };
    match b {
        Button::Files => super::browse(key, tab, file.parent()),
        Button::Home => {}
        Button::Pin => super::pins::toggle_pinning(key),
        Button::History | Button::Older => super::history::older(key, tab),
        Button::Newer => super::history::newer(key, tab),
        Button::Compare => super::history::compare(key, tab),
        Button::Restore => super::history::restore(key, tab),
        Button::Live => super::history::live(key, tab),
        Button::Export => super::export::menu(key, tab, &file),
        Button::Present => super::present::start(key, tab, true),
        Button::Source => {
            super::source_view(key);
        }
    }
}

/// Opens `file` with the program Windows has for it.
pub fn open_with(file: &Path) {
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

/// Shows `file`'s folder in Explorer, the file chosen.
pub fn show_in_explorer(file: &Path) {
    // Explorer reads its own command line: the path in quotes, straight
    // after the comma.
    let spawned = Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", file.display()))
        .spawn();
    if let Err(e) = spawned {
        eprintln!("horadric: cannot start Explorer: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buttons_end_the_field_and_the_name_has_the_rest() {
        let l = layout((100.0, 400.0), 30.0, &[Button::History, Button::Export]);
        assert_eq!(l.buttons, vec![(Button::History, 340.0), (Button::Export, 370.0)]);
        assert_eq!(l.name, (100.0, 336.0));
        assert_eq!(hit(&l, 345.0), Some(Button::History));
        assert_eq!(hit(&l, 399.0), Some(Button::Export));
        assert_eq!(hit(&l, 200.0), None);
    }
}
