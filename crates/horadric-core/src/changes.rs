//! What a session changed, as a phone or a brother reads it (`changes`,
//! `diff`): every file since the commit its work counts from, and one
//! file's diff. Here so a Windows PC, the Mac and a Linux node all read
//! git alike.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::diff::{self, FileDiff};
use crate::gitfiles::git;
use crate::worktree::Worktree;
use serde_json::{json, Value};

fn git_text(dir: &Path, args: &[&str]) -> Option<String> {
    git(dir, args).map(|out| String::from_utf8_lossy(&out).into_owned())
}

/// Where a session's changes are read: the top of its tree, and the
/// commit the changes count from. For a session in a worktree that is
/// where its branch left the main tree, so its commits count too; for any
/// other, its tree's last commit. None outside a repository, or in one
/// with no commit yet.
pub fn changes_base(cwd: &Path, w: Option<&Worktree>) -> Option<(PathBuf, String)> {
    let top = match w {
        Some(w) => PathBuf::from(&w.path),
        None => PathBuf::from(git_text(cwd, &["rev-parse", "--show-toplevel"])?.trim()),
    };
    let head = git_text(&top, &["rev-parse", "HEAD"])?.trim().to_string();
    let base = w
        .and_then(|w| git_text(Path::new(&w.main), &["rev-parse", "HEAD"]))
        .and_then(|main| git_text(&top, &["merge-base", main.trim(), &head]))
        .map_or(head, |b| b.trim().to_string());
    Some((top, base))
}

/// The files git does not track yet in `top`, each counted as all added.
pub fn untracked(top: &Path) -> Vec<FileDiff> {
    let others =
        git_text(top, &["ls-files", "-z", "--others", "--exclude-standard"]).unwrap_or_default();
    others
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(|path| {
            let content = std::fs::read(top.join(path)).unwrap_or_default();
            diff::untracked(path, &content)
        })
        .collect()
}

/// Every file changed in `top` since `base`, committed or not, untracked
/// ones included, with the lines each added and removed.
pub fn changed_files(top: &Path, base: &str) -> Option<Vec<FileDiff>> {
    let out = git_text(top, &["diff", "--numstat", "-z", "--no-renames", base])?;
    let mut files = diff::parse_numstat(&out);
    files.extend(untracked(top));
    Some(files)
}

/// One file's diff since `base`, as `git diff` prints it, without colour.
/// A file git does not track is diffed against nothing, all added.
pub fn file_diff(top: &Path, base: &str, path: &str) -> Option<String> {
    let tracked = git_text(
        top,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-renames",
            base,
            "--",
            path,
        ],
    )?;
    if !tracked.is_empty() {
        return Some(tracked);
    }
    let mut cmd = Command::new("git");
    cmd.args([
        "--no-optional-locks",
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--no-index",
        "--",
        "/dev/null",
        path,
    ])
    .current_dir(top)
    .stdin(Stdio::null())
    .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Run from a windowed app, a git call would flash a console.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().ok()?;
    // `git diff --no-index` exits 1 when the files differ, which they do.
    matches!(out.status.code(), Some(0 | 1))
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The most of one file's diff a phone is sent. A generated file or a
/// lock file can be megabytes, and a frame is at most 1 MB.
pub const DIFF_MAX: usize = 200 * 1024;

/// The changes message: each file the session changed, with the lines it
/// added and removed, `null` for both in a binary file.
pub fn changes_message(id: &str, files: &[FileDiff]) -> Value {
    let list: Vec<Value> = files
        .iter()
        .map(|f| {
            let (added, removed) = f.lines.unzip();
            json!({"path": f.path, "added": added, "removed": removed})
        })
        .collect();
    json!({"t": "changes", "id": id, "files": list})
}

/// The diff message for one file: its diff, cut at [`DIFF_MAX`] at the end
/// of a line, and whether it was cut.
pub fn diff_message(id: &str, path: &str, text: &str) -> Value {
    let (text, cut) = cut_diff(text, DIFF_MAX);
    json!({"t": "diff", "id": id, "path": path, "text": text, "cut": cut})
}

/// `text` whole when it fits in `max` bytes, else up to the last line that
/// does, so the phone never draws half a line or half a character.
pub fn cut_diff(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let end = text[..end].rfind('\n').map_or(end, |i| i + 1);
    (&text[..end], true)
}

/// Whether a phone may see `path`'s diff: only a file the session changed,
/// so a path the phone makes up reads nothing else on the PC.
pub fn listed(files: &[FileDiff], path: &str) -> bool {
    files.iter().any(|f| f.path == path)
}

/// The answer to `changes`, or to `diff` for `path`. A session outside a
/// repository has changed nothing a phone can see; a path that is not one
/// of its changed files gets an error, not the file.
pub fn changes_reply(id: &str, cwd: &Path, tree: Option<&Worktree>, path: Option<&str>) -> Value {
    let Some((top, base)) = changes_base(cwd, tree) else {
        return match path {
            None => changes_message(id, &[]),
            Some(_) => json!({"t": "error", "text": "That session is not in a git repository."}),
        };
    };
    let files = changed_files(&top, &base).unwrap_or_default();
    let Some(path) = path else {
        return changes_message(id, &files);
    };
    if !listed(&files, path) {
        return json!({"t": "error", "text": "That file has no changes now."});
    }
    match file_diff(&top, &base, path) {
        Some(text) => diff_message(id, path, &text),
        None => json!({"t": "error", "text": "git could not diff that file."}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(path: &str, lines: Option<(u32, u32)>) -> FileDiff {
        FileDiff {
            path: path.into(),
            lines,
        }
    }

    #[test]
    fn changes_message_counts_lines_and_leaves_binaries_blank() {
        let m = changes_message(
            "s1",
            &[
                changed("src/a.rs", Some((12, 3))),
                changed("logo.png", None),
            ],
        );
        assert_eq!(m["t"], "changes");
        assert_eq!(m["id"], "s1");
        assert_eq!(m["files"][0]["path"], "src/a.rs");
        assert_eq!(m["files"][0]["added"], 12);
        assert_eq!(m["files"][0]["removed"], 3);
        assert!(m["files"][1]["added"].is_null());
        assert!(m["files"][1]["removed"].is_null());
    }

    #[test]
    fn a_long_diff_is_cut_at_the_end_of_a_line() {
        assert_eq!(cut_diff("+a\n+b\n", 100), ("+a\n+b\n", false));
        assert_eq!(cut_diff("+a\n+bcd\n", 5), ("+a\n", true));
        // No line ends in reach: cut at a character, never inside one.
        assert_eq!(cut_diff("+\u{e6}\u{e6}", 2), ("+", true));
        let m = diff_message("s1", "a", &"+x\n".repeat(DIFF_MAX));
        assert_eq!(m["cut"], true);
        assert!(m["text"].as_str().unwrap().len() <= DIFF_MAX);
        assert_eq!(m["path"], "a");
    }

    #[test]
    fn only_a_changed_file_may_be_diffed() {
        let files = [changed("src/a.rs", Some((1, 0)))];
        assert!(listed(&files, "src/a.rs"));
        assert!(!listed(&files, "../secret.txt"));
        assert!(!listed(&files, "src/b.rs"));
    }

    fn run(dir: &Path, args: &[&str]) {
        assert!(git(dir, args).is_some(), "git {args:?}");
    }

    #[test]
    fn a_phone_sees_tracked_and_untracked_changes() {
        let top = std::env::temp_dir().join(format!("horadric-changes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&top);
        std::fs::create_dir_all(top.join("src")).unwrap();
        run(&top, &["init", "-q", "--template="]);
        run(&top, &["config", "user.email", "t@example.com"]);
        run(&top, &["config", "user.name", "t"]);
        std::fs::write(top.join("src/a.txt"), "one\ntwo\n").unwrap();
        run(&top, &["add", "."]);
        run(&top, &["commit", "-q", "-m", "first"]);
        std::fs::write(top.join("src/a.txt"), "one\nthree\nfour\n").unwrap();
        std::fs::write(top.join("new.txt"), "fresh\n").unwrap();

        // Asked from a subfolder, the answer is about the whole tree.
        let (found, base) = changes_base(&top.join("src"), None).expect("a repository");
        let files = changed_files(&found, &base).unwrap();
        let mut paths: Vec<_> = files.iter().map(|f| (f.path.as_str(), f.lines)).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![("new.txt", Some((1, 0))), ("src/a.txt", Some((2, 1)))]
        );

        let tracked = file_diff(&found, &base, "src/a.txt").unwrap();
        assert!(tracked.contains("-two\n+three\n+four\n"), "{tracked}");
        let fresh = file_diff(&found, &base, "new.txt").unwrap();
        assert!(fresh.contains("+fresh"), "{fresh}");

        let outside = std::env::temp_dir().join(format!("horadric-norepo-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        assert!(changes_base(&outside, None).is_none(), "no repository");
        let _ = std::fs::remove_dir_all(&top);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
