//! Glance command line. With no arguments it is the desktop app. `new` asks
//! the running app for a session in a terminal of its own, `run` starts a
//! tagged `claude` in the current terminal, `serve` shows the state stream as
//! a table, `hooks` manages the Claude Code settings entries.

mod console;
mod run;

use std::process::ExitCode;

const USAGE: &str = "\
glance: every coding agent session as a tile on your desktop

Usage:
  glance                       Show the tiles (the desktop app)
  glance new [--name NAME] [--cwd DIR] [-- claude args...]
                               Start a session in a Glance terminal
  glance run [--name NAME] [--cwd DIR] [-- claude args...]
                               Start a tagged `claude` in this terminal instead
  glance serve                 Listen for Claude Code hook events and show a live table
  glance hooks install         Add Glance hooks to ~/.claude/settings.json
  glance hooks uninstall       Remove them
  glance hooks status          Report whether they are installed

Environment:
  GLANCE_PORT                  Port to listen on (default 43117)
  GLANCE_AGENT                 Program a Glance terminal runs (default claude.exe)
  GLANCE_DEBUG                 Log window positions and paint times
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("serve") => console::serve(),
        Some("new") => run::new(&args[1..]),
        Some("run") => run::run(&args[1..]),
        Some("hooks") => hooks(args.get(1).map(String::as_str)),
        None | Some("tiles") => {
            glance_ui::app::run(glance_hooks::port()).map_err(|e| e.to_string())
        }
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
