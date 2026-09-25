//! What a session's worktree has changed: each file with the lines it
//! added and removed, split into what is not committed yet and what is
//! committed on the session's branch but not in the main tree. Read from
//! `git diff --numstat`; this is the part that makes sense of it.

use std::time::{Duration, Instant, SystemTime};

/// One changed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    /// Relative to the worktree's top, with forward slashes.
    pub path: String,
    /// Lines added and removed, None for a binary file, which git does not
    /// count in lines.
    pub lines: Option<(u32, u32)>,
}

/// Everything a worktree changed, as last counted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    /// In the working tree and the index, against the branch's last commit.
    /// Untracked files count as added.
    pub uncommitted: Vec<FileDiff>,
    /// Committed on the branch since it left the main tree.
    pub committed: Vec<FileDiff>,
}

impl Diff {
    /// Lines added and removed, committed or not.
    pub fn totals(&self) -> (u32, u32) {
        self.uncommitted
            .iter()
            .chain(&self.committed)
            .filter_map(|f| f.lines)
            .fold((0, 0), |(a, r), (fa, fr)| (a + fa, r + fr))
    }

    pub fn is_empty(&self) -> bool {
        self.uncommitted.is_empty() && self.committed.is_empty()
    }
}

/// The least time between two counts of one worktree. A busy agent fires a
/// few hooks a second, and each count starts git three times.
pub const RECOUNT_GAP: Duration = Duration::from_secs(3);

/// When a worktree's changes are counted again: when first seen, then
/// after the agent did something, never more often than [`RECOUNT_GAP`].
/// Nothing polls, so a quiet session costs nothing. What changes without
/// the agent, an edit in VS Code, shows at its next hook or menu.
#[derive(Debug, Default)]
pub struct Recount {
    /// The agent's last activity when last looked at.
    heard: Option<SystemTime>,
    last: Option<Instant>,
    dirty: bool,
    running: bool,
}

impl Recount {
    /// Looks at what the agent did last. A count is due when it did more.
    pub fn heard(&mut self, activity: Option<SystemTime>) {
        if self.last.is_none() || activity != self.heard {
            self.dirty = true;
        }
        self.heard = activity;
    }

    /// Whether to start a count now. True marks it running.
    pub fn start(&mut self, now: Instant) -> bool {
        let soon = self
            .last
            .is_some_and(|t| now.saturating_duration_since(t) < RECOUNT_GAP);
        if !self.dirty || self.running || soon {
            return false;
        }
        self.dirty = false;
        self.running = true;
        self.last = Some(now);
        true
    }

    /// A count came back.
    pub fn done(&mut self) {
        self.running = false;
    }
}

/// Reads `git diff --numstat -z --no-renames`: a record a file, each
/// `added TAB removed TAB path` ended by a NUL, `-` for both counts of a
/// binary file.
pub fn parse_numstat(out: &str) -> Vec<FileDiff> {
    out.split('\0')
        .filter_map(|record| {
            let mut parts = record.splitn(3, '\t');
            let added = parts.next()?.trim_start_matches('\n');
            let removed = parts.next()?;
            let path = parts.next().filter(|p| !p.is_empty())?;
            let lines = added.parse().ok().zip(removed.parse().ok());
            Some(FileDiff {
                path: path.replace('\\', "/"),
                lines,
            })
        })
        .collect()
}

/// A file git does not track yet, as a diff: all its lines added. None for
/// the lines when it holds a NUL, which is how git tells a binary file.
pub fn untracked(path: &str, content: &[u8]) -> FileDiff {
    let lines = (!content.contains(&0)).then(|| {
        let breaks = content.iter().filter(|&&b| b == b'\n').count() as u32;
        let unended = content.last().is_some_and(|&b| b != b'\n');
        (breaks + u32::from(unended), 0)
    });
    FileDiff {
        path: path.replace('\\', "/"),
        lines,
    }
}

/// Lines added and removed the way a tile shows them, `+12 −3`.
pub fn glance((added, removed): (u32, u32)) -> String {
    format!("+{added} \u{2212}{removed}")
}

/// A file's line in a menu: its path, then its counts after a tab, which a
/// Windows menu puts in a column of its own at the right. An `&` is
/// doubled, or the menu would underline the letter after it.
pub fn menu_label(f: &FileDiff) -> String {
    let path = f.path.replace('&', "&&");
    match f.lines {
        Some(lines) => format!("{path}\t{}", glance(lines)),
        None => format!("{path}\tbinary"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, lines: Option<(u32, u32)>) -> FileDiff {
        FileDiff {
            path: path.into(),
            lines,
        }
    }

    #[test]
    fn numstat_reads_counts_paths_and_binaries() {
        let out = "3\t1\tsrc/main.rs\x0012\t0\tdocs/a b.md\x00-\t-\tlogo.png\x00";
        assert_eq!(
            parse_numstat(out),
            vec![
                file("src/main.rs", Some((3, 1))),
                file("docs/a b.md", Some((12, 0))),
                file("logo.png", None),
            ]
        );
    }

    #[test]
    fn numstat_of_nothing_is_empty() {
        assert!(parse_numstat("").is_empty());
        assert!(parse_numstat("\n").is_empty());
    }

    #[test]
    fn an_untracked_file_counts_its_lines_as_added() {
        assert_eq!(untracked("a.txt", b"one\ntwo\n").lines, Some((2, 0)));
        assert_eq!(untracked("a.txt", b"one\ntwo").lines, Some((2, 0)));
        assert_eq!(untracked("a.txt", b"").lines, Some((0, 0)));
        assert_eq!(untracked("a.bin", b"MZ\0\0").lines, None);
        assert_eq!(untracked("web\\a.txt", b"x").path, "web/a.txt");
    }

    #[test]
    fn totals_add_up_both_halves_and_skip_binaries() {
        let d = Diff {
            uncommitted: vec![file("a", Some((3, 1))), file("b.png", None)],
            committed: vec![file("c", Some((10, 4)))],
        };
        assert_eq!(d.totals(), (13, 5));
        assert!(!d.is_empty());
        assert!(Diff::default().is_empty());
    }

    #[test]
    fn a_worktree_is_counted_first_then_after_the_agent_did_something() {
        let t0 = Instant::now();
        let mut r = Recount::default();
        r.heard(None);
        assert!(r.start(t0), "first seen");
        assert!(!r.start(t0), "already running");
        r.done();
        r.heard(None);
        assert!(!r.start(t0 + RECOUNT_GAP * 2), "nothing happened since");

        let busy = SystemTime::now();
        r.heard(Some(busy));
        assert!(!r.start(t0 + Duration::from_secs(1)), "too soon");
        assert!(r.start(t0 + RECOUNT_GAP), "the gap is over");
    }

    #[test]
    fn activity_during_a_count_brings_another_after_it() {
        let t0 = Instant::now();
        let mut r = Recount::default();
        r.heard(None);
        assert!(r.start(t0));
        r.heard(Some(SystemTime::now()));
        assert!(!r.start(t0 + RECOUNT_GAP), "still running");
        r.done();
        assert!(r.start(t0 + RECOUNT_GAP));
    }

    #[test]
    fn a_menu_line_puts_the_counts_in_their_own_column() {
        assert_eq!(
            menu_label(&file("src/app.rs", Some((12, 3)))),
            "src/app.rs\t+12 \u{2212}3"
        );
        assert_eq!(menu_label(&file("logo.png", None)), "logo.png\tbinary");
        assert_eq!(menu_label(&file("R&D.md", None)), "R&&D.md\tbinary");
        assert_eq!(glance((0, 0)), "+0 \u{2212}0");
    }
}
