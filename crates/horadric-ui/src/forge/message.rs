//! What a file tab's page says to Horadric, and the one place it lands.
//!
//! The page side (assets/forge/forge.js, its README has the protocol)
//! posts `{forge: "<what>", ...}`. The page is the file's, and a file
//! is whatever a session or anyone wrote, so every field is checked
//! before it is used.
//!
//! Step 1 of docs/plan/the-forge.md needs only the scroll kept over a
//! reload. Pins and errors land in [`on_message`] and are only logged
//! until the steps that use them are built.

use serde_json::{json, Value};

/// Every message from a file tab's page, the tab `tab` of the project
/// `key`, after its origin was checked.
pub fn on_message(key: &str, tab: u64, msg: &Value) {
    let Some(what) = msg.get("forge").and_then(Value::as_str) else {
        return;
    };
    match what {
        // The page loaded: where it was scrolled to before the reload goes
        // back to it.
        "ready" => {
            if let Some((x, y)) = super::scroll_of(key, tab) {
                super::post(key, tab, &json!({"forge": "restore-scroll", "x": x, "y": y}));
            }
        }
        "scroll" => {
            let at = |k: &str| msg.get(k).and_then(Value::as_f64).filter(|v| v.is_finite());
            if let (Some(x), Some(y)) = (at("x"), at("y")) {
                super::scrolled(key, tab, (x.max(0.0), y.max(0.0)));
            }
        }
        // Step 2 of the plan: sent to the session with a crop of the spot.
        "pin" => eprintln!("horadric: forge pin in tab {tab} of {key} (not handled yet)"),
        // Step 4 of the plan: shown in the tab with a button to send it.
        "error" => {
            let text = msg.get("message").and_then(Value::as_str).unwrap_or_default();
            eprintln!(
                "horadric: forge page error in tab {tab} of {key}: {}",
                text.chars().take(300).collect::<String>()
            );
        }
        _ => {}
    }
}
