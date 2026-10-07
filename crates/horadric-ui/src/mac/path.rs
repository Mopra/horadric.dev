//! The `PATH` a login shell gives, for an app that Finder or launchd
//! started with only `/usr/bin:/bin:/usr/sbin:/sbin`. Without it no agent
//! installed by npm, Homebrew or Claude Code's installer would be found,
//! and the sessions' own shells would not find them either.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long a login shell gets to say its `PATH`. A slow `.zprofile` is
/// waited for, a hung one is not.
const WAIT: Duration = Duration::from_secs(4);

/// Puts the login shell's `PATH` in front of this process's own.
pub fn adopt_login_path() {
    let Some(theirs) = login_path() else {
        return;
    };
    let ours = std::env::var("PATH").unwrap_or_default();
    // SAFETY: called at start, before any thread that reads the
    // environment exists.
    unsafe { std::env::set_var("PATH", merge(&theirs, &ours)) };
}

fn login_path() -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut child = Command::new(shell)
        .args(["-l", "-c", "printf %s \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + WAIT;
    loop {
        if child.try_wait().ok()?.is_some() {
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().ok()?;
    let path = String::from_utf8(out.stdout).ok()?;
    let path = path.trim();
    (!path.is_empty()).then(|| path.to_string())
}

/// `first`'s folders, then those of `second` it lacks, in order.
pub fn merge(first: &str, second: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for d in first.split(':').chain(second.split(':')) {
        if !d.is_empty() && !out.contains(&d) {
            out.push(d);
        }
    }
    out.join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_login_path_comes_first_without_repeats() {
        assert_eq!(
            merge("/opt/homebrew/bin:/usr/bin", "/usr/bin:/bin"),
            "/opt/homebrew/bin:/usr/bin:/bin"
        );
        assert_eq!(merge("", "/usr/bin"), "/usr/bin");
    }
}
