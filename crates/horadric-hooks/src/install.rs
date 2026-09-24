//! Writes Horadric's hook entries into Claude Code's user settings.
//!
//! Claude Code only reads hooks from settings files, so the entries have to
//! live in `~/.claude/settings.json`. They are inert for any `claude` not
//! started under Horadric: the session header comes out empty and the listener
//! drops the event. The installer only ever touches entries whose URL
//! contains `/horadric/`, so a user's own hooks are left exactly as they were.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::{hook_url, HOOK_PATH, OWNER_ENV, SESSION_ENV};

/// Hook events Horadric needs. Everything that moves a session between
/// working, waiting and done.
pub const EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "Notification",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

/// `~/.claude/settings.json`.
pub fn settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(Path::new(&home).join(".claude").join("settings.json"))
}

/// The handler Claude Code runs. One shape for every event.
fn handler(port: u16) -> Value {
    json!({
        "type": "http",
        "url": hook_url(port),
        "headers": {
            "X-Horadric-Session": format!("${SESSION_ENV}"),
            "X-Horadric-Port": format!("${OWNER_ENV}")
        },
        "allowedEnvVars": [SESSION_ENV, OWNER_ENV],
        "timeout": 5
    })
}

fn is_ours(handler: &Value) -> bool {
    handler
        .get("url")
        .and_then(Value::as_str)
        .is_some_and(|u| u.contains(HOOK_PATH))
}

/// Adds or refreshes Horadric's hooks in a settings document.
pub fn install_into(settings: &mut Value, port: u16) {
    remove_from(settings);
    let root = ensure_object(settings);
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks = ensure_object(hooks);
    for event in EVENTS {
        let groups = hooks.entry(*event).or_insert_with(|| Value::Array(vec![]));
        if let Value::Array(groups) = groups {
            groups.push(json!({ "hooks": [handler(port)] }));
        }
    }
}

/// Removes every Horadric hook from a settings document, leaving the rest.
pub fn remove_from(settings: &mut Value) {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    for groups in hooks.values_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                list.retain(|h| !is_ours(h));
            }
        }
        groups.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|l| !l.is_empty())
        });
    }
    hooks.retain(|_, groups| groups.as_array().is_none_or(|g| !g.is_empty()));
    if hooks.is_empty() {
        settings.as_object_mut().map(|o| o.remove("hooks"));
    }
}

/// True when the document has a Horadric hook on every event we need.
pub fn is_installed(settings: &Value, port: u16) -> bool {
    let url = hook_url(port);
    EVENTS.iter().all(|event| {
        settings
            .pointer(&format!("/hooks/{event}"))
            .and_then(Value::as_array)
            .is_some_and(|groups| {
                groups.iter().any(|g| {
                    g.get("hooks").and_then(Value::as_array).is_some_and(|l| {
                        l.iter()
                            .any(|h| h.get("url").and_then(Value::as_str) == Some(&url))
                    })
                })
            })
    })
}

fn ensure_object(v: &mut Value) -> &mut Map<String, Value> {
    if !v.is_object() {
        *v = Value::Object(Map::new());
    }
    v.as_object_mut().expect("just made it an object")
}

fn read(path: &Path) -> io::Result<Value> {
    match fs::read(path) {
        Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => Ok(json!({})),
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e),
    }
}

fn write(path: &Path, settings: &Value) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    // Write beside and rename, so a crash mid write cannot leave Claude Code
    // with half a settings file.
    let tmp = path.with_extension("json.horadric-tmp");
    let mut text = serde_json::to_string_pretty(settings)?;
    text.push('\n');
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

/// Installs the hooks into the user's settings file. Idempotent.
pub fn install(path: &Path, port: u16) -> io::Result<()> {
    let mut settings = read(path)?;
    install_into(&mut settings, port);
    write(path, &settings)
}

/// Removes the hooks from the user's settings file. Idempotent.
pub fn uninstall(path: &Path) -> io::Result<()> {
    let mut settings = read(path)?;
    remove_from(&mut settings);
    write(path, &settings)
}

/// Reports whether the user's settings file has the hooks.
pub fn status(path: &Path, port: u16) -> io::Result<bool> {
    Ok(is_installed(&read(path)?, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_is_idempotent_and_removable() {
        let mut s = json!({
            "model": "opus",
            "hooks": {
                "Stop": [ { "hooks": [ { "type": "command", "command": "echo mine" } ] } ]
            }
        });
        install_into(&mut s, 43117);
        install_into(&mut s, 43117);
        assert!(is_installed(&s, 43117));
        // The user's own Stop hook is untouched and ours sits beside it.
        let stop = s.pointer("/hooks/Stop").unwrap().as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "echo mine");

        remove_from(&mut s);
        assert!(!is_installed(&s, 43117));
        assert_eq!(
            s.pointer("/hooks/Stop").unwrap().as_array().unwrap().len(),
            1
        );
        assert!(s.pointer("/hooks/Notification").is_none());
        assert_eq!(s["model"], "opus");
    }

    #[test]
    fn port_change_replaces_old_entries() {
        let mut s = json!({});
        install_into(&mut s, 1000);
        install_into(&mut s, 2000);
        assert!(!is_installed(&s, 1000));
        assert!(is_installed(&s, 2000));
        let stop = s.pointer("/hooks/Stop").unwrap().as_array().unwrap();
        assert_eq!(stop.len(), 1);
    }

    #[test]
    fn remove_on_empty_is_fine() {
        let mut s = json!({ "model": "x" });
        remove_from(&mut s);
        assert_eq!(s, json!({ "model": "x" }));
    }

    #[test]
    fn handler_carries_session_header() {
        let h = handler(43117);
        assert_eq!(h["type"], "http");
        assert_eq!(h["headers"]["X-Horadric-Session"], "$HORADRIC_SESSION");
        assert_eq!(h["allowedEnvVars"][0], "HORADRIC_SESSION");
    }

    #[test]
    fn handler_carries_owner_port_header() {
        let h = handler(43117);
        assert_eq!(h["headers"]["X-Horadric-Port"], "$HORADRIC_OWNER_PORT");
        assert_eq!(h["allowedEnvVars"][1], "HORADRIC_OWNER_PORT");
    }
}
