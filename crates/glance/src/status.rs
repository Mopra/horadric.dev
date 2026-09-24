//! `glance status`: the status line of every session Glance starts.
//!
//! Claude Code runs it after each reply with JSON on stdin that says what
//! no hook does: the model, how full the context is, the usage limits. It
//! passes that on to the Glance that owns the session and prints a short
//! line for the bottom of the terminal.

use std::io::Read;

use glance_core::Status;
use glance_hooks::{client, OWNER_ENV, SESSION_ENV, SESSION_HEADER, STATUS_PATH};

pub fn run() -> Result<(), String> {
    let mut body = Vec::new();
    std::io::stdin()
        .read_to_end(&mut body)
        .map_err(|e| e.to_string())?;
    let session = std::env::var(SESSION_ENV).unwrap_or_default();
    let owner = std::env::var(OWNER_ENV)
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(glance_hooks::port);
    if !session.is_empty() {
        // Glance not answering must not break the line Claude Code shows.
        let text = String::from_utf8_lossy(&body);
        let _ = client::post(owner, STATUS_PATH, &[(SESSION_HEADER, &session)], &text);
    }
    if let Some(status) = Status::from_json(&body) {
        println!("{}", status.line());
    }
    Ok(())
}
