//! Hand edits from a file tab's page: text changed in place, slides
//! reordered, an image cropped (step 6 of docs/plan/the-forge.md).

use serde_json::Value;

/// A `forge: "edit"`, `"reorder"` or `"crop"` message from the page of
/// the tab `tab` of the project `key`, after its origin was checked.
pub fn on_message(key: &str, tab: u64, msg: &Value) {
    let what = msg.get("forge").and_then(Value::as_str).unwrap_or_default();
    eprintln!("horadric: forge {what} in tab {tab} of {key} (not handled yet)");
}
