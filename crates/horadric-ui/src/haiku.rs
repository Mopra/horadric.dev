//! Asking a small model one thing through the human's own `claude`, for
//! the app's own errands: tidying dictation, titling conversations. Run
//! with no tools, hooks, MCP servers or saved session, so it is no
//! session of theirs and leaves no transcript to list.

use std::io::Read;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// The reply to `prompt` under `system` from `model`, or None when
/// `claude` cannot be started, fails, or takes longer than `longest`.
pub fn ask(model: &str, system: &str, prompt: &str, longest: Duration) -> Option<String> {
    let mut c = Command::new(horadric_pty::headless::claude_exe()?);
    c.arg("-p")
        .arg(prompt)
        .args(["--model", model, "--tools", "", "--strict-mcp-config"])
        .arg("--no-session-persistence")
        .args(["--settings", r#"{"disableAllHooks":true}"#])
        .args(["--system-prompt", system])
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
    for k in horadric_pty::headless::ENV_REMOVE {
        c.env_remove(k);
    }
    for (k, _) in std::env::vars() {
        if k.starts_with("HORADRIC_") {
            c.env_remove(k);
        }
    }
    let mut child = c.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let began = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(None) if began.elapsed() < longest => {
                thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    reader.join().ok()
}
