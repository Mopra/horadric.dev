//! What an Edit, Write, MultiEdit or NotebookEdit step changes, for the
//! Folio to show as a diff with its lines added and removed.
//!
//! From the tool's input alone a step knows the strings it swaps, which is
//! enough for a diff and its counts while the step runs or waits on an ask.
//! When its result comes, the CLI's `tool_use_result` carries a
//! `structuredPatch`: hunks with line numbers and context, which replace
//! the guess. A Write over a new file has an empty patch and
//! `"type": "create"`: every line of its content is added.

use serde_json::Value;

/// One file a step changes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FileEdit {
    /// As the tool was given it, usually absolute.
    pub path: String,
    /// The strings swapped, in order. A Write is one change from "" (or
    /// the file before, when its result says) to its content.
    pub changes: Vec<Change>,
    /// The CLI's own hunks, once the step's result came. Empty before, and
    /// for a new file.
    pub hunks: Vec<Hunk>,
    /// The file did not exist before: a Write that created it.
    pub created: bool,
    /// Lines added and removed, from the hunks when there are any, else
    /// from the changes.
    pub added: u32,
    pub removed: u32,
}

/// One swap of text in a file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Change {
    pub old: String,
    pub new: String,
}

/// One hunk of a unified diff, as the CLI's `structuredPatch` gives it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Hunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    /// Each line with its mark first: ' ' kept, '-' removed, '+' added.
    pub lines: Vec<String>,
}

/// One line of a diff made here, from a [`Change`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLine<'a> {
    Same(&'a str),
    Added(&'a str),
    Removed(&'a str),
}

impl FileEdit {
    /// The edit a tool call makes, from its input, or None for a tool that
    /// edits nothing (or an input still streaming in).
    pub fn from_input(tool: &str, input: &Value) -> Option<FileEdit> {
        let s = |v: &Value, k: &str| v[k].as_str().unwrap_or_default().to_string();
        let (path, changes) = match tool {
            "Edit" => (
                s(input, "file_path"),
                vec![Change {
                    old: s(input, "old_string"),
                    new: s(input, "new_string"),
                }],
            ),
            "MultiEdit" => (
                s(input, "file_path"),
                input["edits"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|e| Change {
                        old: s(e, "old_string"),
                        new: s(e, "new_string"),
                    })
                    .collect(),
            ),
            "Write" => (
                s(input, "file_path"),
                vec![Change {
                    old: String::new(),
                    new: s(input, "content"),
                }],
            ),
            "NotebookEdit" => (
                s(input, "notebook_path"),
                vec![Change {
                    old: String::new(),
                    new: s(input, "new_source"),
                }],
            ),
            _ => return None,
        };
        if path.is_empty() {
            return None;
        }
        let mut edit = FileEdit {
            path,
            changes,
            ..FileEdit::default()
        };
        edit.count();
        Some(edit)
    }

    /// Takes in the step's `tool_use_result`: the CLI's hunks, and whether
    /// the file is new.
    pub fn apply_result(&mut self, result: &Value) {
        if result["type"] == "create" {
            self.created = true;
        }
        self.hunks = result["structuredPatch"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|h| {
                let n = |k: &str| h[k].as_u64().unwrap_or_default() as u32;
                Hunk {
                    old_start: n("oldStart"),
                    old_lines: n("oldLines"),
                    new_start: n("newStart"),
                    new_lines: n("newLines"),
                    lines: h["lines"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|l| l.as_str().map(str::to_string))
                        .collect(),
                }
            })
            .collect();
        // The whole file before is kept only when no hunks say what
        // changed, since it can be large and a long session has many.
        if self.hunks.is_empty() {
            if let Some(before) = result["originalFile"].as_str() {
                if self.changes.len() == 1 && self.changes[0].old.is_empty() {
                    self.changes[0].old = before.to_string();
                }
            }
        }
        self.count();
    }

    /// Sets `added` and `removed`: from the hunks when the CLI gave any,
    /// else by diffing each change here.
    fn count(&mut self) {
        let (mut added, mut removed) = (0, 0);
        if self.hunks.is_empty() {
            for c in &self.changes {
                for l in line_diff(&c.old, &c.new) {
                    match l {
                        DiffLine::Added(_) => added += 1,
                        DiffLine::Removed(_) => removed += 1,
                        DiffLine::Same(_) => {}
                    }
                }
            }
        } else {
            for l in self.hunks.iter().flat_map(|h| &h.lines) {
                match l.as_bytes().first() {
                    Some(b'+') => added += 1,
                    Some(b'-') => removed += 1,
                    _ => {}
                }
            }
        }
        self.added = added;
        self.removed = removed;
    }

    /// The file's last path part, either slash.
    pub fn file_name(&self) -> &str {
        self.path.rsplit(['/', '\\']).next().unwrap_or(&self.path)
    }

    /// "+12 -3", the step's one line count.
    pub fn counts(&self) -> String {
        format!("+{} -{}", self.added, self.removed)
    }
}

/// The lines of `old` and `new` as a diff, shortest edit first by a plain
/// longest common subsequence. Past a size where that costs too much, the
/// middle that differs shows as all removed then all added.
pub fn line_diff<'a>(old: &'a str, new: &'a str) -> Vec<DiffLine<'a>> {
    const MAX_CELLS: usize = 4_000_000;
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (am, bm) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);
    let mut out: Vec<DiffLine> = a[..head].iter().map(|l| DiffLine::Same(l)).collect();
    if am.len().saturating_mul(bm.len()) > MAX_CELLS {
        out.extend(am.iter().map(|l| DiffLine::Removed(l)));
        out.extend(bm.iter().map(|l| DiffLine::Added(l)));
    } else {
        // lcs[i][j]: the longest common run of am[i..] and bm[j..].
        let w = bm.len() + 1;
        let mut lcs = vec![0u32; (am.len() + 1) * w];
        for i in (0..am.len()).rev() {
            for j in (0..bm.len()).rev() {
                lcs[i * w + j] = if am[i] == bm[j] {
                    lcs[(i + 1) * w + j + 1] + 1
                } else {
                    lcs[(i + 1) * w + j].max(lcs[i * w + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < am.len() && j < bm.len() {
            if am[i] == bm[j] {
                out.push(DiffLine::Same(am[i]));
                i += 1;
                j += 1;
            } else if lcs[(i + 1) * w + j] >= lcs[i * w + j + 1] {
                out.push(DiffLine::Removed(am[i]));
                i += 1;
            } else {
                out.push(DiffLine::Added(bm[j]));
                j += 1;
            }
        }
        out.extend(am[i..].iter().map(|l| DiffLine::Removed(l)));
        out.extend(bm[j..].iter().map(|l| DiffLine::Added(l)));
    }
    out.extend(a[a.len() - tail..].iter().map(|l| DiffLine::Same(l)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_edit_counts_its_lines_from_its_strings() {
        let e = FileEdit::from_input(
            "Edit",
            &json!({"file_path": "C:\\p\\main.rs", "old_string": "a\nb\nc", "new_string": "a\nB\nc\nd"}),
        )
        .unwrap();
        assert_eq!(e.file_name(), "main.rs");
        assert_eq!((e.added, e.removed), (2, 1));
        assert_eq!(e.counts(), "+2 -1");
        assert!(e.hunks.is_empty());
    }

    #[test]
    fn a_multi_edit_counts_every_change() {
        let e = FileEdit::from_input(
            "MultiEdit",
            &json!({"file_path": "/a/b.rs", "edits": [
                {"old_string": "x", "new_string": "y"},
                {"old_string": "p\nq", "new_string": "p"},
            ]}),
        )
        .unwrap();
        assert_eq!(e.changes.len(), 2);
        assert_eq!((e.added, e.removed), (1, 2));
    }

    #[test]
    fn a_write_adds_its_lines_and_its_result_says_it_is_new() {
        let mut e = FileEdit::from_input(
            "Write",
            &json!({"file_path": "/a/h.txt", "content": "hi\nthere\n"}),
        )
        .unwrap();
        assert_eq!((e.added, e.removed), (2, 0));
        e.apply_result(&json!({"type": "create", "structuredPatch": [], "originalFile": null}));
        assert!(e.created);
        assert_eq!((e.added, e.removed), (2, 0));
    }

    #[test]
    fn the_cli_s_hunks_replace_the_guess() {
        let mut e = FileEdit::from_input(
            "Edit",
            &json!({"file_path": "m.rs", "old_string": "x", "new_string": "y"}),
        )
        .unwrap();
        e.apply_result(
            &json!({"structuredPatch": [{"oldStart": 1, "oldLines": 3, "newStart": 1,
            "newLines": 4, "lines": [" a", "-b", "+B", "+C", " d"]}]}),
        );
        assert_eq!(e.hunks.len(), 1);
        assert_eq!(e.hunks[0].new_lines, 4);
        assert_eq!((e.added, e.removed), (2, 1));
    }

    #[test]
    fn a_write_keeps_the_file_before_only_without_hunks() {
        let write = || {
            FileEdit::from_input(
                "Write",
                &json!({"file_path": "/a/h.txt", "content": "new\n"}),
            )
            .unwrap()
        };
        let mut e = write();
        e.apply_result(&json!({"structuredPatch": [{"oldStart": 1, "oldLines": 1,
            "newStart": 1, "newLines": 1, "lines": ["-old", "+new"]}], "originalFile": "old\n"}));
        assert!(e.changes[0].old.is_empty());
        let mut e = write();
        e.apply_result(&json!({"structuredPatch": [], "originalFile": "old\n"}));
        assert_eq!(e.changes[0].old, "old\n");
    }

    #[test]
    fn tools_that_edit_nothing_have_no_edit() {
        assert!(FileEdit::from_input("Read", &json!({"file_path": "x"})).is_none());
        assert!(FileEdit::from_input("Edit", &json!({})).is_none());
    }

    #[test]
    fn the_diff_keeps_common_lines() {
        use DiffLine::*;
        assert_eq!(
            line_diff("a\nb\nc\nd", "a\nc\nx\nd"),
            [Same("a"), Removed("b"), Same("c"), Added("x"), Same("d")]
        );
        assert_eq!(line_diff("", "x"), [Added("x")]);
        assert_eq!(line_diff("x", ""), [Removed("x")]);
        assert!(line_diff("same", "same") == [Same("same")]);
    }
}
