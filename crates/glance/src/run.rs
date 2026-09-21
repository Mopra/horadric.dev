//! `glance run`: start a `claude` that the listener can see.
//!
//! This is the plain version: stdio is inherited, so the CLI runs in the
//! terminal you called it from. The PTY and tile come later; the tagging is
//! the same.

use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use glance_hooks::SESSION_ENV;

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

/// A session id that is unique enough and readable in a table:
/// `<name>-<seconds mod a day>`.
fn new_id(name: Option<&str>, cwd: &std::path::Path) -> String {
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
