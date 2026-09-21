//! Glance command line. For now this is the console face of the state
//! stream: `serve` shows a live table, `run` starts a tagged `claude`,
//! `hooks` manages the Claude Code settings entries.

mod console;
mod run;

use std::process::ExitCode;

const USAGE: &str = "\
glance: every coding agent session as a tile on your desktop

Usage:
  glance                       Show the tiles (the desktop app)
  glance serve                 Listen for Claude Code hook events and show a live table
  glance run [--name NAME] [--cwd DIR] [-- claude args...]
                               Start a `claude` tagged so `serve` can see it
  glance hooks install         Add Glance hooks to ~/.claude/settings.json
  glance hooks uninstall       Remove them
  glance hooks status          Report whether they are installed

Environment:
  GLANCE_PORT                  Port to listen on (default 43117)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("serve") => console::serve(),
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
