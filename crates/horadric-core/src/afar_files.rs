//! A brother's project files, read only: the tree its files tile would show
//! and one file's text, served to a machine showing that project (see
//! docs/PORTAL.md, "Protocol v3: a brother's files").
//!
//! Only what git lists is served, tracked or untracked but never ignored,
//! so what a repository keeps out of git (`.env`, keys, build output) never
//! leaves the machine, and a path made up by the asker reads nothing else.

use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::diff::{FileDiff, RECOUNT_GAP};
use crate::gitfiles::{self, Change};

/// The most files one tree names. A frame is at most 1 MB on the relay,
/// and a tree that long is scrolled, never read whole.
pub const TREE_FILES: usize = 20_000;
/// The most bytes of paths one tree holds, under a frame with room to spare.
pub const TREE_BYTES: usize = 640 * 1024;
/// The most of one file sent. More is cut at the end of a line, and says so.
pub const READ_BYTES: usize = 512 * 1024;

/// The answer to `tree`: the files git knows in the project and how each
/// changed, both cut to fit a frame. A project outside git answers no
/// files and says so, as its files tile would show none.
pub fn tree_message(project: &str, files: &[String], changes: &[(String, Change)]) -> Value {
    let mut bytes = 0;
    let mut cut = false;
    let mut listed = Vec::new();
    for f in files {
        bytes += f.len() + 4;
        if listed.len() >= TREE_FILES || bytes > TREE_BYTES {
            cut = true;
            break;
        }
        listed.push(f.as_str());
    }
    let changes: Vec<Value> = changes
        .iter()
        .take(TREE_FILES)
        .map(|(p, c)| json!([p, c.letter()]))
        .collect();
    json!({"t": "tree", "project": project, "git": true, "files": listed, "changes": changes, "cut": cut})
}

/// The tree of `dir`, read from git, as [`tree_message`] sends it.
pub fn tree(project: &str, dir: &Path) -> Value {
    let Some(prefix) = gitfiles::prefix(dir) else {
        return json!({"t": "tree", "project": project, "git": false, "files": [], "changes": []});
    };
    let files = gitfiles::listed(dir).unwrap_or_default();
    let changes = gitfiles::status(dir, &prefix).unwrap_or_default();
    tree_message(project, &files, &changes)
}

/// What a `tree` answer holds: its project, the files and how they changed,
/// and whether it was cut. None for anything else, or a project not in git.
pub type Tree = (String, Vec<String>, Vec<(String, Change)>, bool);

pub fn heard_tree(msg: &Value) -> Option<Tree> {
    if msg["t"] != "tree" || msg["git"] == false {
        return None;
    }
    let project = msg["project"].as_str()?.to_string();
    let files = msg["files"]
        .as_array()?
        .iter()
        .filter_map(|f| f.as_str().filter(|p| safe(p)).map(str::to_string))
        .collect();
    let changes = msg["changes"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|c| {
                    let path = c[0].as_str().filter(|p| safe(p))?;
                    Some((path.to_string(), Change::of_letter(c[1].as_str()?)?))
                })
                .collect()
        })
        .unwrap_or_default();
    Some((project, files, changes, msg["cut"] == true))
}

/// Whether `path` may be asked for at all: relative, `/` separated, and
/// never climbing out of the project. Checked on both sides, so a brother
/// cannot be made to read outside its project, nor this machine to write
/// outside the folder it keeps what it read.
pub fn safe(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && !path.contains(':')
        && !path.contains('\0')
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// The answer to `read`: the file's text, or why not.
pub fn read(project: &str, dir: &Path, path: &str) -> Value {
    if !safe(path) {
        return refused(project, path, "That is not a file of the project.");
    }
    // Asked of git for this one path, so a file it does not list (ignored,
    // or not there) is never opened.
    let known = gitfiles::git(
        dir,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            path,
        ],
    )
    .map(|out| gitfiles::parse_list(&out));
    if !known.is_some_and(|l| l.iter().any(|p| p == path)) {
        return refused(project, path, "That file is not one git knows.");
    }
    let mut bytes = Vec::new();
    let opened = std::fs::File::open(dir.join(path))
        .and_then(|f| f.take(READ_BYTES as u64 + 1).read_to_end(&mut bytes));
    match opened {
        Ok(_) => file_message(project, path, bytes),
        Err(_) => refused(project, path, "That file cannot be read now."),
    }
}

fn refused(project: &str, path: &str, why: &str) -> Value {
    json!({"t": "error", "for": "read", "project": project, "path": path, "text": why})
}

/// The `file` answer for what was read, at most [`READ_BYTES`] plus one
/// so a longer file is known to be cut. A file that is not text is not
/// sent: the viewer shows text only.
pub fn file_message(project: &str, path: &str, mut bytes: Vec<u8>) -> Value {
    let cut = bytes.len() > READ_BYTES;
    if cut {
        bytes.truncate(READ_BYTES);
        // Back to the end of a line, which is also never inside a character.
        if let Some(end) = bytes.iter().rposition(|&b| b == b'\n') {
            bytes.truncate(end + 1);
        }
    }
    if bytes.contains(&0) {
        return refused(project, path, "That file is not text.");
    }
    match String::from_utf8(bytes) {
        Ok(text) => {
            json!({"t": "file", "project": project, "path": path, "text": text, "cut": cut})
        }
        Err(_) => refused(project, path, "That file is not text."),
    }
}

/// What a `file` answer holds: project, path, text, and whether it was cut.
pub fn heard_file(msg: &Value) -> Option<(String, String, String, bool)> {
    if msg["t"] != "file" {
        return None;
    }
    let path = msg["path"].as_str().filter(|p| safe(p))?;
    Some((
        msg["project"].as_str()?.to_string(),
        path.to_string(),
        msg["text"].as_str()?.to_string(),
        msg["cut"] == true,
    ))
}

/// What a `changes` answer holds: the session, by the brother's id, and
/// each file it changed.
pub fn heard_changes(msg: &Value) -> Option<(String, Vec<FileDiff>)> {
    if msg["t"] != "changes" {
        return None;
    }
    let files = msg["files"]
        .as_array()?
        .iter()
        .filter_map(|f| {
            let path = f["path"].as_str().filter(|p| safe(p))?;
            let lines = f["added"]
                .as_u64()
                .zip(f["removed"].as_u64())
                .map(|(a, r)| (a.min(u32::MAX as u64) as u32, r.min(u32::MAX as u64) as u32));
            Some(FileDiff {
                path: path.to_string(),
                lines,
            })
        })
        .collect();
    Some((msg["id"].as_str()?.to_string(), files))
}

/// When a brother is asked again what a project or a session looks like:
/// when first seen, then after it stirred (its sessions moved on there),
/// never more often than [`RECOUNT_GAP`], one ask at a time. As with a
/// local count nothing polls, so a quiet brother is asked nothing.
#[derive(Debug, Default)]
pub struct Due {
    stir: Option<u64>,
    dirty: bool,
    asked: Option<Instant>,
    waiting: bool,
}

/// How long an ask goes unanswered before it is given up: a brother older
/// than `tree` never answers it, and a dropped link loses the answer.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(30);

impl Due {
    /// Looks at how the brother's sessions stand now, as one number.
    pub fn heard(&mut self, stir: u64) {
        if self.stir != Some(stir) {
            self.dirty = true;
        }
        self.stir = Some(stir);
    }

    /// Whether to ask now. True marks it asked.
    pub fn start(&mut self, now: Instant) -> bool {
        let since = self.asked.map(|t| now.saturating_duration_since(t));
        if self.waiting && since.is_some_and(|d| d < ANSWER_WITHIN) {
            return false;
        }
        if !self.dirty || since.is_some_and(|d| d < RECOUNT_GAP) {
            return false;
        }
        self.dirty = false;
        self.waiting = true;
        self.asked = Some(now);
        true
    }

    /// The answer came.
    pub fn done(&mut self) {
        self.waiting = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tree_reads_back_as_it_was_sent() {
        let files = vec!["src/a.rs".to_string(), "README.md".to_string()];
        let changes = vec![("src/a.rs".to_string(), Change::Modified)];
        let msg = tree_message("/home/me/p", &files, &changes);
        let (project, f, c, cut) = heard_tree(&msg).unwrap();
        assert_eq!(project, "/home/me/p");
        assert_eq!(f, files);
        assert_eq!(c, changes);
        assert!(!cut);
    }

    #[test]
    fn a_tree_too_long_for_a_frame_is_cut_and_says_so() {
        let files: Vec<String> = (0..TREE_FILES + 5).map(|i| format!("f{i}")).collect();
        let msg = tree_message("p", &files, &[]);
        assert_eq!(msg["files"].as_array().unwrap().len(), TREE_FILES);
        assert_eq!(msg["cut"], true);
        let long = vec!["x".repeat(1000); 1000];
        let msg = tree_message("p", &long, &[]);
        assert!(msg.to_string().len() < 1024 * 1024);
        assert_eq!(msg["cut"], true);
    }

    #[test]
    fn a_project_outside_git_has_no_tree() {
        let msg = json!({"t": "tree", "project": "p", "git": false, "files": [], "changes": []});
        assert!(heard_tree(&msg).is_none());
    }

    #[test]
    fn paths_that_climb_out_or_are_absolute_are_never_safe() {
        for bad in [
            "",
            "../x",
            "a/../../x",
            "/etc/passwd",
            "C:/x",
            "a\\b",
            "./a",
            "a/./b",
            "a\0b",
        ] {
            assert!(!safe(bad), "{bad:?}");
        }
        for good in ["a", "src/a.rs", ".github/x.yml", "a b/c"] {
            assert!(safe(good), "{good:?}");
        }
    }

    #[test]
    fn a_tree_from_a_brother_drops_paths_that_are_not_safe() {
        let msg = json!({"t": "tree", "project": "p", "git": true,
            "files": ["ok.rs", "../evil"], "changes": [["../evil", "M"], ["ok.rs", "M"]]});
        let (_, f, c, _) = heard_tree(&msg).unwrap();
        assert_eq!(f, vec!["ok.rs"]);
        assert_eq!(c, vec![("ok.rs".to_string(), Change::Modified)]);
    }

    #[test]
    fn a_long_file_is_cut_at_a_line_and_binary_is_refused() {
        let mut long = "line\n".repeat(READ_BYTES / 5 + 10).into_bytes();
        long.truncate(READ_BYTES + 1);
        let msg = file_message("p", "a.txt", long);
        assert_eq!(msg["cut"], true);
        assert!(msg["text"].as_str().unwrap().ends_with('\n'));
        assert!(msg["text"].as_str().unwrap().len() <= READ_BYTES);
        let msg = file_message("p", "a.png", vec![0x89, b'P', 0, 1]);
        assert_eq!(msg["t"], "error");
        let msg = file_message("p", "a.txt", b"hi\n".to_vec());
        assert_eq!(
            heard_file(&msg),
            Some(("p".into(), "a.txt".into(), "hi\n".into(), false))
        );
    }

    #[test]
    fn a_file_git_does_not_list_is_never_read() {
        let dir = std::env::temp_dir().join(format!("horadric-afar-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        if gitfiles::git(&dir, &["init", "-q"]).is_none() {
            return;
        }
        std::fs::write(dir.join(".gitignore"), ".env\n").unwrap();
        std::fs::write(dir.join(".env"), "SECRET=1\n").unwrap();
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        assert_eq!(read("p", &dir, ".env")["t"], "error");
        assert_eq!(read("p", &dir, "nope.txt")["t"], "error");
        assert_eq!(read("p", &dir, "../x")["t"], "error");
        let ok = read("p", &dir, "a.txt");
        assert_eq!(ok["t"], "file");
        assert_eq!(ok["text"], "hello\n");
        let t = tree("p", &dir);
        let (_, files, changes, _) = heard_tree(&t).unwrap();
        assert!(files.contains(&"a.txt".to_string()));
        assert!(!files.contains(&".env".to_string()));
        assert!(changes.contains(&("a.txt".to_string(), Change::Untracked)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_brother_is_asked_when_it_stirs_and_not_faster_than_the_gap() {
        let t0 = Instant::now();
        let mut d = Due::default();
        d.heard(1);
        assert!(d.start(t0));
        // Still waiting for the answer.
        d.heard(2);
        assert!(!d.start(t0 + Duration::from_secs(5)));
        d.done();
        // Answered, and it stirred meanwhile: asked again, after the gap.
        assert!(!d.start(t0 + Duration::from_secs(1)));
        assert!(d.start(t0 + RECOUNT_GAP));
        d.done();
        // Nothing moved: nothing asked.
        d.heard(2);
        assert!(!d.start(t0 + Duration::from_secs(60)));
    }

    #[test]
    fn an_ask_never_answered_is_given_up() {
        let t0 = Instant::now();
        let mut d = Due::default();
        d.heard(1);
        assert!(d.start(t0));
        d.heard(2);
        assert!(!d.start(t0 + ANSWER_WITHIN - Duration::from_secs(1)));
        assert!(d.start(t0 + ANSWER_WITHIN));
    }

    #[test]
    fn changes_read_back_with_binaries_left_without_lines() {
        let msg = json!({"t": "changes", "id": "s1", "files": [
            {"path": "a.rs", "added": 3, "removed": 1},
            {"path": "b.png", "added": null, "removed": null},
            {"path": "../x", "added": 1, "removed": 0},
        ]});
        let (id, files) = heard_changes(&msg).unwrap();
        assert_eq!(id, "s1");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].lines, Some((3, 1)));
        assert_eq!(files[1].lines, None);
    }
}
