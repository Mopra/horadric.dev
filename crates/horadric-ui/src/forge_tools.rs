//! The app's side of the `forge_*` tools of `horadric mcp`: a session
//! shows a file in the Forge, reads its open pins again, or has it
//! rendered to look at. They come the way the browser tools do, as a
//! browser call whose `op` starts with `forge_`, so the session names the
//! project and an agent only reaches its own.

use std::path::{Path, PathBuf};

use horadric_hooks::listener::Reply;
use serde_json::{json, Value};

use super::App;
use crate::browse::error;
use crate::forge;
use crate::web;

impl App {
    /// Answers the `forge_*` call `body` from a session of the project
    /// `key`. The reply back when `body` is not one, for the browser to
    /// answer.
    pub(super) fn forge_call(&mut self, key: &str, body: &Value, reply: Reply) -> Option<Reply> {
        let Some(op) = body["op"].as_str().filter(|o| o.starts_with("forge_")) else {
            return Some(reply);
        };
        let root = self.project_dir(key);
        let file = body["path"].as_str().map(|p| full(root.as_deref(), p));
        match op {
            "forge_open" => {
                let Some(file) = file else {
                    reply.send(error("say which file: path"));
                    return None;
                };
                if let Err(e) = showable(root.as_deref(), &file) {
                    reply.send(error(&e));
                    return None;
                }
                let had = forge::tab_of(key, &file);
                self.open_forge(key, &file, false);
                let Some(id) = forge::tab_of(key, &file) else {
                    reply.send(error("the Forge could not open it"));
                    return None;
                };
                let answer = json!({
                    "file": file.to_string_lossy(),
                    "shown": web::is_shown(key),
                });
                match goto(&body["at"]) {
                    // A tab it had is loaded already; a new one hears
                    // nothing until its page is.
                    Some(msg) if had.is_some() => forge::post(key, id, &msg),
                    Some(msg) => {
                        let k = key.to_string();
                        web::after_load(key, id, move |_| forge::post(&k, id, &msg));
                    }
                    None => {}
                }
                reply.send(answer);
            }
            "forge_pins" => {
                let pins = forge::pins::list(key, file.as_deref());
                let top = root.unwrap_or_default();
                reply.send(json!({
                    "count": pins.len(),
                    "text": horadric_core::forge::pin_message(&pins, &top),
                }));
            }
            "forge_render" => {
                let Some(file) = file else {
                    reply.send(error("say which file: path"));
                    return None;
                };
                if let Err(e) = showable(root.as_deref(), &file) {
                    reply.send(error(&e));
                    return None;
                }
                let slide = body["slide"].as_u64().map(|n| n as u32);
                forge::tools::render(key, root.as_deref(), &file, slide, move |r| {
                    reply.send(r.unwrap_or_else(|e| error(&e)))
                });
            }
            _ => reply.send(error("unknown forge call")),
        }
        None
    }
}

/// `path` as given, or under the project's folder when relative.
fn full(root: Option<&Path>, path: &str) -> PathBuf {
    let p = PathBuf::from(path.replace('/', "\\"));
    match root {
        Some(r) if p.is_relative() => r.join(p),
        _ => p,
    }
}

/// Why the Forge cannot show `file`, if it cannot. A file straight in a
/// drive's root or the home folder is refused: its folder would be served
/// to the page, and with it whatever else is kept there.
fn showable(root: Option<&Path>, file: &Path) -> Result<(), String> {
    if !forge::can_serve(root, file) {
        return Err(format!(
            "the Forge does not show files straight in a drive or the home folder; move {} into a folder of its own",
            file.display()
        ));
    }
    if !file.is_file() {
        return Err(format!("there is no file {}", file.display()));
    }
    if forge::kind(file).is_none() {
        return Err(format!(
            "the Forge does not show .{} files",
            file.extension()
                .map(|e| e.to_string_lossy())
                .unwrap_or_default()
        ));
    }
    Ok(())
}

/// The page message for `at`, `{slide}`, `{line}` or `{page}` as the tool
/// was given it, None when it names no place.
fn goto(at: &Value) -> Option<Value> {
    let n = |k: &str| at.get(k).and_then(Value::as_u64).filter(|n| *n > 0);
    for k in ["slide", "line", "page"] {
        if let Some(v) = n(k) {
            let mut msg = json!({ "forge": "goto" });
            msg[k] = json!(v);
            return Some(msg);
        }
    }
    None
}
