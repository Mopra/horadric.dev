//! Adding and removing a session's own git worktree. What a project wants
//! and what a worktree is told live in `horadric_core::worktree`; this is
//! the part that runs `git`.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use horadric_core::worktree::{self, Place, Ports, Worktree};
use horadric_hooks::tasks as file;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// How often ending a session tries to remove its worktree. The agent was
/// just killed, and its files can stay locked for a moment after.
const REMOVE_TRIES: u32 = 3;

/// A worktree just added for a new session.
pub struct Fresh {
    /// Where the session starts: the same folder in the new worktree as it
    /// was asked to start in the main one.
    pub cwd: PathBuf,
    pub worktree: Worktree,
    /// The project's setup commands, for its first start only.
    pub setup: Vec<String>,
}

/// Where `dir` is in its repository's main working tree, None when it is
/// in a linked worktree or in no repository.
pub fn main_tree(dir: &Path) -> Option<Place> {
    let out = git(
        dir,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
            "--show-toplevel",
            "--show-prefix",
        ],
    )
    .ok()?;
    worktree::main_tree(&out)
}

/// Adds a worktree for a new session named `name` in `dir`, on a new
/// branch from what the main tree has checked out, with a range of ports
/// that `taken` does not use. None when the project does not want one;
/// an error when git refused, and the session then shares the main tree.
pub fn add(dir: &Path, name: &str, taken: &[Ports]) -> Result<Option<Fresh>, String> {
    let settings = file::worktrees(dir);
    if !settings.enabled {
        return Ok(None);
    }
    let Some(place) = main_tree(dir) else {
        return Ok(None);
    };
    let top = Path::new(&place.top);
    let branch = worktree::branch(name, |b| {
        Path::new(&worktree::folder(&place.top, b)).exists()
            || git(
                top,
                &[
                    "show-ref",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{b}"),
                ],
            )
            .is_ok()
    });
    let path = worktree::folder(&place.top, &branch);
    git(top, &["worktree", "add", "-b", &branch, &path])?;
    let cwd = if place.prefix.is_empty() {
        PathBuf::from(&path)
    } else {
        Path::new(&path).join(&place.prefix)
    };
    Ok(Some(Fresh {
        cwd,
        worktree: Worktree {
            path,
            main: place.top.replace('\\', "/"),
            branch,
            ports: worktree::free_ports(settings.ports, taken),
        },
        setup: settings.setup,
    }))
}

/// Removes a session's worktree once the session has ended, and its branch
/// once nothing is on it that the main tree lacks. Git refuses both while
/// there is work to lose, changed files in the tree or commits on the
/// branch, and then both stay for the human. In the background, since the
/// ended agent may hold its files for a moment.
pub fn remove(w: Worktree) {
    std::thread::spawn(move || {
        let main = Path::new(&w.main);
        let mut result = Ok(String::new());
        for attempt in 0..REMOVE_TRIES {
            std::thread::sleep(Duration::from_secs(1 + attempt as u64));
            result = git(main, &["worktree", "remove", &w.path]);
            if result.is_ok() || !Path::new(&w.path).exists() {
                break;
            }
        }
        if let Err(e) = result {
            eprintln!("horadric: kept the worktree {}: {e}", w.path);
            return;
        }
        if let Err(e) = git(main, &["branch", "-d", &w.branch]) {
            eprintln!("horadric: kept the branch {}: {e}", w.branch);
        }
    });
}

/// Runs git in `dir`: what it printed, or what it said when it failed.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}
