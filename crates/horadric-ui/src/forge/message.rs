//! What a file tab's page says to Horadric, and the one place it lands.
//!
//! The page side (assets/forge/forge.js, its README has the protocol)
//! posts `{forge: "<what>", ...}`. The page is the file's, and a file
//! is whatever a session or anyone wrote, so every field is checked
//! before it is used.
//!
//! Pins, pin mode and errors sent on go to [`super::pins`], hand edits
//! to [`super::edit`].

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
            super::edit::page_gone(key, tab);
            if let Some((x, y)) = super::scroll_of(key, tab) {
                super::post(
                    key,
                    tab,
                    &json!({"forge": "restore-scroll", "x": x, "y": y}),
                );
            }
            super::pins::page_ready(key, tab);
        }
        "scroll" => {
            let at = |k: &str| msg.get(k).and_then(Value::as_f64).filter(|v| v.is_finite());
            if let (Some(x), Some(y)) = (at("x"), at("y")) {
                super::scrolled(key, tab, (x.max(0.0), y.max(0.0)));
            }
        }
        "pin" => super::pins::page_pin(key, tab, msg),
        "pinning" => {
            if let Some(on) = msg.get("on").and_then(Value::as_bool) {
                super::pins::page_pinning(key, tab, on);
            }
        }
        "send-error" => super::pins::page_send_error(key, tab, msg),
        // Step 4 of the plan: shown in the tab with a button to send it.
        "error" => {
            let text = msg
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            eprintln!(
                "horadric: forge page error in tab {tab} of {key}: {}",
                text.chars().take(300).collect::<String>()
            );
        }
        // Step 6 of the plan, kept in a module of its own.
        "edit" | "reorder" | "crop" => super::edit::on_message(key, tab, msg),
        // The editor: the whole file written, edit mode on or off.
        "write" => super::edit::on_write(key, tab, msg),
        // The markdown editor: a block drawn as the saved page draws it, a
        // picture pasted or dropped kept beside the file.
        "render" => super::edit::on_render(key, tab, msg),
        "save-image" => super::edit::on_save_image(key, tab, msg),
        "editing" => {
            if let Some(on) = msg.get("on").and_then(Value::as_bool) {
                super::edit::page_editing(key, tab, on);
            }
        }
        _ => {}
    }
}
