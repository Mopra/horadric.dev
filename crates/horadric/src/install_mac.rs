//! `horadric install` on a Mac: makes Horadric an app like any other.
//!
//! Puts `Horadric.app` in `~/Applications`, where per user apps live and
//! Launchpad and Spotlight find them, links the command into
//! `~/.local/bin` (where Claude Code's own installer puts `claude`, so it
//! is usually on `PATH` already), adds the LaunchAgent that opens it at
//! login, and installs the Claude Code hooks and, when Grok Build is
//! there, its hook file and its entry for `horadric mcp`. No admin rights
//! anywhere. `horadric uninstall` takes all of it back out, and leaves the
//! saved sessions in Application Support.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use horadric_ui::plist;

/// What a bundle's `Contents/MacOS` holds, the one binary.
pub const BINARIES: [&str; 1] = ["horadric"];

fn home() -> Option<PathBuf> {
    horadric_hooks::home().map(PathBuf::from)
}

/// The installed bundle.
pub fn app_dir() -> Option<PathBuf> {
    home().map(|h| h.join("Applications/Horadric.app"))
}

/// Where the installed binary lives, inside the bundle.
pub fn dir() -> Option<PathBuf> {
    app_dir().map(|a| a.join("Contents/MacOS"))
}

/// The bundle a binary runs from, when it runs from one.
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension()? == "app")
        .then(|| app.to_path_buf())
}

pub fn install(running: bool) -> Result<PathBuf, String> {
    if running {
        return Err(
            "Horadric is running. Quit it from the menu bar first, then install again.".into(),
        );
    }
    let app = app_dir().ok_or("cannot find your home folder")?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.canonicalize().unwrap_or(exe);
    match bundle_of(&exe) {
        Some(from) if same_dir(&from, &app) => {}
        Some(from) => {
            let _ = fs::remove_dir_all(&app);
            if let Some(parent) = app.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            // ditto keeps the signature, attributes and links as they are.
            let status = Command::new("/usr/bin/ditto")
                .arg(&from)
                .arg(&app)
                .status()
                .map_err(|e| format!("ditto: {e}"))?;
            if !status.success() {
                return Err(format!(
                    "could not copy {} to {}",
                    from.display(),
                    app.display()
                ));
            }
        }
        None => bundle(&exe, &app, None)?,
    }
    let binary = app.join("Contents/MacOS/horadric");
    link_command(&binary)?;
    horadric_ui::mac::autostart::enable_at(&binary)?;
    Ok(app)
}

pub fn uninstall(running: bool) -> Result<(), String> {
    if running {
        return Err(
            "Horadric is running. Quit it from the menu bar first, then uninstall again.".into(),
        );
    }
    horadric_ui::mac::autostart::disable();
    if let Some(link) = link_path() {
        if fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink()) {
            let _ = fs::remove_file(link);
        }
    }
    if let Some(app) = app_dir() {
        let _ = fs::remove_dir_all(app);
    }
    Ok(())
}

/// Makes `app` a bundle around the binary `exe`: what CI does for a
/// release, and what installing a plain build does.
pub fn bundle(exe: &Path, app: &Path, icon: Option<&Path>) -> Result<(), String> {
    let contents = app.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    let _ = fs::remove_dir_all(app);
    for d in [&macos, &resources] {
        fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    fs::copy(exe, macos.join("horadric")).map_err(|e| format!("copying the binary: {e}"))?;
    let icns = resources.join("horadric.icns");
    match icon {
        Some(i) => fs::copy(i, &icns).map(|_| ()),
        // Drawn by the same code as the tiles' and the tray's icon.
        None => fs::write(&icns, horadric_ui::icns::horadric(horadric_hooks::dev())),
    }
    .map_err(|e| format!("icon: {e}"))?;
    let icon_name = Some("horadric");
    let version = env!("CARGO_PKG_VERSION");
    fs::write(
        contents.join("Info.plist"),
        plist::info_plist(version, icon_name),
    )
    .map_err(|e| format!("Info.plist: {e}"))?;
    fs::write(contents.join("PkgInfo"), "APPL????").map_err(|e| format!("PkgInfo: {e}"))?;
    // Apple Silicon runs nothing unsigned. An ad hoc signature is enough
    // for a binary that was never downloaded by a browser.
    let _ = Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-"])
        .arg(app)
        .status();
    Ok(())
}

/// `horadric bundle OUT.app [--icon FILE.icns]`, for CI's release build.
pub fn bundle_command(args: &[String]) -> Result<(), String> {
    let usage = "usage: horadric bundle OUT.app [--icon FILE.icns]";
    let (out, icon) = match args {
        [out] => (out, None),
        [out, flag, icon] if flag == "--icon" => (out, Some(Path::new(icon))),
        _ => return Err(usage.into()),
    };
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    bundle(&exe, Path::new(out), icon)?;
    println!("made {out}");
    Ok(())
}

fn link_path() -> Option<PathBuf> {
    home().map(|h| h.join(".local/bin/horadric"))
}

/// Where the command is linked from, and whether that folder is on `PATH`.
pub fn link() -> Option<(PathBuf, bool)> {
    let link = link_path()?;
    let dir = link.parent()?.to_path_buf();
    let on_path = std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| same_dir(&d, &dir)));
    Some((link, on_path))
}

fn link_command(binary: &Path) -> Result<(), String> {
    let link = link_path().ok_or("cannot find your home folder")?;
    if let Some(dir) = link.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    match fs::symlink_metadata(&link) {
        Ok(m) if m.file_type().is_symlink() => {
            let _ = fs::remove_file(&link);
        }
        Ok(_) => {
            return Err(format!(
                "{} is there already and is not a link; move it and install again",
                link.display()
            ))
        }
        Err(_) => {}
    }
    std::os::unix::fs::symlink(binary, &link).map_err(|e| format!("{}: {e}", link.display()))
}

/// Writes Grok's hook file to run `exe hook grok`, and gives Grok `exe mcp`
/// in its config, when Grok is on this machine. Returns where the hooks
/// went, or None when there is no Grok.
pub fn grok_hooks(exe: &Path) -> Result<Option<PathBuf>, String> {
    use horadric_hooks::install as hooks;
    let Some(home) = hooks::grok_home() else {
        return Ok(None);
    };
    let command = horadric_ui::exe_command(exe, "hook grok");
    let path = hooks::grok_hooks_path(&home);
    match hooks::install_grok(&home, &command, &exe.to_string_lossy()) {
        Ok(true) => Ok(Some(path)),
        Ok(false) => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Removes Grok's hook file and its MCP table, if there are any.
pub fn remove_grok_hooks() -> Result<(), String> {
    use horadric_hooks::install as hooks;
    let Some(home) = hooks::grok_home() else {
        return Ok(());
    };
    hooks::uninstall_grok(&home).map_err(|e| format!("{}: {e}", home.display()))
}

pub fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binary_in_a_bundle_knows_its_bundle() {
        assert_eq!(
            bundle_of(Path::new(
                "/Applications/Horadric.app/Contents/MacOS/horadric"
            )),
            Some(PathBuf::from("/Applications/Horadric.app"))
        );
        assert_eq!(bundle_of(Path::new("/repo/target/release/horadric")), None);
        assert_eq!(bundle_of(Path::new("/x/Contents/MacOS/horadric")), None);
    }
}
