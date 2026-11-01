//! Line comments on an edit under review: the human picks a row of its
//! diff, or a run of rows, says something about it, and once there are
//! some sends them all back as the denial, each quoting the lines it is
//! about with where they are in the file, so Claude can revise.
//!
//! Shared by both faces: Windows draws the diff as rows with numbers to
//! click, the Mac lists them in its transcript and takes the rows in a
//! dialog. The model, the rows and the denial are the same.

use std::path::Path;

use horadric_core::folio::{line_diff, Block, Change, DiffLine, FileEdit};

/// The most of a comment's rows quoted back. A comment on a whole Write
/// would otherwise send the file again.
const QUOTE_ROWS: usize = 12;

/// One comment on rows `from..=to` of the edit's diff rows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Comment {
    pub from: usize,
    pub to: usize,
    pub text: String,
}

/// The comments on one edit ask, and the rows of the one being written.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Review {
    pub comments: Vec<Comment>,
    /// The row first picked and the row it was stretched to, in the order
    /// clicked, so Shift+click either side of the first works.
    pub draft: Option<(usize, usize)>,
}

impl Review {
    /// A click on a row's number: a comment on it starts, or with
    /// `extend` the one being written stretches to it.
    pub fn pick(&mut self, row: usize, extend: bool) {
        self.draft = match (self.draft, extend) {
            (Some((first, _)), true) => Some((first, row)),
            _ => Some((row, row)),
        };
    }

    /// The rows of the comment being written, first to last.
    pub fn span(&self) -> Option<(usize, usize)> {
        self.draft.map(|(a, b)| (a.min(b), a.max(b)))
    }

    /// Keeps the comment being written. False, and it stays open, when
    /// there is nothing in it.
    pub fn add(&mut self, text: &str) -> bool {
        let text = text.trim();
        let Some((from, to)) = self.span() else {
            return false;
        };
        if text.is_empty() {
            return false;
        }
        let at = (self.comments.iter())
            .position(|c| c.from > from)
            .unwrap_or(self.comments.len());
        let text = text.to_string();
        self.comments.insert(at, Comment { from, to, text });
        self.draft = None;
        true
    }

    pub fn remove(&mut self, k: usize) {
        if k < self.comments.len() {
            self.comments.remove(k);
        }
    }

    /// Whether `row` is under a comment kept or being written.
    pub fn marks(&self, row: usize) -> bool {
        let under = |a: usize, b: usize| (a..=b).contains(&row);
        self.span().is_some_and(|(a, b)| under(a, b))
            || self.comments.iter().any(|c| under(c.from, c.to))
    }

    pub fn is_empty(&self) -> bool {
        self.comments.is_empty()
    }
}

/// Per change of an edit, how many lines of the file sit above it, found
/// by applying the changes in turn to `file` as it is now. None where a
/// change's old text is not found, or there is none to find.
pub fn lines_before(file: &str, changes: &[Change]) -> Vec<Option<u32>> {
    let mut text = file.replace("\r\n", "\n");
    let mut out = Vec::new();
    for c in changes {
        let old = c.old.replace("\r\n", "\n");
        let at = (!old.is_empty()).then(|| text.find(&old)).flatten();
        out.push(at.map(|at| text[..at].matches('\n').count() as u32));
        if let Some(at) = at {
            let new = c.new.replace("\r\n", "\n");
            text.replace_range(at..at + old.len(), &new);
        }
    }
    out
}

/// Where rows `from..=to` are, as Claude is told: "Line 42", "Lines 42
/// to 44", or for rows only removed, "Removed lines 42 to 43". `before`
/// holds, per change (the rows between gap rows), the lines of the file
/// above it; where that is not known the numbers count within the edit.
pub fn place(rows: &[DiffRow], from: usize, to: usize, before: &[Option<u32>]) -> String {
    let to = to.min(rows.len().saturating_sub(1));
    let segment = |k: usize| rows[..k].iter().filter(|r| r.mark == '~').count();
    let span = |pick: fn(&DiffRow) -> Option<u32>| {
        let mut known = true;
        let ns: Vec<u32> = (from..=to)
            .filter_map(|k| {
                let n = pick(rows.get(k)?)?;
                match before.get(segment(k)).copied().flatten() {
                    Some(b) => Some(b + n),
                    None => {
                        known = false;
                        Some(n)
                    }
                }
            })
            .collect();
        Some((*ns.iter().min()?, *ns.iter().max()?, known))
    };
    let (removed, found) = match span(|r| r.new) {
        Some(s) => ("", Some(s)),
        None => ("Removed l", span(|r| r.old)),
    };
    let Some((a, b, known)) = found else {
        return "Between changes".into();
    };
    let start = if removed.is_empty() { "L" } else { removed };
    let lines = match a == b {
        true => format!("{start}ine {a}"),
        false => format!("{start}ines {a} to {b}"),
    };
    match known {
        true => lines,
        false => format!("{lines} of the edit's text"),
    }
}

/// Where rows `from..=to` are as the card numbers them, for the human.
pub fn shown_place(rows: &[DiffRow], from: usize, to: usize) -> String {
    let changes = rows.iter().filter(|r| r.mark == '~').count() + 1;
    place(rows, from, to, &vec![Some(0); changes])
}

/// The denial Claude gets: what happened, then each comment with where
/// it is, the rows it is on quoted, and what the human said, then
/// anything the human typed as the reason. `path` is as the human sees it.
pub fn reason(
    path: &str,
    rows: &[DiffRow],
    before: &[Option<u32>],
    comments: &[Comment],
    extra: &str,
) -> String {
    let mut out = format!(
        "Not applied. The human reviewed this edit to {path} and left comments on its lines. \
         Revise the edit with each in mind, then make it again.\n"
    );
    for c in comments {
        let to = c.to.min(rows.len().saturating_sub(1));
        out.push('\n');
        out.push_str(&place(rows, c.from, to, before));
        out.push_str(":\n");
        let quoted: Vec<&DiffRow> = (rows.get(c.from..=to).into_iter().flatten())
            .filter(|r| r.mark != '~')
            .collect();
        for r in quoted.iter().take(QUOTE_ROWS) {
            out.push_str(&format!("> {} {}\n", r.mark, r.text));
        }
        if quoted.len() > QUOTE_ROWS {
            let more = quoted.len() - QUOTE_ROWS;
            out.push_str(&format!("> ({more} more lines)\n"));
        }
        out.push_str(c.text.trim());
        out.push('\n');
    }
    let extra = extra.trim();
    if !extra.is_empty() {
        out.push_str(&format!("\nAlso: {extra}\n"));
    }
    out.trim_end().to_string()
}

/// One row of a diff as shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow {
    /// Line numbers before and after; None on the side it is not on.
    pub old: Option<u32>,
    pub new: Option<u32>,
    /// '+', '-', ' ', or '~' for the gap between hunks.
    pub mark: char,
    pub text: String,
}

/// An edit's diff as rows, from the CLI's hunks when it gave them, else
/// from the strings the tool swaps (numbered from 1, since where they sit
/// in the file is not known).
pub fn diff_rows(edit: &FileEdit) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    if !edit.hunks.is_empty() {
        for (k, h) in edit.hunks.iter().enumerate() {
            if k > 0 {
                rows.push(gap_row());
            }
            let (mut o, mut n) = (h.old_start, h.new_start);
            for l in &h.lines {
                let mut cs = l.chars();
                let mark = cs.next().unwrap_or(' ');
                let text = cs.as_str().to_string();
                let (old, new) = match mark {
                    '+' => {
                        n += 1;
                        (None, Some(n - 1))
                    }
                    '-' => {
                        o += 1;
                        (Some(o - 1), None)
                    }
                    _ => {
                        o += 1;
                        n += 1;
                        (Some(o - 1), Some(n - 1))
                    }
                };
                let mark = if matches!(mark, '+' | '-') { mark } else { ' ' };
                rows.push(DiffRow {
                    old,
                    new,
                    mark,
                    text,
                });
            }
        }
        return rows;
    }
    for (k, c) in edit.changes.iter().enumerate() {
        if k > 0 {
            rows.push(gap_row());
        }
        let (mut o, mut n) = (1, 1);
        for l in line_diff(&c.old, &c.new) {
            let (old, new, mark, text) = match l {
                DiffLine::Same(t) => {
                    o += 1;
                    n += 1;
                    (Some(o - 1), Some(n - 1), ' ', t)
                }
                DiffLine::Removed(t) => {
                    o += 1;
                    (Some(o - 1), None, '-', t)
                }
                DiffLine::Added(t) => {
                    n += 1;
                    (None, Some(n - 1), '+', t)
                }
            };
            rows.push(DiffRow {
                old,
                new,
                mark,
                text: text.to_string(),
            });
        }
    }
    rows
}

fn gap_row() -> DiffRow {
    DiffRow {
        old: None,
        new: None,
        mark: '~',
        text: String::new(),
    }
}


/// The denial for the comments `review` holds on the edit ask
/// `request_id` of `blocks`, with `extra` typed as the reason, or None
/// when it has none. Reads the file once, to say where in it each
/// comment is. `shown` gives the path as the human sees it.
pub fn edit_reason(
    blocks: &[Block],
    request_id: &str,
    review: &Review,
    extra: &str,
    shown: impl FnOnce(&str) -> Option<String>,
) -> Option<String> {
    if review.is_empty() {
        return None;
    }
    let (tool, input) = blocks.iter().find_map(|b| match b {
        Block::Ask {
            request_id: r,
            tool,
            input,
            ..
        } if r == request_id => Some((tool, input)),
        _ => None,
    })?;
    let e = FileEdit::from_input(tool, input)?;
    let rows = diff_rows(&e);
    let before = if !e.hunks.is_empty() {
        vec![Some(0); e.hunks.len()]
    } else {
        match tool.as_str() {
            "Write" => vec![Some(0)],
            "Edit" | "MultiEdit" => match std::fs::read_to_string(Path::new(&e.path)) {
                Ok(file) => lines_before(&file, &e.changes),
                Err(_) => vec![None; e.changes.len()],
            },
            _ => vec![None; e.changes.len()],
        }
    };
    let path = shown(&e.path).unwrap_or_else(|| e.path.clone());
    Some(reason(&path, &rows, &before, &review.comments, extra))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(old: Option<u32>, new: Option<u32>, mark: char, text: &str) -> DiffRow {
        DiffRow {
            old,
            new,
            mark,
            text: text.into(),
        }
    }

    fn rows() -> Vec<DiffRow> {
        vec![
            row(Some(1), Some(1), ' ', "fn main() {"),
            row(Some(2), None, '-', "    let x = 1;"),
            row(None, Some(2), '+', "    let x = 2;"),
            row(Some(3), Some(3), ' ', "}"),
            row(None, None, '~', ""),
            row(Some(1), None, '-', "old"),
        ]
    }

    #[test]
    fn picking_stretches_either_way_and_adding_keeps_order() {
        let mut r = Review::default();
        r.pick(3, false);
        r.pick(1, true);
        assert_eq!(r.span(), Some((1, 3)));
        assert!(r.marks(2) && !r.marks(0));
        assert!(!r.add("  "), "an empty comment is not kept");
        assert!(r.add("Why 2?"));
        assert_eq!(r.draft, None);
        r.pick(0, true);
        assert!(r.add("First"));
        let froms: Vec<usize> = r.comments.iter().map(|c| c.from).collect();
        assert_eq!(froms, vec![0, 1]);
        r.remove(0);
        assert_eq!(r.comments[0].text, "Why 2?");
    }

    #[test]
    fn places_count_from_the_file_when_known() {
        let rows = rows();
        let before = [Some(40), None];
        assert_eq!(place(&rows, 2, 2, &before), "Line 42");
        assert_eq!(place(&rows, 0, 3, &before), "Lines 41 to 43");
        assert_eq!(place(&rows, 1, 1, &before), "Removed line 42");
        assert_eq!(place(&rows, 5, 5, &before), "Removed line 1 of the edit's text");
        assert_eq!(place(&rows, 4, 4, &before), "Between changes");
    }

    #[test]
    fn lines_before_follows_earlier_changes() {
        let file = "a\nb\nc\nd\n";
        let changes = [
            Change {
                old: "b\n".into(),
                new: "b\nb2\nb3\n".into(),
            },
            Change {
                old: "d".into(),
                new: "D".into(),
            },
            Change {
                old: "zzz".into(),
                new: "y".into(),
            },
        ];
        assert_eq!(lines_before(file, &changes), vec![Some(1), Some(5), None]);
    }

    #[test]
    fn reason_quotes_each_comment_with_its_place() {
        let rows = rows();
        let comments = vec![
            Comment {
                from: 1,
                to: 2,
                text: "Keep it 1.".into(),
            },
            Comment {
                from: 3,
                to: 3,
                text: " Fine. ".into(),
            },
        ];
        let got = reason("src/main.rs", &rows, &[Some(9), None], &comments, "and run the tests");
        let want = "Not applied. The human reviewed this edit to src/main.rs and left comments on its lines. \
                    Revise the edit with each in mind, then make it again.\n\
                    \n\
                    Line 11:\n\
                    > -     let x = 1;\n\
                    > +     let x = 2;\n\
                    Keep it 1.\n\
                    \n\
                    Line 12:\n\
                    >   }\n\
                    Fine.\n\
                    \n\
                    Also: and run the tests";
        assert_eq!(got, want);
    }
}
