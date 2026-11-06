//! What the transcript shows, worked out without a window: a reply's
//! markdown as a flat run of paragraphs to lay out, a table's columns,
//! where a selection starts and ends in each piece of text, what a find
//! matches, the diff of an edit as rows, and the one line texts of
//! footers, notices and lanes. The drawing is in [`super::rich`]
//! and [`super::transcript`]; everything here is tested.

use std::ops::Range;

use horadric_core::folio::{Ending, Lane, Notice, TurnUsage};
use horadric_core::markdown::{self as md, Align, Inline, Span, Style};
use serde_json::Value;

/// How far each level of a list or quote moves its text in.
pub const LIST_INDENT: f32 = 26.0;
pub const QUOTE_INDENT: f32 = 18.0;
/// A callout card: its text in from its left edge, the room kept at its
/// right, and its padding above and below.
pub const CARD_INDENT: f32 = 18.0;
pub const CARD_RIGHT: f32 = 14.0;
pub const CARD_PAD: f32 = 11.0;
/// Between a callout's title and its body, which the title's own line
/// already spaces.
pub const TITLE_GAP: f32 = 2.0;
/// Room between paragraphs, and inside a list.
pub const PARA_GAP: f32 = 14.0;
pub const ITEM_GAP: f32 = 6.0;
/// Under a heading: less than between paragraphs, so a heading binds to
/// the section it opens rather than floating between two.
pub const UNDER_HEADING: f32 = 8.0;
/// Before a list that a line ending in a colon leads into.
pub const LEAD_GAP: f32 = 8.0;
/// Between a hanging term's column and the text right of it.
pub const TERM_GAP: f32 = 18.0;
/// How far an open `<details>` section's body sits in under its title.
pub const DETAILS_INDENT: f32 = 22.0;
/// Between two footnotes, which read as one small list.
pub const NOTE_GAP: f32 = 3.0;
/// A term wider than this, as guessed, keeps the list as it was written.
const TERM_MAX: f32 = 220.0;
const BULLETS: [char; 3] = ['\u{2022}', '\u{25e6}', '\u{25aa}'];

/// Above a heading that is not the reply's first: more for a bigger one,
/// as the break it makes is bigger.
pub fn heading_gap(level: u8) -> f32 {
    match level {
        1 => 32.0,
        2 => 28.0,
        3 => 22.0,
        _ => 20.0,
    }
}

/// The size a paragraph's text is set in. H4 stands for levels 4 to 6,
/// set as a small tracked label rather than a smaller H3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Font {
    Body,
    H1,
    H2,
    H3,
    H4,
}

/// What sits in a list item's marker column.
#[derive(Debug, Clone, PartialEq)]
pub enum Marker {
    Bullet(char),
    Number(String),
    /// A task list's box, ticked or not.
    Task(bool),
}

/// A quote or a callout a paragraph is inside, drawn at its left.
#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    /// Where its left edge is.
    pub x: f32,
    /// The room kept at the right by the cards around it.
    pub right: f32,
    /// None for a plain quote.
    pub callout: Option<md::Callout>,
    /// This is the last paragraph inside it.
    pub end: bool,
}

/// One paragraph of a reply, placed: how far in, and in what.
#[derive(Debug, Clone, PartialEq)]
pub struct Para {
    pub kind: Kind,
    pub indent: f32,
    /// The quotes and callouts it is inside, outermost first.
    pub bars: Vec<Bar>,
    /// Room above it, none for the first.
    pub gap: f32,
    /// A list item's bullet, number or box, drawn left of its first line.
    pub marker: Option<Marker>,
    /// How wide the column left of it is that its list marker sits in.
    pub marker_w: f32,
    /// How wide a hanging term's column is, between the marker's and the
    /// text's.
    pub term_w: f32,
    /// Room kept at the right, inside the cards around it.
    pub right: f32,
    /// A heading whose section a click folds: its fold key, and whether
    /// the section is folded now.
    pub fold: Option<(String, bool)>,
    /// The footnotes its marks point at, by label, as plain text: a mark
    /// shows its note on hover without the reader going to the end.
    pub notes: Vec<(String, String)>,
}

impl Para {
    /// Inside a plain quote, as its nearest bar.
    pub fn quoted(&self) -> bool {
        self.bars.last().is_some_and(|b| b.callout.is_none())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Text {
        font: Font,
        inline: Inline,
        /// A hanging term, drawn in its own column left of the text.
        term: Option<Inline>,
    },
    /// A callout's title line: its icon and its name, or its own title.
    Callout {
        kind: md::Callout,
        title: Inline,
    },
    Code {
        /// The fence's whole info string: the language, and a file name
        /// when it names one.
        info: String,
        text: String,
    },
    Table {
        header: Vec<Inline>,
        align: Vec<Align>,
        rows: Vec<Vec<Inline>>,
    },
    Rule,
    /// A `<details>` section's title row; its body follows only when it
    /// is open. `key` is what a click toggles in the open set.
    Details {
        summary: Inline,
        key: String,
        open: bool,
    },
    /// One of the reply's footnotes at its end; the `first` has the rule
    /// above that sets them apart.
    Footnote {
        label: String,
        text: Inline,
        first: bool,
    },
    /// An image alone in its paragraph: a thumbnail when it is a file
    /// here, else a chip naming it.
    Image {
        alt: Inline,
        src: String,
    },
}

/// A reply's markdown, parsed and laid flat, its sections open or folded
/// as the transcript has them.
pub fn paras(src: &str) -> Vec<Para> {
    paras_with(src, super::code::is_open, false)
}

/// [`paras`] with its headings folding their sections too. Only for what
/// is built again when a fold key is toggled (a reply, a thought): on a
/// card that is not, a click on a heading would seem to do nothing.
pub fn paras_folding(src: &str) -> Vec<Para> {
    paras_with(src, super::code::is_open, true)
}

/// [`paras`] with `open` saying which fold keys are toggled, and whether
/// `headings` fold.
fn paras_with(src: &str, open: fn(&str) -> bool, headings: bool) -> Vec<Para> {
    let mut out = Vec::new();
    let at = At {
        indent: 0.0,
        bars: Vec::new(),
        right: 0.0,
        depth: 0,
        open,
        headings,
    };
    flatten(&md::parse(src), &at, &mut out);
    if let Some(first) = out.first_mut() {
        first.gap = 0.0;
    }
    attach_notes(&mut out);
    out
}

/// The fold key of a heading's section. It shares the code blocks'
/// prefix, as [`details_key`] does, so a toggle builds the reply again.
pub fn heading_key(level: u8, text: &str) -> String {
    super::code::fold_key(&format!("heading\n{level}\n{text}"))
}

/// Whether a heading at `level` has anything under it before the next
/// heading as big or bigger: an empty section has nothing to fold.
fn has_section(rest: &[md::Block], level: u8) -> bool {
    match rest.first() {
        None | Some(md::Block::Footnotes(_)) => false,
        Some(md::Block::Heading { level: l, .. }) => *l > level,
        Some(_) => true,
    }
}

/// Gives each paragraph the text of the footnotes its marks name. The
/// notes come last, so this runs once all of them are laid.
fn attach_notes(out: &mut [Para]) {
    let notes: Vec<(String, String)> = (out.iter())
        .filter_map(|p| match &p.kind {
            Kind::Footnote { label, text, .. } => Some((label.clone(), text.text.clone())),
            _ => None,
        })
        .collect();
    if notes.is_empty() {
        return;
    }
    for p in out.iter_mut() {
        let Kind::Text { inline, term, .. } = &p.kind else {
            continue;
        };
        let marks = (inline.spans.iter())
            .chain(term.iter().flat_map(|t| t.spans.iter()))
            .filter_map(|s| match &s.style {
                Style::Footnote(label) => Some(label),
                _ => None,
            });
        let mut mine: Vec<(String, String)> = Vec::new();
        for label in marks {
            if mine.iter().any(|(l, _)| l == label) {
                continue;
            }
            if let Some(n) = notes.iter().find(|(l, _)| l == label) {
                mine.push(n.clone());
            }
        }
        p.notes = mine;
    }
}

/// Where the blocks being laid flat sit.
#[derive(Clone)]
struct At {
    indent: f32,
    bars: Vec<Bar>,
    right: f32,
    /// How many lists deep.
    depth: usize,
    /// Whether a fold key is toggled from how its section starts.
    open: fn(&str) -> bool,
    /// The reply's own headings fold their sections.
    headings: bool,
}

impl At {
    fn para(&self, kind: Kind, gap: f32) -> Para {
        Para {
            kind,
            indent: self.indent,
            bars: self.bars.clone(),
            gap,
            marker: None,
            marker_w: 0.0,
            term_w: 0.0,
            right: self.right,
            fold: None,
            notes: Vec::new(),
        }
    }

    /// Inside one more quote or callout, its text `indent` further in.
    fn within(&self, callout: Option<md::Callout>, indent: f32, right: f32) -> At {
        let mut a = self.clone();
        a.bars.push(Bar {
            x: self.indent,
            right: self.right,
            callout,
            end: false,
        });
        a.indent += indent;
        a.right += right;
        a
    }
}

/// The fold key of a `<details>` section, from its summary: two with the
/// same title open together, as code blocks with the same text do. It
/// shares the code blocks' prefix, so the transcript keeps it with them
/// and builds a reply again when it is toggled.
pub fn details_key(summary: &str) -> String {
    super::code::fold_key(&format!("details\n{summary}"))
}

/// The source of a paragraph that is one image and nothing else.
fn lone_image(p: &Inline) -> Option<String> {
    match p.spans.as_slice() {
        [Span {
            range,
            style: Style::Image(src),
        }] if range.start == 0 && range.end == p.text.len() => Some(src.clone()),
        _ => None,
    }
}

fn text(font: Font, inline: &Inline) -> Kind {
    Kind::Text {
        font,
        inline: inline.clone(),
        term: None,
    }
}

/// Marks the last paragraph laid as the end of the bar at `level`.
fn end_bar(out: &mut [Para], from: usize, level: usize) {
    let n = out.len();
    if let Some(p) = out[from.min(n)..].last_mut() {
        if let Some(b) = p.bars.get_mut(level) {
            b.end = true;
        }
    }
}

fn flatten(blocks: &[md::Block], at: &At, out: &mut Vec<Para>) {
    let normal = if at.depth > 0 { ITEM_GAP } else { PARA_GAP };
    let mut next_gap: Option<f32> = None;
    let mut prev: Option<&md::Block> = None;
    // The level of a folded heading while its section is passed over.
    let mut folded: Option<u8> = None;
    for (i, b) in blocks.iter().enumerate() {
        if let Some(level) = folded {
            match b {
                md::Block::Heading { level: l, .. } if *l <= level => folded = None,
                md::Block::Footnotes(_) => {}
                _ => continue,
            }
        }
        let after_heading = next_gap.is_some();
        let gap = next_gap.take().unwrap_or(normal);
        match b {
            md::Block::Paragraph(inline) => {
                let kind = match lone_image(inline) {
                    Some(src) => Kind::Image {
                        alt: inline.clone(),
                        src,
                    },
                    None => text(Font::Body, inline),
                };
                out.push(at.para(kind, gap));
            }
            md::Block::Details {
                summary,
                body,
                open,
            } => {
                let key = details_key(&summary.text);
                let open = *open != (at.open)(&key);
                out.push(at.para(
                    Kind::Details {
                        summary: summary.clone(),
                        key,
                        open,
                    },
                    gap,
                ));
                if open {
                    let mut inner = at.clone();
                    inner.indent += DETAILS_INDENT;
                    let first = out.len();
                    flatten(body, &inner, out);
                    if let Some(p) = out.get_mut(first) {
                        p.gap = ITEM_GAP;
                    }
                }
            }
            md::Block::Footnotes(notes) => {
                for (k, (label, t)) in notes.iter().enumerate() {
                    let kind = Kind::Footnote {
                        label: label.clone(),
                        text: t.clone(),
                        first: k == 0,
                    };
                    out.push(at.para(kind, if k == 0 { PARA_GAP } else { NOTE_GAP }));
                }
            }
            md::Block::Heading { level, text: t } => {
                let font = match level {
                    1 => Font::H1,
                    2 => Font::H2,
                    3 => Font::H3,
                    _ => Font::H4,
                };
                // Two headings in a row read as one title and its subtitle.
                let gap = if after_heading { gap } else { heading_gap(*level) };
                let mut p = at.para(text(font, t), gap);
                // Only the reply's own headings fold, not one in a list,
                // a quote or a details section.
                let top = at.headings && at.depth == 0 && at.bars.is_empty() && at.indent == 0.0;
                if top && has_section(&blocks[i + 1..], *level) {
                    let key = heading_key(*level, &t.text);
                    let shut = (at.open)(&key);
                    if shut {
                        folded = Some(*level);
                    }
                    p.fold = Some((key, shut));
                }
                let shut = folded.is_some();
                out.push(p);
                // A folded heading is a line of its own, not a title over
                // the next one.
                next_gap = (!shut).then_some(UNDER_HEADING);
            }
            md::Block::Code { info, text, .. } => out.push(at.para(
                Kind::Code {
                    info: info.clone(),
                    text: text.clone(),
                },
                gap,
            )),
            md::Block::Table {
                header,
                align,
                rows,
            } => out.push(at.para(
                Kind::Table {
                    header: header.clone(),
                    align: align.clone(),
                    rows: rows.clone(),
                },
                gap,
            )),
            md::Block::Rule => out.push(at.para(Kind::Rule, gap)),
            md::Block::Quote(inner) => {
                let first = out.len();
                flatten(inner, &at.within(None, QUOTE_INDENT, 0.0), out);
                if let Some(p) = out.get_mut(first) {
                    p.gap = gap;
                }
                end_bar(out, first, at.bars.len());
            }
            md::Block::Callout { kind, title, body } => {
                let inner = at.within(Some(*kind), CARD_INDENT, CARD_RIGHT);
                let first = out.len();
                out.push(inner.para(
                    Kind::Callout {
                        kind: *kind,
                        title: title.clone(),
                    },
                    gap,
                ));
                flatten(body, &inner, out);
                if let Some(p) = out.get_mut(first + 1) {
                    p.gap = TITLE_GAP;
                }
                end_bar(out, first, at.bars.len());
            }
            md::Block::List {
                ordered,
                start,
                items,
                tasks,
            } => {
                let leads = matches!(prev, Some(md::Block::Paragraph(p)) if p.text.trim_end().ends_with(':'));
                let gap = if leads && !after_heading {
                    LEAD_GAP
                } else {
                    gap
                };
                list(*ordered, *start, items, tasks, at, gap, out);
            }
        }
        prev = Some(b);
    }
}

fn list(
    ordered: bool,
    start: u64,
    items: &[Vec<md::Block>],
    tasks: &[Option<bool>],
    at: &At,
    gap: f32,
    out: &mut Vec<Para>,
) {
    let hang = hanging(items);
    let boxes = tasks.iter().any(Option::is_some);
    let step = if ordered {
        // As wide as its widest number, so "25." is not cut short.
        let last = start + items.len().saturating_sub(1) as u64;
        list_indent(&last.to_string())
    } else if hang.is_some() && !boxes {
        // Terms in a column of their own need no bullets to set them apart.
        0.0
    } else {
        LIST_INDENT
    };
    let term_w = hang.as_ref().map_or(0.0, |h| h.w);
    for (k, item) in items.iter().enumerate() {
        let marker = match tasks.get(k).copied().flatten() {
            Some(done) => Some(Marker::Task(done)),
            None if ordered => Some(Marker::Number(format!("{}.", start + k as u64))),
            None if hang.is_some() => None,
            None => Some(Marker::Bullet(BULLETS[at.depth.min(2)])),
        };
        let mut inner = at.clone();
        inner.indent += step + term_w;
        inner.depth += 1;
        let first = out.len();
        match hang.as_ref().and_then(|h| h.parts[k].as_ref()) {
            Some((term, rest)) => {
                out.push(inner.para(
                    Kind::Text {
                        font: Font::Body,
                        inline: rest.clone(),
                        term: Some(term.clone()),
                    },
                    0.0,
                ));
                flatten(&item[1..], &inner, out);
            }
            None => flatten(item, &inner, out),
        }
        if let Some(p) = out.get_mut(first) {
            p.gap = if k == 0 { gap } else { ITEM_GAP };
            p.marker = marker;
            p.marker_w = step;
            p.term_w = term_w;
        }
    }
}

/// A list laid out as terms and what they mean: the term column's width
/// and each item's term and rest.
struct Hang {
    w: f32,
    /// None for a last item still arriving.
    parts: Vec<Option<(Inline, Inline)>>,
}

/// Whether every item of a list reads `**Term**: what it is` (or with a
/// dash), so the terms can hang in a column of their own. One item that
/// does not keeps the list as written. The last may be still streaming in,
/// so a term half sent does not flip the whole list back and forth.
fn hanging(items: &[Vec<md::Block>]) -> Option<Hang> {
    if items.len() < 2 {
        return None;
    }
    let mut parts = Vec::new();
    let mut widest = 0.0f32;
    for (k, item) in items.iter().enumerate() {
        let Some(md::Block::Paragraph(p)) = item.first() else {
            return None;
        };
        match term_of(p) {
            Some((term, rest)) => {
                widest = widest.max(term_width(&term.text));
                parts.push(Some((term, rest)));
            }
            None if k + 1 == items.len() && arriving(p) => parts.push(None),
            None => return None,
        }
    }
    (widest <= TERM_MAX).then(|| Hang {
        w: (widest + TERM_GAP).ceil(),
        parts,
    })
}

/// The bold run at a paragraph's start, if there is one: where it ends.
fn lead_bold(p: &Inline) -> Option<usize> {
    (p.spans.iter())
        .filter(|s| s.style == Style::Bold && s.range.start == 0)
        .map(|s| s.range.end)
        .max()
}

/// `**Term**: rest`, `**Term:** rest` or `**Term** - rest` as the term
/// and the rest.
fn term_of(p: &Inline) -> Option<(Inline, Inline)> {
    let bold = lead_bold(p)?;
    let head = &p.text[..bold];
    let after = &p.text[bold..];
    let (term_end, rest_at, colon) = if let Some(t) = head.strip_suffix(':') {
        (t.len(), bold, true)
    } else if after.starts_with(':') {
        (bold, bold + 1, true)
    } else {
        let dash = [" - ", " \u{2013} ", " \u{2014} "]
            .iter()
            .find(|d| after.starts_with(**d))?;
        (bold, bold + dash.len(), false)
    };
    let rest = &p.text[rest_at..];
    // A colon wants a space after it, so "**a**:b" stays as written.
    let spaced = !colon || rest.is_empty() || rest.starts_with(char::is_whitespace);
    if term_end == 0 || head.contains('\n') || !spaced {
        return None;
    }
    let skip = rest.len() - rest.trim_start().len();
    let mut term = slice(p, 0..term_end);
    term.spans.retain(|s| s.style != Style::Bold);
    Some((term, slice(p, rest_at + skip..p.text.len())))
}

/// The start of a term still being sent: an unclosed `**`, or a closed
/// one with no more than its separator after it yet.
fn arriving(p: &Inline) -> bool {
    match lead_bold(p) {
        Some(end) => matches!(
            p.text[end..].trim(),
            "" | "-" | "\u{2013}" | "\u{2014}"
        ),
        None => p.text.starts_with("**") && !p.text.contains('\n'),
    }
}

/// About how wide a term is set in the body's semibold, without a window
/// to measure it in: capitals and figures are wider than small letters,
/// and a CJK character or an emoji takes about two.
pub fn term_width(s: &str) -> f32 {
    s.chars()
        .map(|c| match c {
            ' ' => 4.5,
            'i' | 'l' | 'j' | '.' | ',' | ':' | '\'' | '|' => 4.5,
            'm' | 'w' | 'M' | 'W' => 13.5,
            c if is_wide(c) => 17.2,
            c if c.is_uppercase() || c.is_ascii_digit() => 10.5,
            _ => 8.6,
        })
        .sum()
}

/// Whether `c` is set full width: East Asian wide and full width
/// characters, and the emoji blocks.
fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD)
}

/// How tall a list item with a hanging term is: its text, from where it
/// starts, or the term beside it when that wraps to more lines.
pub fn hang_height(text_top: f32, text_h: f32, term_h: f32) -> f32 {
    (text_top + text_h).max(term_h)
}

/// The part of `inl` in byte range `r`, its spans cut to it.
fn slice(inl: &Inline, r: Range<usize>) -> Inline {
    Inline {
        text: inl.text[r.clone()].to_string(),
        spans: (inl.spans.iter())
            .filter_map(|s| {
                let a = s.range.start.max(r.start);
                let b = s.range.end.min(r.end);
                (a < b).then(|| Span {
                    range: a - r.start..b - r.start,
                    style: s.style.clone(),
                })
            })
            .collect(),
    }
}

/// How far an ordered list's items sit in, room for its widest number in
/// the body's figures: one digit takes the usual indent, each more about
/// a figure's width.
pub fn list_indent(widest: &str) -> f32 {
    let digits = widest.chars().count().max(1) as f32;
    LIST_INDENT + (digits - 1.0) * 9.5
}

/// Column widths for a table `avail` wide, from what each column's widest
/// cell takes unwrapped. Columns that fit keep their width; when they do
/// not, the wide ones give up room, none below `least` unless even that
/// is too wide, when all shrink alike. The sum never passes `avail`.
pub fn table_widths(natural: &[f32], least: f32, avail: f32) -> Vec<f32> {
    // No room at all would divide by nothing below.
    if avail <= 0.0 {
        return vec![0.0; natural.len()];
    }
    let total: f32 = natural.iter().sum();
    if total <= avail || natural.is_empty() {
        return natural.to_vec();
    }
    // A column held back is held at what it wants or `least`, whichever
    // is less; the rest share what is left in proportion to what they
    // wanted.
    let floor = |w: f32| w.min(least);
    let mut keep = vec![false; natural.len()];
    loop {
        let fixed: f32 = (natural.iter().zip(&keep))
            .filter(|(_, k)| **k)
            .map(|(w, _)| floor(*w))
            .sum();
        if fixed >= avail {
            let k = avail / fixed;
            return natural.iter().map(|w| floor(*w) * k).collect();
        }
        let wide: f32 = (natural.iter().zip(&keep))
            .filter(|(_, k)| !**k)
            .map(|(w, _)| *w)
            .sum();
        let scale = if wide > 0.0 { (avail - fixed) / wide } else { 1.0 };
        let mut changed = false;
        for (k, w) in keep.iter_mut().zip(natural) {
            if !*k && w * scale <= floor(*w) {
                *k = true;
                changed = true;
            }
        }
        if !changed {
            return (natural.iter().zip(&keep))
                .map(|(w, k)| if *k { floor(*w) } else { w * scale })
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

/// A turn's footer: how it ended, its time, the tokens it read and wrote,
/// and the context left when known.
pub fn footer(duration_ms: u64, ending: Ending, usage: &TurnUsage) -> String {
    let mut s = crate::folio_look::footer_line(duration_ms, ending);
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

/// The clock time of `ms` (Unix milliseconds) at `offset` seconds from
/// UTC, as hours, minutes and seconds.
pub fn clock(ms: u64, offset: i64) -> String {
    let secs = (ms / 1000) as i64 + offset;
    let day = secs.rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", day / 3600, day % 3600 / 60, day % 60)
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
        Notice::Refused(s) => format!("Refused: {}", s.trim()),
        Notice::Info(s) => s.trim().to_string(),
        Notice::ApiError { kind, .. } => horadric_core::folio::api_error_title(kind).to_string(),
    }
}

pub use crate::folio_review::{diff_rows, DiffRow};

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

/// The file a link names, relative to the project (with forward slashes),
/// or None when it is not a file inside it. The name comes from the model,
/// so it is taken apart rather than trusted: `..` may not climb out, a
/// network path (`\\host` or `//host`) or another drive is refused, and
/// so is a `:` past the drive (a stream, or a drive's own working folder).
pub fn project_path(dir: &std::path::Path, path: &str) -> Option<String> {
    let p = path.trim().replace('/', "\\");
    if p.is_empty() || p.starts_with("\\\\") {
        return None;
    }
    let root = dir.to_string_lossy().replace('/', "\\");
    let root = root.trim_end_matches('\\');
    let b = p.as_bytes();
    let rest = if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        // Absolute: it must start with the project's folder, in any case.
        let n = root.len();
        let inside = b.len() > n && b[..n].eq_ignore_ascii_case(root.as_bytes()) && b[n] == b'\\';
        if !inside {
            return None;
        }
        &p[n..]
    } else if p.starts_with('\\') {
        return None;
    } else {
        &p[..]
    };
    if rest.contains(':') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('\\') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            name => parts.push(name),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Where a transcript starts showing so it shows about `want` blocks of
/// the first `end`: at the start of a turn, the latest that leaves at
/// least that many, unless that turn alone holds more than twice as many,
/// when it starts within it. `prompts` are the turns' first blocks, in
/// order. A long session draws its end, not all of it.
pub fn shown_from(prompts: &[usize], end: usize, want: usize) -> usize {
    if end <= want {
        return 0;
    }
    let at_most = end - want;
    match prompts.iter().rev().find(|&&p| p <= at_most) {
        Some(&p) if end - p <= 2 * want => p,
        _ => at_most,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::folio::{Change, FileEdit, Hunk};

    fn text_of(p: &Para) -> &str {
        match &p.kind {
            Kind::Text { inline, .. } => &inline.text,
            Kind::Code { text, .. } => text,
            _ => "",
        }
    }

    fn marker(p: &Para) -> Option<String> {
        p.marker.as_ref().map(|m| match m {
            Marker::Bullet(c) => c.to_string(),
            Marker::Number(n) => n.clone(),
            Marker::Task(done) => format!("[{}]", if *done { "x" } else { " " }),
        })
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
        assert_eq!(marker(&ps[2]).as_deref(), Some("\u{2022}"));
        assert_eq!(marker(&ps[3]).as_deref(), Some("\u{25e6}"));
        assert_eq!(ps[3].indent, 2.0 * LIST_INDENT);
        assert_eq!(ps[4].indent, LIST_INDENT);
        assert_eq!(marker(&ps[5]).as_deref(), Some("1."));
        assert_eq!(marker(&ps[6]).as_deref(), Some("2."));
        assert_eq!(ps[7].bars.len(), 1);
        assert!(ps[7].quoted());
        assert_eq!(ps[7].indent, QUOTE_INDENT);
        assert!(matches!(&ps[8].kind, Kind::Code { info, .. } if info == "rust"));
        assert!(matches!(ps[9].kind, Kind::Rule));
    }

    #[test]
    fn a_list_to_25_has_room_for_its_numbers() {
        let src: String = (1..=25)
            .map(|i| {
                format!(
                    "{i}. item {i}
"
                )
            })
            .collect();
        let ps = paras(&src);
        assert_eq!(ps.len(), 25);
        assert_eq!(marker(&ps[24]).as_deref(), Some("25."));
        assert!(ps.iter().all(|p| p.indent == ps[0].indent), "one column");
        assert!(ps[0].marker_w > LIST_INDENT);
        assert_eq!(ps[0].marker_w, ps[0].indent);
        let short = paras(
            "1. a
2. b",
        );
        assert_eq!(short[0].marker_w, LIST_INDENT);
        assert_eq!(list_indent("100"), LIST_INDENT + 19.0);
    }

    #[test]
    fn an_ordered_list_counts_from_its_start() {
        let ps = paras("3. c\n4. d");
        assert_eq!(marker(&ps[0]).as_deref(), Some("3."));
        assert_eq!(marker(&ps[1]).as_deref(), Some("4."));
    }

    #[test]
    fn task_items_get_boxes_in_the_marker_column() {
        let ps = paras("- [ ] todo\n- [x] done\n- plain");
        assert_eq!(marker(&ps[0]).as_deref(), Some("[ ]"));
        assert_eq!(marker(&ps[1]).as_deref(), Some("[x]"));
        assert_eq!(marker(&ps[2]).as_deref(), Some("\u{2022}"));
        assert_eq!(text_of(&ps[1]), "done");
        assert_eq!(ps[0].marker_w, LIST_INDENT);
    }

    #[test]
    fn an_item_that_opens_with_code_keeps_its_bullet() {
        let ps = paras("- ```sh\n  ls\n  ```\n- | a |\n  |---|\n  | 1 |");
        assert!(matches!(ps[0].kind, Kind::Code { .. }));
        assert_eq!(marker(&ps[0]).as_deref(), Some("\u{2022}"));
        assert!(matches!(ps[1].kind, Kind::Table { .. }));
        assert_eq!(marker(&ps[1]).as_deref(), Some("\u{2022}"));
    }

    #[test]
    fn a_callout_is_a_title_then_its_body_inside_one_bar() {
        let ps = paras("Before.\n\n> [!WARNING]\n> Careful.\n>\n> - one\n\nAfter.");
        assert!(matches!(
            ps[1].kind,
            Kind::Callout {
                kind: md::Callout::Warning,
                ..
            }
        ));
        assert_eq!(ps[1].gap, PARA_GAP);
        assert_eq!(ps[1].indent, CARD_INDENT);
        assert_eq!(ps[1].right, CARD_RIGHT);
        assert_eq!(text_of(&ps[2]), "Careful.");
        assert_eq!(ps[2].gap, TITLE_GAP);
        assert_eq!(text_of(&ps[3]), "one");
        assert_eq!(ps[3].indent, CARD_INDENT + LIST_INDENT);
        for p in &ps[1..4] {
            assert_eq!(p.bars.len(), 1);
            assert_eq!(p.bars[0].callout, Some(md::Callout::Warning));
            assert_eq!(p.bars[0].x, 0.0);
            assert!(!p.quoted());
        }
        assert!(!ps[1].bars[0].end && !ps[2].bars[0].end);
        assert!(ps[3].bars[0].end, "the last inside ends the card");
        assert!(ps[4].bars.is_empty());
    }

    #[test]
    fn a_quote_in_a_list_has_its_bar_at_the_item() {
        let ps = paras("- item\n\n  > said");
        assert_eq!(ps[1].bars[0].x, LIST_INDENT);
        assert_eq!(ps[1].indent, LIST_INDENT + QUOTE_INDENT);
        assert!(ps[1].quoted() && ps[1].bars[0].end);
    }

    #[test]
    fn headings_take_room_above_and_bind_below() {
        let ps = paras("Intro.\n\n## Section\n\nBody.\n\n#### Small\n\nMore.\n\n# A\n## B");
        assert_eq!(ps[1].gap, heading_gap(2));
        assert_eq!(ps[2].gap, UNDER_HEADING);
        assert!(matches!(ps[3].kind, Kind::Text { font: Font::H4, .. }));
        assert_eq!(ps[4].gap, UNDER_HEADING);
        assert_eq!(ps[6].gap, UNDER_HEADING, "a subtitle stays with its title");
        assert!(heading_gap(1) > heading_gap(3));
        for (src, font) in [("##### x", Font::H4), ("###### x", Font::H4), ("### x", Font::H3)] {
            assert!(matches!(paras(src)[0].kind, Kind::Text { font: f, .. } if f == font));
        }
    }

    #[test]
    fn a_list_led_in_by_a_colon_sits_close() {
        let ps = paras("Steps:\n\n- one");
        assert_eq!(ps[1].gap, LEAD_GAP);
        let ps = paras("Done.\n\n- one");
        assert_eq!(ps[1].gap, PARA_GAP);
    }

    fn term(p: &Para) -> Option<&str> {
        match &p.kind {
            Kind::Text { term, .. } => term.as_ref().map(|t| t.text.as_str()),
            _ => None,
        }
    }

    #[test]
    fn terms_hang_when_every_item_has_one() {
        let ps = paras("- **Fast**: it is quick\n- **Small:** it is light\n- **Safe** - no `unsafe`");
        assert_eq!(term(&ps[0]), Some("Fast"));
        assert_eq!(text_of(&ps[0]), "it is quick");
        assert_eq!(term(&ps[1]), Some("Small"));
        assert_eq!(text_of(&ps[1]), "it is light");
        assert_eq!(term(&ps[2]), Some("Safe"));
        assert_eq!(text_of(&ps[2]), "no unsafe");
        match &ps[2].kind {
            Kind::Text { inline, term, .. } => {
                assert_eq!(inline.spans.len(), 1, "the code stays code");
                assert!(term.as_ref().unwrap().spans.is_empty(), "no bold left");
            }
            _ => unreachable!(),
        }
        let w = ps[0].term_w;
        assert!(w >= term_width("Small") + TERM_GAP);
        assert!(ps.iter().all(|p| p.term_w == w && p.marker.is_none()));
        assert!(ps.iter().all(|p| p.indent == w), "no bullet column");
    }

    #[test]
    fn numbered_terms_keep_their_numbers() {
        let ps = paras("1. **Build**: compile it\n2. **Ship**: send it");
        assert_eq!(marker(&ps[1]).as_deref(), Some("2."));
        assert_eq!(term(&ps[1]), Some("Ship"));
        assert_eq!(ps[1].indent, ps[1].marker_w + ps[1].term_w);
    }

    #[test]
    fn one_item_without_a_term_keeps_the_list_as_written() {
        for src in [
            "- **Fast**: quick\n- plain item\n- **Safe**: sound",
            "- **Fast**: quick",
            "- **Fast**:quick\n- **Safe**: sound",
            "- **A very long term that would take half the line**: x\n- **B**: y",
            "- **Fast** quick\n- **Safe** sound",
        ] {
            let ps = paras(src);
            assert!(ps.iter().all(|p| term(p).is_none()), "{src}");
            assert_eq!(marker(&ps[0]).as_deref(), Some("\u{2022}"), "{src}");
        }
    }

    #[test]
    fn a_term_still_arriving_keeps_the_list_hanging() {
        for last in ["- **Th", "- **Third**", "- **Third** -"] {
            let ps = paras(&format!("- **One**: a\n- **Two**: b\n{last}"));
            assert_eq!(term(&ps[0]), Some("One"), "{last}");
            assert_eq!(term(&ps[2]), None);
            assert_eq!(ps[2].term_w, ps[0].term_w);
        }
    }

    #[test]
    fn a_streaming_reply_keeps_its_earlier_paragraphs_equal() {
        let a = paras("First para.\n\nSecond is gro");
        let b = paras("First para.\n\nSecond is growing");
        assert_eq!(a[0], b[0], "an unchanged paragraph compares equal");
        assert_ne!(a[1], b[1]);
    }

    fn none_open(_: &str) -> bool {
        false
    }

    fn all_open(k: &str) -> bool {
        k.starts_with("code:")
    }

    const DETAILS: &str = "Before.\n\n<details>\n<summary>More</summary>\n\nHidden.\n\n</details>\n\nAfter.";

    #[test]
    fn details_fold_until_their_key_is_toggled() {
        let ps = paras_with(DETAILS, none_open, false);
        assert_eq!(ps.len(), 3);
        match &ps[1].kind {
            Kind::Details { summary, key, open } => {
                assert_eq!(summary.text, "More");
                assert_eq!(key, &details_key("More"));
                assert!(key.starts_with("code:"), "kept with the code blocks' keys");
                assert!(!open);
            }
            other => panic!("{other:?}"),
        }
        let ps = paras_with(DETAILS, all_open, false);
        assert_eq!(ps.len(), 4);
        assert!(matches!(ps[1].kind, Kind::Details { open: true, .. }));
        assert_eq!(text_of(&ps[2]), "Hidden.");
        assert_eq!(ps[2].indent, DETAILS_INDENT);
        assert_eq!(ps[2].gap, ITEM_GAP);
        assert_eq!(text_of(&ps[3]), "After.");
    }

    #[test]
    fn details_written_open_close_when_toggled() {
        let src = "<details open><summary>S</summary>body</details>";
        assert_eq!(paras_with(src, none_open, false).len(), 2);
        assert_eq!(paras_with(src, all_open, false).len(), 1);
    }

    #[test]
    fn footnotes_come_last_under_one_rule() {
        let ps = paras("Claim[^1] and[^2].\n\n[^1]: One.\n[^2]: Two.\n\nMore.");
        assert_eq!(ps.len(), 4);
        assert_eq!(text_of(&ps[1]), "More.");
        match (&ps[2].kind, &ps[3].kind) {
            (
                Kind::Footnote { label: a, text: ta, first: true },
                Kind::Footnote { label: b, first: false, .. },
            ) => {
                assert_eq!((a.as_str(), b.as_str()), ("1", "2"));
                assert_eq!(ta.text, "One.");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(ps[3].gap, NOTE_GAP);
    }

    #[test]
    fn a_paragraph_carries_the_notes_its_marks_name() {
        let ps = paras("Claim[^a] and[^b], again[^a].\n\nPlain.\n\n[^a]: One.\n[^b]: Two.");
        let want = vec![("1".to_string(), "One.".to_string()), ("2".into(), "Two.".into())];
        assert_eq!(ps[0].notes, want);
        assert!(ps[1].notes.is_empty());
    }

    const SECTIONS: &str = "# Top\n\nIntro.\n\n## A\n\nIn A.\n\n### A1\n\nDeep.\n\n## B\n\nIn B.\n\n## Empty\n\n## C\n\n- item\n\n[^1]: Note.";

    #[test]
    fn a_heading_with_a_section_can_fold() {
        let ps = paras_with(SECTIONS, none_open, true);
        let folds: Vec<(String, bool)> = (ps.iter())
            .filter_map(|p| p.fold.as_ref().map(|(_, shut)| (text_of(p).to_string(), *shut)))
            .collect();
        let names: Vec<&str> = folds.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(names, ["Top", "A", "A1", "B", "C"], "an empty section has nothing to fold");
        assert!(folds.iter().all(|(_, shut)| !shut));
    }

    #[test]
    fn a_folded_heading_hides_its_section_up_to_the_next_as_big() {
        let a = heading_key(2, "A");
        let ps: Vec<String> = paras_with(SECTIONS, |k| k == heading_key(2, "A"), true)
            .iter()
            .filter(|p| matches!(p.kind, Kind::Text { .. }))
            .map(|p| text_of(p).to_string())
            .collect();
        assert_eq!(ps, ["Top", "Intro.", "A", "B", "In B.", "Empty", "C", "item"]);
        let folded = paras_with(SECTIONS, |k| k == heading_key(2, "A"), true);
        let head = folded.iter().find(|p| text_of(p) == "A").unwrap();
        assert_eq!(head.fold, Some((a, true)));
        // The footnotes stay, though the last section is folded.
        let ps = paras_with(SECTIONS, |k| k == heading_key(2, "C"), true);
        assert!(matches!(ps.last().unwrap().kind, Kind::Footnote { .. }));
    }

    #[test]
    fn a_heading_inside_a_list_or_quote_does_not_fold() {
        let ps = paras_with("> ## Quoted\n>\n> Body.\n\n- ## Listed\n\n  Body.", none_open, true);
        assert!(ps.iter().all(|p| p.fold.is_none()));
        // Nor on a card, which is not built again when one is toggled.
        assert!(paras(SECTIONS).iter().all(|p| p.fold.is_none()));
    }

    #[test]
    fn an_image_alone_is_an_image_and_in_text_stays_text() {
        let ps = paras("![logo](assets/rune.png)\n\nSee ![x](https://x.dev/a.png) here.");
        assert!(matches!(&ps[0].kind, Kind::Image { alt, src } if alt.text == "logo" && src == "assets/rune.png"));
        assert!(matches!(ps[1].kind, Kind::Text { .. }));
    }

    #[test]
    fn code_keeps_the_fence_info_and_math_is_labelled() {
        let ps = paras("```rust title=\"src/x.rs\"\nfn a() {}\n```\n\n$$\nx^2\n$$\n\n    indented");
        assert!(matches!(&ps[0].kind, Kind::Code { info, .. } if info == "rust title=\"src/x.rs\""));
        assert!(matches!(&ps[1].kind, Kind::Code { info, text } if info == "math" && text == "x^2"));
        assert!(matches!(&ps[2].kind, Kind::Code { info, text } if info.is_empty() && text == "indented"));
    }

    #[test]
    fn wide_characters_count_double_in_a_term() {
        assert!(term_width("\u{6F22}\u{5B57}") >= 2.0 * term_width("ab"));
        assert!(term_width("\u{D55C}") > term_width("a") * 1.5);
    }

    #[test]
    fn a_wrapping_term_sets_the_item_height() {
        assert_eq!(hang_height(0.0, 22.0, 66.0), 66.0);
        assert_eq!(hang_height(20.0, 44.0, 22.0), 64.0);
    }

    #[test]
    fn tables_with_no_room_get_zero_widths() {
        for avail in [0.0, -10.0] {
            let w = table_widths(&[50.0, 80.0], 40.0, avail);
            assert_eq!(w, [0.0, 0.0]);
        }
        let w = table_widths(&[50.0, 80.0], 0.0, 0.0);
        assert!(w.iter().all(|c| !c.is_nan()), "{w:?}");
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

    #[test]
    fn table_columns_never_pass_the_room_given() {
        let sum = |w: &[f32]| w.iter().sum::<f32>();
        let w = table_widths(&[500.0, 2000.0], 64.0, 300.0);
        assert!(sum(&w) <= 300.01, "{w:?}");
        assert!(w.iter().all(|c| *c >= 64.0), "{w:?}");
        let w = table_widths(&[300.0; 8], 64.0, 300.0);
        assert!(sum(&w) <= 300.01, "too many for least, all shrink: {w:?}");
        let w = table_widths(&[30.0, 900.0, 40.0], 64.0, 200.0);
        assert!(sum(&w) <= 200.01, "{w:?}");
        assert_eq!((w[0], w[2]), (30.0, 40.0));
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
    fn a_clock_reads_local_time() {
        // 2026-10-31 12:00:05 UTC.
        let ms = 1_793_448_005_000;
        assert_eq!(clock(ms, 0), "12:00:05");
        assert_eq!(clock(ms, 3600), "13:00:05");
        assert_eq!(clock(ms, -13 * 3600), "23:00:05");
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
        let s = footer(2_500, Ending::Done, &usage);
        assert_eq!(
            s,
            "2.5 s \u{b7} 2k in \u{b7} 300 out \u{b7} 75% context left"
        );
        let plain = footer(1_000, Ending::Done, &TurnUsage::default());
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
    fn a_link_path_resolves_against_the_project() {
        let dir = std::path::Path::new("C:\\p");
        let rel = |p: &str| project_path(dir, p);
        assert_eq!(rel("src/a.rs").as_deref(), Some("src/a.rs"));
        assert_eq!(rel("./src/../b.rs").as_deref(), Some("b.rs"));
        assert_eq!(rel("c:\\P\\src\\a.rs").as_deref(), Some("src/a.rs"));
        assert_eq!(rel("C:/p/x/y.md").as_deref(), Some("x/y.md"));
        assert_eq!(rel("D:\\elsewhere\\b.rs"), None);
        assert_eq!(rel("C:\\pp\\b.rs"), None, "a sibling that starts the same");
        assert_eq!(rel("C:\\p"), None, "the folder itself");
        assert_eq!(rel("../secret.txt"), None);
        assert_eq!(rel("src/../../secret.txt"), None);
        assert_eq!(rel("C:\\p\\..\\secret.txt"), None);
        assert_eq!(rel("\\\\host\\share\\a.rs"), None);
        assert_eq!(rel("//host/share/a.rs"), None);
        assert_eq!(rel("\\Windows\\win.ini"), None);
        assert_eq!(rel("C:a.rs"), None);
        assert_eq!(rel("a.rs:stream"), None);
        assert_eq!(rel(""), None);
        assert_eq!(
            project_path(std::path::Path::new("C:\\p\\"), "a.rs").as_deref(),
            Some("a.rs")
        );
        let dk = std::path::Path::new("C:\\Users\\Ærø\\p");
        assert_eq!(
            project_path(dk, "C:\\users\\Ærø\\p\\æ.rs").as_deref(),
            Some("æ.rs")
        );
    }

    #[test]
    fn a_long_transcript_shows_its_end_from_a_turn_s_start() {
        assert_eq!(shown_from(&[0, 10], 50, 100), 0, "short shows all");
        assert_eq!(shown_from(&[0, 100, 300, 450], 500, 100), 300);
        assert_eq!(
            shown_from(&[0, 100, 300, 450], 300, 100),
            100,
            "earlier, up to where it shows"
        );
        assert_eq!(
            shown_from(&[0], 1000, 100),
            900,
            "one long turn starts within it"
        );
        assert_eq!(shown_from(&[], 1000, 100), 900);
    }
}
