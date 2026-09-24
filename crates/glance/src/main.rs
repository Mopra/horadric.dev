//! Glance command line. With no arguments it starts the desktop app in the
//! background and returns, so a terminal is a launcher and not a host: closing
//! it must not take every session with it. `app` runs the app in the
//! foreground instead, for its log. `new` asks the running app for a session
//! in a terminal of its own, `run` starts a tagged `claude` in the current
//! terminal, `serve` shows the state stream as a table, `reload` hands the
//! running app over to this build, and the rest set Glance up on this machine.

mod console;
mod explorer;
mod install;
mod reload;
mod run;
mod status;

use std::net::TcpStream;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

/// Runs a console program without giving it a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const USAGE: &str = "\
glance: every coding agent session as a tile on your desktop

Usage:
  glance                       Start Glance in the background (tray icon and tiles)
  glance app                   Run it in this terminal instead, with its log
  glance new [--name NAME] [--cwd DIR] [-- claude args...]
                               Start a session in a Glance terminal
  glance run [--name NAME] [--cwd DIR] [-- claude args...]
                               Start a tagged `claude` in this terminal instead
  glance serve                 Listen for Claude Code hook events and show a live table
  glance hooks install         Add Glance hooks to ~/.claude/settings.json
  glance hooks uninstall       Remove them
  glance hooks status          Report whether they are installed
  glance explorer install      Add 'Open in Glance' to folders in Explorer
  glance explorer uninstall    Remove it
  glance explorer status       Report whether it is installed
  glance install               Install for this user: PATH, Start menu, Explorer,
                               start with Windows, Claude Code hooks
  glance uninstall             Take all of that back out
  glance status                The status line Glance gives its sessions: reads Claude
                               Code's JSON on stdin and passes it to the app
  glance reload [--now]        Swap the running Glance for this build once no session
                               is working, and resume the running sessions in it.
                               --now does not wait. A dev instance just restarts.

Environment:
  GLANCE_PORT                  Port to listen on (default 43117, or 43118 with GLANCE_DEV)
  GLANCE_DEV                   Run beside the installed Glance: own port, own state in
                               %APPDATA%\\Glance-dev, no autostart, red tray icon, and
                               install, hooks and explorer changes are refused
  GLANCE_AGENT                 Program a Glance terminal runs (default claude.exe)
  GLANCE_DEBUG                 Log window positions and paint times
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if glance_hooks::dev() && changes_machine(&args) {
        eprintln!(
            "glance: GLANCE_DEV is set, and `{}` would change the installed Glance",
            args.join(" ")
        );
        return ExitCode::FAILURE;
    }
    let result = match args.first().map(String::as_str) {
        Some("serve") => console::serve(),
        Some("new") => run::new(&args[1..]),
        Some("run") => run::run(&args[1..]),
        Some("hooks") => hooks(args.get(1).map(String::as_str)),
        Some("explorer") => explorer_command(args.get(1).map(String::as_str)),
        None => std::env::current_exe()
            .map_err(|e| e.to_string())
            .and_then(|exe| launch(&exe)),
        Some("app") => glance_ui::app::run(
            glance_hooks::port(),
            args.get(1).is_some_and(|a| a == "--reload"),
        ),
        Some("reload") => reload::request(&args[1..]),
        Some("status") => status::run(),
        Some("swap") => reload::swap(&args[1..]),
        Some("install") => install_command(),
        Some("uninstall") => uninstall_command(),
        Some("-h" | "--help" | "help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("glance: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Commands that write to the registry, the user's files or Claude Code's
/// settings. A dev instance must leave all of those to the installed one.
fn changes_machine(args: &[String]) -> bool {
    let words: Vec<&str> = args.iter().take(2).map(String::as_str).collect();
    matches!(
        words.as_slice(),
        ["install" | "uninstall", ..] | ["hooks" | "explorer", "install" | "uninstall"]
    )
}

fn running() -> bool {
    let port = glance_hooks::port();
    TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(200)).is_ok()
}

/// Starts `exe app` hidden and detached, and waits until it listens.
fn launch(exe: &Path) -> Result<(), String> {
    if running() {
        println!("Glance is already running. Its icon is in the tray, by the clock.");
        return Ok(());
    }
    Command::new(exe)
        .arg("app")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", exe.display()))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !running() {
        if Instant::now() > deadline {
            return Err("Glance did not start within 10 seconds".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("Glance started. Its icon is in the tray, by the clock.");
    Ok(())
}

fn install_command() -> Result<(), String> {
    let dir = install::install(running())?;
    let settings =
        glance_hooks::install::settings_path().ok_or("cannot find your home directory")?;
    glance_hooks::install::install(&settings, glance_hooks::port())
        .map_err(|e| format!("{}: {e}", settings.display()))?;
    println!("installed Glance in {}", dir.display());
    println!("  Start menu: search for Glance");
    println!("  Terminal:   `glance` in any new terminal (this one still has the old PATH)");
    println!("  Explorer:   right click a folder, Show more options, Open in Glance");
    println!("  Starts with Windows (switch it off from the tray menu)");
    println!("  Claude Code hooks in {}", settings.display());
    launch(&dir.join("glance.exe"))
}

fn uninstall_command() -> Result<(), String> {
    install::uninstall(running())?;
    let settings =
        glance_hooks::install::settings_path().ok_or("cannot find your home directory")?;
    glance_hooks::install::uninstall(&settings)
        .map_err(|e| format!("{}: {e}", settings.display()))?;
    println!("uninstalled Glance: PATH, Start menu, Explorer, start with Windows and hooks");
    if let Some(dir) = install::dir().filter(|d| d.exists()) {
        println!(
            "{} is still there (a program can not delete itself); remove it by hand",
            dir.display()
        );
    }
    println!("saved sessions stay in %APPDATA%\\Glance");
    Ok(())
}

fn explorer_command(sub: Option<&str>) -> Result<(), String> {
    match sub {
        Some("install") => {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let glancew = exe.with_file_name("glancew.exe");
            explorer::install(&glancew)?;
            println!("added \"Open in Glance\" to folders in Explorer");
            println!("runs {}", glancew.display());
            println!("on Windows 11 it is under \"Show more options\"");
            Ok(())
        }
        Some("uninstall") => {
            explorer::uninstall()?;
            println!("removed \"Open in Glance\" from Explorer");
            Ok(())
        }
        Some("status") => {
            let ok = explorer::is_installed();
            println!("{}", if ok { "installed" } else { "not installed" });
            Ok(())
        }
        _ => Err(format!(
            "usage: glance explorer install|uninstall|status\n\n{USAGE}"
        )),
    }
}

fn hooks(sub: Option<&str>) -> Result<(), String> {
    use glance_hooks::install;
    let path = install::settings_path().ok_or("cannot find your home directory")?;
    let port = glance_hooks::port();
    match sub {
        Some("install") => {
            install::install(&path, port).map_err(|e| format!("{}: {e}", path.display()))?;
            println!("installed Glance hooks in {}", path.display());
            println!("posting to {}", glance_hooks::hook_url(port));
            Ok(())
        }
        Some("uninstall") => {
            install::uninstall(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            println!("removed Glance hooks from {}", path.display());
            Ok(())
        }
        Some("status") => {
            let ok =
                install::status(&path, port).map_err(|e| format!("{}: {e}", path.display()))?;
            println!(
                "{} ({})",
                if ok { "installed" } else { "not installed" },
                path.display()
            );
            Ok(())
        }
        _ => Err(format!(
            "usage: glance hooks install|uninstall|status\n\n{USAGE}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn only_setup_commands_change_the_machine() {
        for line in [
            "install",
            "uninstall",
            "hooks install",
            "explorer uninstall",
        ] {
            assert!(changes_machine(&args(line)), "{line}");
        }
        for line in [
            "",
            "app",
            "new --name x",
            "hooks status",
            "explorer",
            "serve",
        ] {
            assert!(!changes_machine(&args(line)), "{line}");
        }
    }
}
