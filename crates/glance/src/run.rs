//! `glance run`: start a `claude` that the tiles can see.
//!
//! This is the plain version: stdio is inherited, so the CLI runs in the
//! terminal you called it from. The PTY and tile come later; the tagging is
//! the same.

use std::io::Write;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use glance_core::HookEvent;
use glance_hooks::{HOOK_PATH, SESSION_ENV, SESSION_HEADER};
use serde_json::json;

pub fn run(args: &[String]) -> Result<(), String> {
    let mut name: Option<String> = None;
    let mut cwd: Option<PathBuf> = None;
    let mut passthrough: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--name" => {
                name = args.get(i + 1).cloned();
                i += 2;
            }
            "--cwd" => {
                cwd = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--" => {
                passthrough.extend_from_slice(&args[i + 1..]);
                break;
            }
            other => return Err(format!("unknown option `{other}` for run")),
        }
    }

    let cwd = match cwd {
        Some(c) => c,
        None => std::env::current_dir().map_err(|e| e.to_string())?,
    };
    let id = new_id(name.as_deref(), &cwd);
    let shown = name
        .clone()
        .or_else(|| cwd.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| id.clone());

    // Tell the tiles about this session now. Claude Code sends nothing until
    // the first prompt, and an idle tile is better than a missing one.
    if !register(&id, &shown, &cwd) {
        eprintln!("glance: tiles are not running, the session will appear on its first prompt");
    }
    eprintln!("glance: starting claude as session {id}");
    let status = Command::new("claude")
        .args(&passthrough)
        .current_dir(&cwd)
        .env(SESSION_ENV, &id)
        .status()
        .map_err(|e| format!("could not start `claude`: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("claude exited with {status}"))
    }
}

/// Posts a `GlanceRegister` event to the listener. False when nothing listens.
fn register(id: &str, name: &str, cwd: &Path) -> bool {
    let port = glance_hooks::port();
    let Ok(mut stream) =
        TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(300))
    else {
        return false;
    };
    let body = json!({
        "session_id": "",
        "hook_event_name": HookEvent::REGISTER,
        "cwd": cwd.to_string_lossy(),
        "name": name,
    })
    .to_string();
    let req = format!(
        "POST {HOOK_PATH} HTTP/1.1\r\nHost: 127.0.0.1\r\n{SESSION_HEADER}: {id}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).is_ok()
}

/// A session id that is unique enough and readable in a table:
/// `<name>-<seconds mod a day>`.
fn new_id(name: Option<&str>, cwd: &Path) -> String {
    let base = name
        .map(str::to_string)
        .or_else(|| cwd.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "session".into());
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() % 86_400)
        .unwrap_or(0);
    format!("{base}-{secs}")
}
