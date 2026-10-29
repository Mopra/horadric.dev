//! What the transcript shows, worked out without a window: a reply's
//! markdown as a flat run of paragraphs to lay out, a table's columns,
//! where a selection starts and ends in each piece of text, what a find
//! matches, the diff of an edit as rows, and the one line texts of
//! footers, notices, lanes and todos. The drawing is in [`super::rich`]
//! and [`super::transcript`]; everything here is tested.

use std::ops::Range;

use horadric_core::folio::{
    line_diff, DiffLine, Ending, FileEdit, Lane, Notice, Todo, TodoStatus, TurnUsage,
};
use horadric_core::markdown::{self as md, Align, Inline};
use serde_json::Value;

/// How far each level of a list or quote moves its text in.
pub const LIST_INDENT: f32 = 22.0;
pub const QUOTE_INDENT: f32 = 16.0;
/// Room between paragraphs, and inside a list.
pub const PARA_GAP: f32 = 10.0;
pub const ITEM_GAP: f32 = 4.0;
/// Above a heading that is not the reply's first.
pub const HEADING_GAP: f32 = 18.0;

/// The size a paragraph's text is set in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Font {
    Body,
    H1,
    H2,
    H3,
}

/// One paragraph of a reply, placed: how far in, and in how many quotes.
#[derive(Debug, Clone, PartialEq)]
pub struct Para {
    pub kind: Kind,
    pub indent: f32,
    /// Quotes it is inside, each a bar at its left.
    pub quotes: u8,
    /// Room above it, none for the first.
    pub gap: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Text {
        font: Font,
        inline: Inline,
        /// A list item's bullet or number, drawn left of the text.
        marker: Option<String>,
    },
    Code {
        lang: String,
        text: String,
    },
    Table {
        header: Vec<Inline>,
        align: Vec<Align>,
        rows: Vec<Vec<Inline>>,
    },
    Rule,
}

/// A reply's markdown, parsed and laid flat.
pub fn paras(src: &str) -> Vec<Para> {
    let mut out = Vec::new();
    flatten(&md::parse(src), 0.0, 0, 0, &mut out);
    if let Some(first) = out.first_mut() {
        first.gap = 0.0;
    }
    out
}

fn flatten(blocks: &[md::Block], indent: f32, quotes: u8, depth: usize, out: &mut Vec<Para>) {
    let inside = depth > 0;
    for b in blocks {
        let gap = if inside { ITEM_GAP } else { PARA_GAP };
        let mut push = |kind: Kind, gap: f32| {
            out.push(Para {
                kind,
                indent,
                quotes,
                gap,
            })
        };
        match b {
            md::Block::Paragraph(inline) => push(
                Kind::Text {
                    font: Font::Body,
                    inline: inline.clone(),
                    marker: None,
                },
                gap,
            ),
            md::Block::Heading { level, text } => {
                let font = match level {
                    1 => Font::H1,
                    2 => Font::H2,
                    _ => Font::H3,
                };
                push(
                    Kind::Text {
                        font,
                        inline: text.clone(),
                        marker: None,
                    },
                    HEADING_GAP,
                )
            }
            md::Block::Code { lang, text, .. } => push(
                Kind::Code {
                    lang: lang.clone(),
                    text: text.clone(),
                },
                gap,
            ),
            md::Block::Table {
                header,
                align,
                rows,
            } => push(
                Kind::Table {
                    header: header.clone(),
                    align: align.clone(),
                    rows: rows.clone(),
                },
                gap,
            ),
            md::Block::Rule => push(Kind::Rule, gap),
            md::Block::Quote(inner) => {
                let at = out.len();
                flatten(inner, indent + QUOTE_INDENT, quotes + 1, depth, out);
                if let Some(p) = out.get_mut(at) {
                    p.gap = gap;
                }
            }
            md::Block::List {
                ordered,
                start,
                items,
            } => {
                for (k, item) in items.iter().enumerate() {
                    let marker = if *ordered {
                        format!("{}.", start + k as u64)
                    } else {
                        ["\u{2022}", "\u{25e6}", "\u{25aa}"][depth.min(2)].to_string()
                    };
                    let at = out.len();
                    flatten(item, indent + LIST_INDENT, quotes, depth + 1, out);
                    if let Some(p) = out.get_mut(at) {
                        p.gap = if k == 0 { gap } else { ITEM_GAP };
                        if let Kind::Text { marker: m, .. } = &mut p.kind {
                            *m = Some(marker);
                        }
                    }
                }
            }
        }
    }
}

/// Column widths for a table `avail` wide, from what each column's widest
/// cell takes unwrapped. Columns that fit keep their width; when they do
/// not, the wide ones give up room, none below `least`.
pub fn table_widths(natural: &[f32], least: f32, avail: f32) -> Vec<f32> {
    let total: f32 = natural.iter().sum();
    if total <= avail || natural.is_empty() {
        return natural.to_vec();
    }
    // The narrow columns keep their width; the rest share what is left
    // in proportion to what they wanted.
    let mut keep = vec![false; natural.len()];
    loop {
        let fixed: f32 = (natural.iter().zip(&keep))
            .filter(|(_, k)| **k)
            .map(|(w, _)| *w)
            .sum();
        let wide: f32 = (natural.iter().zip(&keep))
            .filter(|(_, k)| !**k)
            .map(|(w, _)| *w)
            .sum();
        let room = (avail - fixed).max(0.0);
        let scale = if wide > 0.0 { room / wide } else { 1.0 };
        let mut changed = false;
        for (k, w) in keep.iter_mut().zip(natural) {
            if !*k && w * scale <= least.min(*w) {
                *k = true;
                changed = true;
            }
        }
        if !changed {
            return (natural.iter().zip(&keep))
                .map(|(w, k)| if *k { *w } else { (w * scale).max(least) })
                .collect();
        }
    }
}

/// A place in the transcript's text: which block, which piece of text in
/// it, in the order they are drawn, and a UTF-16 offset in that piece.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Pos {
    pub block: usize,
    pub unit: u32,
    pub at: u32,
}

/// The part of piece (`block`, `unit`), `len` units long, inside the
/// selection from `a` to `b` in either order. None when it is outside or
/// the part is empty.
pub fn unit_range(a: Pos, b: Pos, block: usize, unit: u32, len: u32) -> Option<Range<u32>> {
    let (from, to) = if a <= b { (a, b) } else { (b, a) };
    let me = (block, unit);
    if me < (from.block, from.unit) || me > (to.block, to.unit) {
        return None;
    }
    let start = if me == (from.block, from.unit) {
        from.at.min(len)
    } else {
        0
    };
    let end = if me == (to.block, to.unit) {
        to.at.min(len)
    } else {
        len
    };
    (end > start).then_some(start..end)
}

/// One piece of text in the transcript, for copying and finding.
pub struct Text<'a> {
    pub block: usize,
    pub unit: u32,
    pub text: &'a str,
}

/// The selection from `a` to `b` as text: pieces of one block on lines of
/// their own, blocks apart by a blank line.
pub fn copy_text(texts: &[Text], a: Pos, b: Pos) -> String {
    let mut out = String::new();
    let mut last: Option<usize> = None;
    for t in texts {
        let wide: Vec<u16> = t.text.encode_utf16().collect();
        let Some(r) = unit_range(a, b, t.block, t.unit, wide.len() as u32) else {
            continue;
        };
        if let Some(l) = last {
            out.push_str(if l == t.block { "\n" } else { "\n\n" });
        }
        out.push_str(&String::from_utf16_lossy(
            &wide[r.start as usize..r.end as usize],
        ));
        last = Some(t.block);
    }
    out
}

/// Where `query` is found in the pieces, ignoring case, in order. Ranges
/// are UTF-16 offsets, as DirectWrite counts.
pub fn find_all(texts: &[Text], query: &str) -> Vec<Pos2> {
    let q: Vec<u16> = lower(query).encode_utf16().collect();
    if q.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for t in texts {
        // Lowercasing can change a text's length; one char at a time keeps
        // offsets true for the scripts that matter here.
        let hay: Vec<u16> = t
            .text
            .chars()
            .flat_map(|c| {
                let l = c.to_lowercase().next().unwrap_or(c);
                let l = if l.len_utf16() == c.len_utf16() { l } else { c };
                let mut b = [0u16; 2];
                l.encode_utf16(&mut b).to_vec()
            })
            .collect();
        let mut i = 0;
        while i + q.len() <= hay.len() {
            if hay[i..i + q.len()] == q[..] {
                out.push(Pos2 {
                    block: t.block,
                    unit: t.unit,
                    range: i as u32..(i + q.len()) as u32,
                });
                i += q.len();
            } else {
                i += 1;
            }
        }
    }
    out
}

fn lower(s: &str) -> String {
    s.chars()
        .map(|c| {
            let l = c.to_lowercase().next().unwrap_or(c);
            if l.len_utf16() == c.len_utf16() {
                l
            } else {
                c
            }
        })
        .collect()
}

/// A run of text found: its piece and its range there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pos2 {
    pub block: usize,
    pub unit: u32,
    pub range: Range<u32>,
}

/// The word around UTF-16 offset `at`, for a double click: letters,
/// digits and underscores, or the one other character there.
pub fn word_at(text: &str, at: u32) -> Range<u32> {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let n = wide.len() as u32;
    if n == 0 {
        return 0..0;
    }
    let at = at.min(n - 1);
    let word = |u: u16| char::from_u32(u as u32).is_some_and(|c| c.is_alphanumeric() || c == '_');
    if !word(wide[at as usize]) {
        return at..at + 1;
    }
    let mut s = at;
    while s > 0 && word(wide[s as usize - 1]) {
        s -= 1;
    }
    let mut e = at;
    while e < n && word(wide[e as usize]) {
        e += 1;
    }
    s..e
}

/// Which of `rects` a point is in, or else the nearest above or below it,
/// so a drag through the gaps between pieces still selects.
pub fn nearest(rects: &[(f32, f32, f32, f32)], x: f32, y: f32) -> Option<usize> {
    let inside =
        |&(rx, ry, rw, rh): &(f32, f32, f32, f32)| x >= rx && x < rx + rw && y >= ry && y < ry + rh;
    if let Some(i) = rects.iter().position(inside) {
        return Some(i);
    }
    let dist = |&(rx, ry, rw, rh): &(f32, f32, f32, f32)| {
        let dy = if y < ry {
            ry - y
        } else if y > ry + rh {
            y - (ry + rh)
        } else {
            0.0
        };
        let dx = if x < rx {
            rx - x
        } else if x > rx + rw {
            x - (rx + rw)
        } else {
            0.0
        };
        dy * 4.0 + dx
    };
    (0..rects.len()).min_by(|&a, &b| dist(&rects[a]).total_cmp(&dist(&rects[b])))
}

/// Where to scroll to show the prompt before or after the one at the top
/// of the view, from the prompts' tops. None when there is none that way.
pub fn next_prompt(tops: &[f32], scroll: f32, up: bool) -> Option<f32> {
    const SLACK: f32 = 4.0;
    if up {
        tops.iter().rev().find(|&&t| t < scroll - SLACK).copied()
    } else {
        tops.iter().find(|&&t| t > scroll + SLACK).copied()
    }
}

/// A count of tokens as a person reads it: 950, 12.3k, 1.2M.
pub fn tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=99_999 => format!("{:.1}k", n as f64 / 1000.0).replace(".0k", "k"),
        100_000..=999_999 => format!("{}k", n / 1000),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0).replace(".0M", "M"),
    }
}

/// A turn's footer: how it ended, its time and cost, the tokens it read
/// and wrote, and the context left when known.
pub fn footer(duration_ms: u64, cost_usd: f64, ending: Ending, usage: &TurnUsage) -> String {
    let mut s = crate::folio_look::footer_line(duration_ms, cost_usd, ending);
    let read = usage.input_tokens + usage.cache_read_tokens + usage.cache_creation_tokens;
    if read + usage.output_tokens > 0 {
        s.push_str(&format!(
            " \u{b7} {} in \u{b7} {} out",
            tokens(read),
            tokens(usage.output_tokens)
        ));
    }
    if let (Some(left), Some(limit)) = (usage.context_left(), usage.context_limit) {
        if limit > 0 {
            let pct = (left as f64 / limit as f64 * 100.0).round() as u64;
            s.push_str(&format!(" \u{b7} {pct}% context left"));
        }
    }
    s
}

/// A notice as its one quiet line.
pub fn notice(n: &Notice) -> String {
    match n {
        Notice::Compacted {
            trigger,
            pre_tokens,
            post_tokens,
        } => {
            let how = if trigger == "auto" {
                "Compacted by itself"
            } else {
                "Compacted"
            };
            match post_tokens {
                Some(post) => format!("{how}, {} to {} tokens", tokens(*pre_tokens), tokens(*post)),
                None => format!("{how}, from {} tokens", tokens(*pre_tokens)),
            }
        }
        Notice::Cleared => "Context cleared, a new conversation starts here".to_string(),
        Notice::Retrying {
            attempt,
            max_retries,
            delay_ms,
            status,
            error,
        } => {
            let why = match status {
                Some(s) if error.is_empty() => format!("error {s}"),
                Some(s) => format!("{s} {error}"),
                None if error.is_empty() => "no answer".to_string(),
                None => error.clone(),
            };
            format!(
                "Retrying, {attempt} of {max_retries}, in {:.0} s: {why}",
                (*delay_ms as f64 / 1000.0).ceil()
            )
        }
        Notice::CommandOutput(s) => s.trim().to_string(),
        Notice::Refused(s) => format!("Claude refused: {}", s.trim()),
    }
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

/// The last `n` lines of a shell step's output, and how many were left
/// out above them.
pub fn last_lines(s: &str, n: usize) -> (String, usize) {
    let lines: Vec<&str> = s.trim_end().lines().collect();
    let cut = lines.len().saturating_sub(n);
    (lines[cut..].join("\n"), cut)
}

/// What "always allow" adds, from an ask's `permission_suggestions`, such
/// as "Bash(npm test:*)". None when there is nothing to add.
pub fn always_label(suggestions: &Value) -> Option<String> {
    let list = suggestions.as_array().filter(|a| !a.is_empty())?;
    let mut rules = Vec::new();
    let mut dirs = Vec::new();
    let mut modes = Vec::new();
    for s in list {
        match s["type"].as_str().unwrap_or_default() {
            "addRules" | "replaceRules" => {
                for r in s["rules"].as_array().into_iter().flatten() {
                    let tool = r["toolName"].as_str().unwrap_or_default();
                    rules.push(match r["ruleContent"].as_str() {
                        Some(c) if !c.is_empty() => format!("{tool}({c})"),
                        _ => tool.to_string(),
                    });
                }
            }
            "addDirectories" => {
                dirs.extend(
                    (s["directories"].as_array().into_iter().flatten())
                        .filter_map(|d| d.as_str().map(str::to_string)),
                );
            }
            "setMode" => modes.extend(s["mode"].as_str().map(str::to_string)),
            _ => {}
        }
    }
    let mut parts = rules;
    parts.extend(dirs.into_iter().map(|d| format!("files in {d}")));
    parts.extend(modes.into_iter().map(|m| match m.as_str() {
        "acceptEdits" => "accept every edit this session".to_string(),
        "bypassPermissions" => "allow everything this session".to_string(),
        "plan" => "plan mode".to_string(),
        _ => format!("{m} mode"),
    }));
    Some(if parts.is_empty() {
        "the same again".to_string()
    } else {
        parts.join(", ")
    })
}

/// What a line the model did not know is, from its `type` and
/// `subtype`, such as "system background_tasks_changed".
pub fn raw_label(line: &str) -> String {
    let v: Value = serde_json::from_str(line).unwrap_or(Value::Null);
    let kind = (["type", "subtype"].iter())
        .filter_map(|k| v[*k].as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if kind.is_empty() {
        "A line Horadric does not know".to_string()
    } else {
        format!("Not shown: {kind}")
    }
}

/// A subagent lane's heading: its type and how far it got.
pub fn lane_line(lane: &Lane, running: bool) -> String {
    let who = if lane.agent_type.is_empty() {
        "Agent"
    } else {
        &lane.agent_type
    };
    let mut s = who.to_string();
    let uses = lane.tool_uses.max(lane.steps.len() as u64);
    if uses > 0 {
        s.push_str(&format!(
            " \u{b7} {uses} tool {}",
            if uses == 1 { "use" } else { "uses" }
        ));
    }
    if lane.tokens > 0 {
        s.push_str(&format!(" \u{b7} {} tokens", tokens(lane.tokens)));
    }
    if running && !lane.progress.trim().is_empty() {
        s.push_str(&format!(" \u{b7} {}", lane.progress.trim()));
    }
    s
}

/// The todo list folded to a line: how many are done, and what is being
/// done now.
pub fn todo_line(todos: &[Todo]) -> String {
    let done = (todos.iter())
        .filter(|t| t.status == TodoStatus::Completed)
        .count();
    let mut s = format!("Todos {done} of {}", todos.len());
    if let Some(now) = todos.iter().find(|t| t.status == TodoStatus::InProgress) {
        let what = if now.active_form.is_empty() {
            &now.content
        } else {
            &now.active_form
        };
        s.push_str(&format!(" \u{b7} {what}"));
    }
    s
}

/// The path a link names, resolved against the project, and the part of
/// it inside the project (with forward slashes) when it is in it.
pub fn project_path(dir: &std::path::Path, path: &str) -> (std::path::PathBuf, Option<String>) {
    let p = std::path::Path::new(path);
    let full = if p.is_absolute() {
        p.to_path_buf()
    } else {
        dir.join(path.replace('/', "\\"))
    };
    let rel = full
        .strip_prefix(dir)
        .ok()
        .map(|r| r.to_string_lossy().replace('\\', "/"));
    (full, rel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::folio::{Change, Hunk};

    fn text_of(p: &Para) -> &str {
        match &p.kind {
            Kind::Text { inline, .. } => &inline.text,
            Kind::Code { text, .. } => text,
            _ => "",
        }
    }

    fn marker(p: &Para) -> Option<&str> {
        match &p.kind {
            Kind::Text { marker, .. } => marker.as_deref(),
            _ => None,
        }
    }

    #[test]
    fn a_reply_lays_flat_with_lists_indented_and_marked() {
        let ps = paras("# Title\n\nSome **bold** text.\n\n- one\n  - inner\n- two\n\n1. first\n2. second\n\n> quoted\n\n```rust\nfn main() {}\n```\n\n---");
        let texts: Vec<&str> = ps.iter().map(text_of).collect();
        assert_eq!(
            texts,
            [
                "Title",
                "Some bold text.",
                "one",
                "inner",
                "two",
                "first",
                "second",
                "quoted",
                "fn main() {}",
                ""
            ]
        );
        assert_eq!(ps[0].gap, 0.0, "the first has no room above");
        assert!(matches!(ps[0].kind, Kind::Text { font: Font::H1, .. }));
        assert_eq!(marker(&ps[2]), Some("\u{2022}"));
        assert_eq!(marker(&ps[3]), Some("\u{25e6}"));
        assert_eq!(ps[3].indent, 2.0 * LIST_INDENT);
        assert_eq!(ps[4].indent, LIST_INDENT);
        assert_eq!(marker(&ps[5]), Some("1."));
        assert_eq!(marker(&ps[6]), Some("2."));
        assert_eq!(ps[7].quotes, 1);
        assert_eq!(ps[7].indent, QUOTE_INDENT);
        assert!(matches!(&ps[8].kind, Kind::Code { lang, .. } if lang == "rust"));
        assert!(matches!(ps[9].kind, Kind::Rule));
    }

    #[test]
    fn an_ordered_list_counts_from_its_start() {
        let ps = paras("3. c\n4. d");
        assert_eq!(marker(&ps[0]), Some("3."));
        assert_eq!(marker(&ps[1]), Some("4."));
    }

    #[test]
    fn a_streaming_reply_keeps_its_earlier_paragraphs_equal() {
        let a = paras("First para.\n\nSecond is gro");
        let b = paras("First para.\n\nSecond is growing");
        assert_eq!(a[0], b[0], "an unchanged paragraph compares equal");
        assert_ne!(a[1], b[1]);
    }

    #[test]
    fn table_columns_that_fit_keep_their_width() {
        assert_eq!(table_widths(&[50.0, 80.0], 40.0, 300.0), [50.0, 80.0]);
    }

    #[test]
    fn wide_table_columns_give_up_room_and_narrow_ones_keep_it() {
        let w = table_widths(&[40.0, 400.0, 400.0], 60.0, 440.0);
        assert_eq!(w[0], 40.0);
        assert!(
            (w[1] - 200.0).abs() < 0.01 && (w[2] - 200.0).abs() < 0.01,
            "{w:?}"
        );
        let w = table_widths(&[100.0, 1000.0], 80.0, 200.0);
        assert!(w[0] >= 80.0 && w[1] >= 80.0, "{w:?}");
    }

    fn pos(block: usize, unit: u32, at: u32) -> Pos {
        Pos { block, unit, at }
    }

    #[test]
    fn a_selection_covers_whole_pieces_between_its_ends() {
        let (a, b) = (pos(1, 2, 3), pos(3, 0, 4));
        assert_eq!(unit_range(a, b, 1, 2, 10), Some(3..10));
        assert_eq!(unit_range(a, b, 2, 7, 5), Some(0..5));
        assert_eq!(unit_range(a, b, 3, 0, 10), Some(0..4));
        assert_eq!(unit_range(a, b, 1, 1, 10), None);
        assert_eq!(unit_range(a, b, 3, 1, 10), None);
        // Backwards is the same selection.
        assert_eq!(unit_range(b, a, 1, 2, 10), Some(3..10));
        assert_eq!(unit_range(a, a, 1, 2, 10), None, "empty");
    }

    #[test]
    fn copy_joins_pieces_by_lines_and_blocks_by_blank_lines() {
        let texts = [
            Text {
                block: 0,
                unit: 0,
                text: "hello world",
            },
            Text {
                block: 0,
                unit: 1,
                text: "second",
            },
            Text {
                block: 2,
                unit: 0,
                text: "\u{1F600} end",
            },
        ];
        let s = copy_text(&texts, pos(0, 0, 6), pos(2, 0, 3));
        assert_eq!(s, "world\nsecond\n\n\u{1F600} ");
    }

    #[test]
    fn find_ignores_case_and_counts_in_utf16() {
        let texts = [
            Text {
                block: 0,
                unit: 0,
                text: "\u{1F600} Foo foo",
            },
            Text {
                block: 3,
                unit: 1,
                text: "no",
            },
        ];
        let found = find_all(&texts, "FOO");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].range, 3..6);
        assert_eq!(found[1].range, 7..10);
        assert!(find_all(&texts, "").is_empty());
    }

    #[test]
    fn a_double_click_takes_the_word() {
        assert_eq!(word_at("let snake_case = 1;", 6), 4..14);
        assert_eq!(word_at("a + b", 2), 2..3);
        assert_eq!(word_at("end", 9), 0..3);
        assert_eq!(word_at("", 0), 0..0);
    }

    #[test]
    fn a_point_between_pieces_goes_to_the_nearest() {
        let rects = [(0.0, 0.0, 100.0, 20.0), (0.0, 40.0, 100.0, 20.0)];
        assert_eq!(nearest(&rects, 10.0, 10.0), Some(0));
        assert_eq!(nearest(&rects, 10.0, 35.0), Some(1));
        assert_eq!(nearest(&rects, 10.0, 22.0), Some(0));
        assert_eq!(nearest(&rects, 500.0, 50.0), Some(1));
        assert_eq!(nearest(&[], 0.0, 0.0), None);
    }

    #[test]
    fn prompts_are_jumped_to_in_either_direction() {
        let tops = [0.0, 300.0, 900.0];
        assert_eq!(next_prompt(&tops, 300.0, false), Some(900.0));
        assert_eq!(next_prompt(&tops, 300.0, true), Some(0.0));
        assert_eq!(next_prompt(&tops, 950.0, false), None);
        assert_eq!(next_prompt(&tops, 0.0, true), None);
    }

    #[test]
    fn tokens_read_short() {
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(12_345), "12.3k");
        assert_eq!(tokens(2_000), "2k");
        assert_eq!(tokens(250_000), "250k");
        assert_eq!(tokens(1_000_000), "1M");
    }

    #[test]
    fn a_footer_says_tokens_and_the_context_left() {
        let usage = TurnUsage {
            input_tokens: 10,
            output_tokens: 300,
            cache_read_tokens: 2_000,
            cache_creation_tokens: 0,
            context_used: Some(50_000),
            context_limit: Some(200_000),
        };
        let s = footer(2_500, 0.0123, Ending::Done, &usage);
        assert_eq!(
            s,
            "2.5 s \u{b7} $0.0123 \u{b7} 2k in \u{b7} 300 out \u{b7} 75% context left"
        );
        let plain = footer(1_000, 0.0, Ending::Done, &TurnUsage::default());
        assert_eq!(plain, "1.0 s");
    }

    #[test]
    fn notices_are_one_line() {
        let n = Notice::Compacted {
            trigger: "manual".into(),
            pre_tokens: 120_000,
            post_tokens: Some(30_000),
        };
        assert_eq!(notice(&n), "Compacted, 120k to 30k tokens");
        let r = Notice::Retrying {
            attempt: 2,
            max_retries: 10,
            delay_ms: 1_500,
            status: Some(529),
            error: "overloaded".into(),
        };
        assert_eq!(notice(&r), "Retrying, 2 of 10, in 2 s: 529 overloaded");
    }

    #[test]
    fn hunks_number_their_rows_on_each_side() {
        let edit = FileEdit {
            path: "a.rs".into(),
            hunks: vec![
                Hunk {
                    old_start: 10,
                    old_lines: 2,
                    new_start: 10,
                    new_lines: 2,
                    lines: vec![" keep".into(), "-old".into(), "+new".into()],
                },
                Hunk {
                    old_start: 40,
                    new_start: 40,
                    lines: vec!["+more".into()],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let rows = diff_rows(&edit);
        let marks: String = rows.iter().map(|r| r.mark).collect();
        assert_eq!(marks, " -+~+");
        assert_eq!((rows[0].old, rows[0].new), (Some(10), Some(10)));
        assert_eq!((rows[1].old, rows[1].new), (Some(11), None));
        assert_eq!((rows[2].old, rows[2].new), (None, Some(11)));
        assert_eq!(rows[2].text, "new");
        assert_eq!(rows[4].new, Some(40));
    }

    #[test]
    fn without_hunks_the_swapped_strings_are_diffed() {
        let edit = FileEdit {
            path: "a.rs".into(),
            changes: vec![Change {
                old: "a\nb".into(),
                new: "a\nc".into(),
            }],
            ..Default::default()
        };
        let marks: String = diff_rows(&edit).iter().map(|r| r.mark).collect();
        assert_eq!(marks, " -+");
    }

    #[test]
    fn last_lines_says_how_many_it_left_out() {
        assert_eq!(last_lines("a\nb\nc\n", 2), ("b\nc".to_string(), 1));
        assert_eq!(last_lines("a", 5), ("a".to_string(), 0));
    }

    #[test]
    fn always_names_the_rules_it_adds() {
        let v = serde_json::json!([{"type": "addRules", "rules": [
            {"toolName": "Bash", "ruleContent": "npm test:*"}, {"toolName": "Read"}]},
            {"type": "addDirectories", "directories": ["C:\\x"]}]);
        assert_eq!(
            always_label(&v).as_deref(),
            Some("Bash(npm test:*), Read, files in C:\\x")
        );
        assert_eq!(always_label(&Value::Null), None);
        assert_eq!(always_label(&serde_json::json!([])), None);
    }

    #[test]
    fn a_raw_line_says_what_kind_it_is() {
        let line = r#"{"type":"system","subtype":"background_tasks_changed","tasks":[]}"#;
        assert_eq!(
            raw_label(line),
            "Not shown: system background_tasks_changed"
        );
        assert_eq!(raw_label("not json"), "A line Horadric does not know");
    }

    #[test]
    fn always_for_a_mode_says_what_it_does() {
        let v = serde_json::json!([{"type": "setMode", "mode": "acceptEdits"}]);
        assert_eq!(
            always_label(&v).as_deref(),
            Some("accept every edit this session")
        );
    }

    #[test]
    fn a_lane_line_shows_progress_only_while_running() {
        let lane = Lane {
            agent_type: "Explore".into(),
            progress: "Reading files".into(),
            tool_uses: 3,
            ..Default::default()
        };
        assert_eq!(
            lane_line(&lane, true),
            "Explore \u{b7} 3 tool uses \u{b7} Reading files"
        );
        assert_eq!(lane_line(&lane, false), "Explore \u{b7} 3 tool uses");
    }

    #[test]
    fn the_todo_line_counts_and_names_the_current() {
        let t = |s, status| Todo {
            content: s,
            active_form: String::new(),
            status,
        };
        let todos = [
            t("a".into(), TodoStatus::Completed),
            t("b".into(), TodoStatus::InProgress),
            t("c".into(), TodoStatus::Pending),
        ];
        assert_eq!(todo_line(&todos), "Todos 1 of 3 \u{b7} b");
    }

    #[test]
    fn a_link_path_resolves_against_the_project() {
        let dir = std::path::Path::new("C:\\p");
        let (full, rel) = project_path(dir, "src/a.rs");
        assert_eq!(full, std::path::Path::new("C:\\p\\src\\a.rs"));
        assert_eq!(rel.as_deref(), Some("src/a.rs"));
        let (_, rel) = project_path(dir, "D:\\elsewhere\\b.rs");
        assert_eq!(rel, None);
    }
}
