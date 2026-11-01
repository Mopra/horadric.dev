//! What git says about a project's files: which it knows and how each
//! changed. The files tile reads it from the disk it runs on, and a brother
//! serves the same to a machine showing one of its projects (`afar_files`),
//! so both read git alike.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What git says about one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Change {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflict,
}

impl Change {
    /// The letter VS Code puts beside the name.
    pub fn letter(self) -> &'static str {
        match self {
            Change::Modified => "M",
            Change::Added => "A",
            Change::Deleted => "D",
            Change::Renamed => "R",
            Change::Untracked => "U",
            Change::Conflict => "C",
        }
    }

    /// The change a [`letter`](Change::letter) stands for, as a brother
    /// sends it.
    pub fn of_letter(letter: &str) -> Option<Change> {
        Some(match letter {
            "M" => Change::Modified,
            "A" => Change::Added,
            "D" => Change::Deleted,
            "R" => Change::Renamed,
            "U" => Change::Untracked,
            "C" => Change::Conflict,
            _ => return None,
        })
    }

    /// Which change a folder shows when it holds several. A conflict blocks
    /// the next commit, so it wins; an untracked file is the least news.
    pub fn rank(self) -> u8 {
        match self {
            Change::Conflict => 5,
            Change::Modified => 4,
            Change::Deleted => 3,
            Change::Renamed => 2,
            Change::Added => 1,
            Change::Untracked => 0,
        }
    }

    fn from_xy(x: u8, y: u8) -> Option<Change> {
        Some(match (x, y) {
            (b'?', b'?') => Change::Untracked,
            (b'!', b'!') => return None,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => Change::Conflict,
            (b'R' | b'C', _) | (_, b'R' | b'C') => Change::Renamed,
            (_, b'D') | (b'D', _) => Change::Deleted,
            (b'A', _) => Change::Added,
            _ => Change::Modified,
        })
    }
}

/// Splits NUL separated output, as every `-z` git command prints it.
pub fn parse_list(out: &[u8]) -> Vec<String> {
    out.split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect()
}

/// Reads `git status --porcelain=v1 -z`. Its paths are relative to the
/// repository root, so `prefix` (from `git rev-parse --show-prefix`, with
/// its trailing slash) is taken off, and anything outside it is dropped.
pub fn parse_status(out: &[u8], prefix: &str) -> Vec<(String, Change)> {
    let mut changes = Vec::new();
    let mut fields = out.split(|&b| b == 0);
    while let Some(f) = fields.next() {
        if f.len() < 4 {
            continue;
        }
        let (x, y) = (f[0], f[1]);
        // A rename or copy is followed by the path it came from.
        if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') {
            fields.next();
        }
        let Some(change) = Change::from_xy(x, y) else {
            continue;
        };
        let path = String::from_utf8_lossy(&f[3..]);
        if let Some(rest) = path.strip_prefix(prefix) {
            // An untracked folder is reported as one entry ending in a slash
            // unless every file was asked for; either way it is a folder.
            let rest = rest.trim_end_matches('/');
            if !rest.is_empty() {
                changes.push((rest.to_string(), change));
            }
        }
    }
    changes
}

/// Runs git in `dir`, what it printed when it succeeded. It never takes
/// git's optional locks: a plain `git status` may rewrite the index, which
/// a watcher of the folder would hear as a change, and scan again forever.
pub fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.arg("--no-optional-locks")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Run from a windowed app, every git call would flash a console.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().ok()?;
    out.status.success().then_some(out.stdout)
}

/// The project folder's place in its repository, `""` at the top, else
/// ending in a slash. None outside git.
pub fn prefix(dir: &Path) -> Option<String> {
    let out = git(dir, &["rev-parse", "--show-prefix"])?;
    Some(String::from_utf8_lossy(&out).trim_end().to_string())
}

/// Every file git knows in `dir`, tracked or untracked but not ignored,
/// relative to it.
pub fn listed(dir: &Path) -> Option<Vec<String>> {
    git(
        dir,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )
    .map(|out| parse_list(&out))
}

/// How each changed file in `dir` changed, relative to it.
pub fn status(dir: &Path, prefix: &str) -> Option<Vec<(String, Change)>> {
    git(
        dir,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            ".",
        ],
    )
    .map(|out| parse_status(&out, prefix))
}

/// What git ignores in `dir`, a folder ending in a slash, for a watcher
/// to skip: every `cargo build` would otherwise rescan a hundred times.
pub fn ignored(dir: &Path) -> Vec<String> {
    git(
        dir,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ],
    )
    .map(|out| parse_list(&out))
    .unwrap_or_default()
}

/// The repository's git folder when it is not the project's own `.git`:
/// the project is a subfolder of the repository, or a worktree, whose
/// `.git` is a file pointing into the main repository. A watcher watches
/// it too, or staging and committing would go unseen.
pub fn outside_git_dir(dir: &Path) -> Option<PathBuf> {
    let out = git(dir, &["rev-parse", "--absolute-git-dir"])?;
    let git_dir = PathBuf::from(String::from_utf8_lossy(&out).trim_end());
    let git_dir = std::fs::canonicalize(git_dir).ok()?;
    let own = std::fs::canonicalize(dir.join(".git")).ok();
    (own.as_ref() != Some(&git_dir)).then_some(git_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_reads_every_kind_and_skips_the_rename_source() {
        let out = b" M src/a.rs\0A  src/b.rs\0R  src/new.rs\0src/old.rs\0?? notes.md\0 D gone.rs\0UU both.rs\0";
        let got = parse_status(out, "");
        assert_eq!(
            got,
            vec![
                ("src/a.rs".into(), Change::Modified),
                ("src/b.rs".into(), Change::Added),
                ("src/new.rs".into(), Change::Renamed),
                ("notes.md".into(), Change::Untracked),
                ("gone.rs".into(), Change::Deleted),
                ("both.rs".into(), Change::Conflict),
            ]
        );
    }

    #[test]
    fn status_is_made_relative_to_the_project_folder() {
        let out = b" M app/src/a.rs\0 M other/b.rs\0";
        assert_eq!(
            parse_status(out, "app/"),
            vec![("src/a.rs".into(), Change::Modified)]
        );
    }

    #[test]
    fn list_splits_on_nul() {
        assert_eq!(parse_list(b"a\0b/c\0"), vec!["a", "b/c"]);
        assert!(parse_list(b"").is_empty());
    }

    #[test]
    fn every_letter_reads_back_as_its_change() {
        for c in [
            Change::Modified,
            Change::Added,
            Change::Deleted,
            Change::Renamed,
            Change::Untracked,
            Change::Conflict,
        ] {
            assert_eq!(Change::of_letter(c.letter()), Some(c));
        }
        assert_eq!(Change::of_letter("Z"), None);
    }
}
