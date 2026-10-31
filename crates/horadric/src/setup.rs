//! `horadric setup <program> <args>`: runs a new worktree's setup commands,
//! then the agent. The app starts a session in a new worktree through this,
//! so the setup's output shows in the session's own pane and the agent
//! starts in the same console once it is done. Its arguments go on
//! exactly as given, which a batch file in between could not promise.
//!
//! `horadric setup --headless <program> <args>` is the same for a Folio,
//! whose stdout and stdin are the agent's stream-json: the setup writes
//! to stderr and reads nothing, so neither the stream nor the first prompt
//! is touched.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

use horadric_core::worktree::SETUP_ENV;

/// Dim, then back to normal, so a command reads apart from its output.
const DIM: &str = "\x1b[2m";
const PLAIN: &str = "\x1b[0m";

pub fn run(args: &[String]) -> Result<(), String> {
    let headless = args.first().is_some_and(|a| a == "--headless");
    let args = &args[usize::from(headless)..];
    let (program, rest) = args
        .split_first()
        .ok_or("usage: horadric setup <program> [args...]")?;
    let commands: Vec<String> = std::env::var(SETUP_ENV)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    ignore_ctrl_c();
    let comspec = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into());
    let say = |line: String| match headless {
        true => eprintln!("{line}"),
        false => println!("{line}"),
    };
    for c in &commands {
        say(match headless {
            true => format!("> {c}"),
            false => format!("{DIM}> {c}{PLAIN}"),
        });
        // `/s` takes the quotes off and runs the rest as typed, so the
        // command's own quotes reach cmd as they are in the config.
        let mut command = Command::new(&comspec);
        command.raw_arg(format!("/d /s /c \"{c}\""));
        if headless {
            command.stdin(Stdio::null()).stdout(std::io::stderr());
        }
        match command.status() {
            Ok(s) if s.success() => {}
            // The agent starts anyway: it can read what went wrong above
            // and put it right, which is better than a pane that stops.
            Ok(s) => say(format!(
                "horadric: `{c}` failed ({}), the agent starts anyway",
                s.code().unwrap_or(1)
            )),
            Err(e) => say(format!("horadric: cannot run `{c}`: {e}")),
        }
    }
    if !commands.is_empty() && !headless {
        println!();
    }
    let status = Command::new(program)
        .args(rest)
        .env_remove(SETUP_ENV)
        .status()
        .map_err(|e| format!("cannot start {program}: {e}"))?;
    // The agent's own code, which says to the app how the session ended.
    std::process::exit(status.code().unwrap_or(1))
}

/// Ctrl+C is for the setup command or the agent in front, not for this
/// process between them, which would end the session with it. A handler
/// rather than ignoring it outright, since ignoring is handed down to the
/// children.
pub(crate) fn ignore_ctrl_c() {
    use windows::core::BOOL;
    use windows::Win32::System::Console::SetConsoleCtrlHandler;
    unsafe extern "system" fn handled(_: u32) -> BOOL {
        BOOL(1)
    }
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(handled), true);
    }
}
