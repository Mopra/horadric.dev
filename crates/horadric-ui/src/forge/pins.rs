//! The open pins of each file shown in the Forge, numbered as the page
//! draws them, until a session's next write to the file lands.

use std::cell::RefCell;
use std::path::Path;

use horadric_core::forge::Pin;
use serde_json::Value;

/// A pin the human made and the anchor the page gave for its rune.
struct Open {
    key: String,
    n: u32,
    pin: Pin,
    anchor: Value,
}

thread_local! {
    static OPEN: RefCell<Vec<Open>> = const { RefCell::new(Vec::new()) };
}

/// The open pins of the project `key`, of `file` alone when given, in
/// the order they were made.
pub fn list(key: &str, file: Option<&Path>) -> Vec<Pin> {
    OPEN.with(|o| {
        o.borrow()
            .iter()
            .filter(|p| p.key == key && file.is_none_or(|f| super::same(&p.pin.file, f)))
            .map(|p| p.pin.clone())
            .collect()
    })
}
