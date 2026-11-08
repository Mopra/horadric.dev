//! Markdown for the Folio chat: a small parser of our own.
//!
//! Decided 2026-10-25 (docs/plan/headless.md, "Markdown is our own"): no
//! `pulldown-cmark`, only the subset a reply uses, and anything this does
//! not know shows as plain text. The output is shaped for a Direct2D
//! renderer: each run of inline text is one `String` with flat style spans
//! over it, so a span maps straight onto a DirectWrite range.
//!
//! It is pure and cheap enough to rerun over a whole streaming reply on
//! every delta, and it takes any prefix of a reply: an unclosed fence is a
//! code block that runs to the end, an unclosed `**` is literal text.

use std::collections::HashMap;
use std::ops::Range;

/// How deep quotes, lists and links may nest before the rest is plain text,
/// so a hostile reply cannot run the stack out.
const MAX_DEPTH: usize = 32;

/// Extensions that make a lone file name in inline code a path. A name with
/// a directory or a line number needs none of these.
const KNOWN_EXTENSIONS: &[&str] = &[
    "bat", "c", "cc", "cfg", "cjs", "cmd", "cpp", "cs", "css", "go", "h", "hpp", "html", "ini",
    "java", "js", "json", "jsonl", "jsx", "kt", "lock", "lua", "md", "mjs", "php", "ps1", "py",
    "rb", "rs", "scss", "sh", "sql", "svelte", "swift", "toml", "ts", "tsx", "txt", "vue", "xml",
    "yaml", "yml", "zig",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Inline),
    /// `level` is 1 to 6.
    Heading {
        level: u8,
        text: Inline,
    },
    /// Each item is its own blocks, so a nested list is a `List` inside one.
    List {
        ordered: bool,
        start: u64,
        items: Vec<Vec<Block>>,
        /// One per item: a task list's box, ticked or not, None for an
        /// item without one.
        tasks: Vec<Option<bool>>,
    },
    /// `closed` is false while a streaming reply has not sent the closing
    /// fence yet.
    Code {
        lang: String,
        /// The fence's whole info string, such as `rust title="x.rs"`,
        /// for what else it names. `lang` is its first word.
        info: String,
        text: String,
        closed: bool,
    },
    Quote(Vec<Block>),
    /// An HTML `<details>` section: its `<summary>` line, and what folds
    /// under it. `open` is the `open` attribute, how it starts.
    Details {
        summary: Inline,
        body: Vec<Block>,
        open: bool,
    },
    /// The reply's `[^label]: text` notes, gathered at its end, each with
    /// the number its marks show, in that order.
    Footnotes(Vec<(String, Inline)>),
    /// A quote that says what it is: GitHub's `> [!NOTE]`, or a quote
    /// whose first words are `**Note:**`. `title` is any text after
    /// `[!NOTE]` on its line, empty for the kind's own name.
    Callout {
        kind: Callout,
        title: Inline,
        body: Vec<Block>,
    },
    /// Every row has exactly as many cells as `header` and `align`.
    Table {
        header: Vec<Inline>,
        align: Vec<Align>,
        rows: Vec<Vec<Inline>>,
    },
    Rule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Callout {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl Callout {
    fn named(name: &str) -> Option<Callout> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "note" => Callout::Note,
            "tip" => Callout::Tip,
            "important" => Callout::Important,
            "warning" => Callout::Warning,
            "caution" => Callout::Caution,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Callout::Note => "Note",
            Callout::Tip => "Tip",
            Callout::Important => "Important",
            Callout::Warning => "Warning",
            Callout::Caution => "Caution",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

/// A run of text and the styles over it. Spans may overlap and nest (bold
/// inside a link, bold and italic over the same words), are sorted by start,
/// and their ranges are byte offsets into `text` on char boundaries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inline {
    pub text: String,
    pub spans: Vec<Span>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub style: Style,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Style {
    Bold,
    Italic,
    Strike,
    Code,
    /// A `[text](url)`, a `<url>` or a bare `http(s)://` address. The url
    /// always passes [`is_web_link`]: a link to anything else is plain text.
    Link(String),
    /// A file path such as `crates/x/src/y.rs:120`, for the UI to open.
    Path {
        path: String,
        line: Option<u32>,
    },
    /// One key of a shortcut, from `<kbd>Esc</kbd>` or from inline code
    /// that is nothing but a key combination, such as `Ctrl+Shift+P`.
    Key,
    /// `<sup>` and `<sub>`.
    Sup,
    Sub,
    /// A `[^label]` footnote mark, shown small and raised: its number in
    /// the reply, by order of first mark, which its note in
    /// [`Block::Footnotes`] carries too. Outside a whole reply, the label.
    Footnote(String),
    /// `![alt](src)` or `<img>` over its alt text. Unlike a link the src
    /// may be anything, a local file included: the UI decides what it
    /// may show or open.
    Image(String),
}

impl Inline {
    /// The spans with their ranges in UTF-16 code units, which is what
    /// DirectWrite counts in.
    pub fn utf16_spans(&self) -> Vec<(Range<u32>, &Style)> {
        let mut at = vec![0u32; self.text.len() + 1];
        let mut units = 0u32;
        for (i, c) in self.text.char_indices() {
            at[i] = units;
            units += c.len_utf16() as u32;
        }
        at[self.text.len()] = units;
        self.spans
            .iter()
            .map(|s| (at[s.range.start]..at[s.range.end], &s.style))
            .collect()
    }

    fn span(&mut self, start: usize, style: Style) {
        if start < self.text.len() {
            self.spans.push(Span {
                range: start..self.text.len(),
                style,
            });
        }
    }
}

/// Whether a link may be handed to the system to open: a web or mail
/// address. A reply is written by a model that may have read a hostile
/// page, and Windows runs a path, a share or an `ms-` handler on open, so
/// a click on any other target must do nothing.
pub fn is_web_link(url: &str) -> bool {
    let b = url.as_bytes();
    let scheme = ["https://", "http://", "mailto:"]
        .iter()
        .any(|s| b.len() > s.len() && b[..s.len()].eq_ignore_ascii_case(s.as_bytes()));
    scheme && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Parses a whole reply, or any prefix of one, into blocks.
pub fn parse(src: &str) -> Vec<Block> {
    let src = src.strip_suffix('\n').unwrap_or(src);
    let lines: Vec<&str> = src
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let defs = definitions(&lines);
    let was = REFS.with(|r| std::mem::replace(&mut *r.borrow_mut(), defs.links));
    let was_notes = NOTES.with(|n| n.borrow_mut().replace(HashMap::new()));
    let mut out = blocks(&defs.lines, 0, true);
    let mut notes: Vec<(usize, Inline)> = (defs.notes.iter())
        .map(|(label, text)| {
            // After its text, whose marks come before it in reading order;
            // a note never marked is numbered on from the rest.
            let inline = parse_inline(text);
            (note_number(label).unwrap_or(0), inline)
        })
        .collect();
    NOTES.with(|n| *n.borrow_mut() = was_notes);
    REFS.with(|r| *r.borrow_mut() = was);
    if !notes.is_empty() {
        notes.sort_by_key(|(n, _)| *n);
        let notes = (notes.into_iter())
            .map(|(n, t)| (n.to_string(), t))
            .collect();
        out.push(Block::Footnotes(notes));
    }
    out
}

/// The number a footnote label is shown as, given on its first mark;
/// None outside [`parse`], where there is no reply to number in.
fn note_number(label: &str) -> Option<usize> {
    NOTES.with(|n| {
        let mut n = n.borrow_mut();
        let map = n.as_mut()?;
        let next = map.len() + 1;
        Some(*map.entry(label.to_string()).or_insert(next))
    })
}

thread_local! {
    /// The reply's `[id]: url` definitions while it is parsed, so the
    /// inline reader deep inside can resolve `[text][id]` without every
    /// call between carrying them.
    static REFS: std::cell::RefCell<HashMap<String, String>> =
        std::cell::RefCell::new(HashMap::new());
    /// Each footnote label's number while a reply is parsed: its marks
    /// are numbered in the order first met, which is reading order, so a
    /// long label such as `[^note]` shows as a short figure.
    static NOTES: std::cell::RefCell<Option<HashMap<String, usize>>> =
        const { std::cell::RefCell::new(None) };
}

/// A reply's lines with its definitions taken out, and what they define.
struct Defs<'a> {
    lines: Vec<&'a str>,
    links: HashMap<String, String>,
    /// `[^label]: text`, in the order written.
    notes: Vec<(String, String)>,
}

/// How a reference label is matched: case and runs of space do not count.
fn label_key(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Takes `[id]: url` and `[^label]: text` lines out of a reply, outside
/// its fences, wherever they are written: replies put them at the end,
/// and a stray one left in would show as raw text.
fn definitions<'a>(lines: &[&'a str]) -> Defs<'a> {
    let mut d = Defs {
        lines: Vec::with_capacity(lines.len()),
        links: HashMap::new(),
        notes: Vec::new(),
    };
    let mut fence: Option<Fence> = None;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if let Some(f) = &fence {
            if f.closes(line) {
                fence = None;
            }
            d.lines.push(line);
            continue;
        }
        if let Some(f) = Fence::open(line) {
            fence = Some(f);
            d.lines.push(line);
            continue;
        }
        match definition(line) {
            Some(Def::Note(label, first)) => {
                let mut text = first.to_string();
                // Its own further lines are indented under it.
                while i < lines.len() && !is_blank(lines[i]) && indent(lines[i]) >= 2 {
                    text.push('\n');
                    text.push_str(lines[i].trim());
                    i += 1;
                }
                d.notes.push((label.to_string(), text));
            }
            Some(Def::Link(label, url)) => {
                d.links.entry(label_key(label)).or_insert(url.to_string());
            }
            None => d.lines.push(line),
        }
    }
    d
}

enum Def<'a> {
    Link(&'a str, &'a str),
    Note(&'a str, &'a str),
}

fn definition(line: &str) -> Option<Def<'_>> {
    if indent(line) >= 4 {
        return None;
    }
    let rest = line.trim_start().strip_prefix('[')?;
    let (label, after) = rest.split_once("]:")?;
    if label.trim().is_empty() || label.contains(['[', ']']) || label.len() > 100 {
        return None;
    }
    if let Some(note) = label.strip_prefix('^') {
        return (!note.is_empty() && !note.contains(char::is_whitespace))
            .then(|| Def::Note(note, after.trim()));
    }
    let after = after.trim();
    let url = after.split_whitespace().next()?;
    // Only a title may follow, as CommonMark has it, so "[Note]: this is
    // important" is a sentence and not a definition.
    let title = after[url.len()..].trim();
    let quoted = |o: char, c: char| title.len() >= 2 && title.starts_with(o) && title.ends_with(c);
    if !(title.is_empty() || quoted('"', '"') || quoted('\'', '\'') || quoted('(', ')')) {
        return None;
    }
    let url = url
        .strip_prefix('<')
        .and_then(|u| u.strip_suffix('>'))
        .unwrap_or(url);
    // A link goes somewhere: a lone word such as "this" is the first word
    // of a sentence still streaming in, which must not vanish meanwhile.
    if !url.contains(['/', '.', ':', '#']) {
        return None;
    }
    Some(Def::Link(label, url))
}

fn lookup(label: &str) -> Option<String> {
    REFS.with(|r| r.borrow().get(&label_key(label)).cloned())
}

/// Parses one run of inline text: a heading, a cell, a title.
pub fn parse_inline(src: &str) -> Inline {
    let mut out = Inline::default();
    emit(tokenize(src.trim(), 0), &mut out);
    out.spans.sort_by(|a, b| {
        a.range
            .start
            .cmp(&b.range.start)
            .then(b.range.end.cmp(&a.range.end))
    });
    out
}

// Blocks

/// `code` is whether a line four columns in starts an indented code
/// block. Inside a list it does not: there an item's text indented a
/// little too far is far likelier than code, and an item's own nesting
/// already reads as indent.
fn blocks(lines: &[&str], depth: usize, code: bool) -> Vec<Block> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if is_blank(line) {
            i += 1;
            continue;
        }
        let shallow = indent(line) < 4;
        let t = line.trim_start();
        if !shallow && code {
            i = indented_code(lines, i, &mut out);
            continue;
        }
        if shallow {
            if let Some(fence) = Fence::open(line) {
                i = code_block(lines, i, fence, &mut out);
                continue;
            }
            if math_starts(t) {
                i = math(lines, i, &mut out);
                continue;
            }
            if depth < MAX_DEPTH && details_starts(t) {
                i = details(lines, i, depth, code, &mut out);
                continue;
            }
            if let Some((level, text)) = atx(t) {
                out.push(Block::Heading {
                    level,
                    text: parse_inline(text),
                });
                i += 1;
                continue;
            }
            if is_rule(t) {
                out.push(Block::Rule);
                i += 1;
                continue;
            }
            if t.starts_with('>') && depth < MAX_DEPTH {
                i = quote(lines, i, depth, code, &mut out);
                continue;
            }
        }
        if depth < MAX_DEPTH {
            if let Some(marker) = Marker::read(line) {
                i = list(lines, i, marker, depth, &mut out);
                continue;
            }
        }
        if table_starts(lines, i) {
            i = table(lines, i, &mut out);
            continue;
        }
        i = paragraph(lines, i, &mut out);
    }
    out
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// Leading whitespace in columns, a tab reaching the next multiple of four.
fn indent(line: &str) -> usize {
    let mut col = 0;
    for b in line.bytes() {
        match b {
            b' ' => col += 1,
            b'\t' => col += 4 - col % 4,
            _ => break,
        }
    }
    col
}

/// The line with up to `n` columns of leading whitespace taken off.
fn strip_cols(line: &str, n: usize) -> &str {
    let mut col = 0;
    for (i, b) in line.bytes().enumerate() {
        if col >= n {
            return &line[i..];
        }
        match b {
            b' ' => col += 1,
            b'\t' => col += 4 - col % 4,
            _ => return &line[i..],
        }
    }
    ""
}

/// Whether this line ends the paragraph before it.
fn interrupts(lines: &[&str], i: usize) -> bool {
    let line = lines[i];
    if is_blank(line) {
        return true;
    }
    if indent(line) < 4 {
        let t = line.trim_start();
        if Fence::open(line).is_some()
            || atx(t).is_some()
            || is_rule(t)
            || t.starts_with('>')
            || math_starts(t)
            || details_starts(t)
        {
            return true;
        }
    }
    if table_starts(lines, i) {
        return true;
    }
    // As in CommonMark, so a sentence that starts "2024." stays a sentence.
    match Marker::read(line) {
        Some(m) => !m.empty && (!m.ordered || m.start == 1),
        None => false,
    }
}

fn paragraph(lines: &[&str], start: usize, out: &mut Vec<Block>) -> usize {
    let mut i = start + 1;
    let mut level = None;
    while i < lines.len() {
        // Before the interrupt check, as `===` under text is a heading.
        if let Some(l) = setext(lines[i]) {
            level = Some(l);
            break;
        }
        if interrupts(lines, i) {
            break;
        }
        i += 1;
    }
    let text = lines[start..i]
        .iter()
        .map(|l| l.trim_start())
        .collect::<Vec<_>>()
        .join("\n");
    let inline = parse_inline(&text);
    match level {
        Some(level) => {
            out.push(Block::Heading {
                level,
                text: inline,
            });
            i + 1
        }
        None => {
            // A line of nothing but HTML tags, such as `<div align="center">`,
            // has nothing left to show.
            if !inline.text.trim().is_empty() {
                out.push(Block::Paragraph(inline));
            }
            i
        }
    }
}

/// A setext underline: `===` makes the text above a level one heading.
/// `---` under text stays a rule, not a level two heading: replies use it
/// as a separator, and while one streams the text above would flip into
/// a heading and back.
fn setext(line: &str) -> Option<u8> {
    let t = line.trim();
    (indent(line) < 4 && !t.is_empty() && t.bytes().all(|b| b == b'=')).then_some(1)
}

/// Lines four columns in, and the blank lines between them, as code.
fn indented_code(lines: &[&str], start: usize, out: &mut Vec<Block>) -> usize {
    let mut i = start;
    let mut body = Vec::new();
    while i < lines.len() && (is_blank(lines[i]) || indent(lines[i]) >= 4) {
        body.push(strip_cols(lines[i], 4));
        i += 1;
    }
    while body.last().is_some_and(|l| is_blank(l)) {
        body.pop();
    }
    out.push(Block::Code {
        lang: String::new(),
        info: String::new(),
        text: body.join("\n"),
        closed: true,
    });
    i
}

/// A `$$` alone on its line, or `$$...$$` whole on one: a line that only
/// starts with `$$`, as "$$10 more", is prose about money.
fn math_starts(t: &str) -> bool {
    let t = t.trim_end();
    t == "$$" || (t.len() > 4 && t.starts_with("$$") && t.ends_with("$$"))
}

/// A `$$` display math block, shown as code labelled "math": the Folio
/// draws no formulas, and the source reads well enough.
fn math(lines: &[&str], start: usize, out: &mut Vec<Block>) -> usize {
    let first = lines[start].trim()[2..].trim();
    if let Some(one) = first.strip_suffix("$$") {
        out.push(math_block(one.trim().to_string(), true));
        return start + 1;
    }
    let mut body: Vec<&str> = Vec::new();
    if !first.is_empty() {
        body.push(first);
    }
    let mut i = start + 1;
    let mut closed = false;
    while i < lines.len() {
        let t = lines[i].trim();
        i += 1;
        if let Some(last) = t.strip_suffix("$$") {
            if !last.trim().is_empty() {
                body.push(last.trim());
            }
            closed = true;
            break;
        }
        body.push(t);
    }
    // The first `$` of the closing `$$`, as with a fence.
    if !closed && body.last() == Some(&"$") {
        body.pop();
    }
    out.push(math_block(body.join("\n"), closed));
    i
}

fn math_block(text: String, closed: bool) -> Block {
    Block::Code {
        lang: "math".into(),
        info: "math".into(),
        text,
        closed,
    }
}

fn details_starts(t: &str) -> bool {
    let b = t.as_bytes();
    b.len() >= 9 && b[..8].eq_ignore_ascii_case(b"<details") && matches!(b[8], b'>' | b' ' | b'\t')
}

/// Where `tag` (as `<details` or `</details>`) is in `s`, ignoring case.
fn find_tag(s: &str, tag: &str) -> Option<usize> {
    let (b, t) = (s.as_bytes(), tag.as_bytes());
    (0..b.len().saturating_sub(t.len() - 1)).find(|&k| b[k..k + t.len()].eq_ignore_ascii_case(t))
}

/// `<details>` to its `</details>`, or to the end while it streams in.
/// The summary may share a line with the tags or have its own.
fn details(lines: &[&str], start: usize, depth: usize, code: bool, out: &mut Vec<Block>) -> usize {
    let first = lines[start].trim_start();
    let close_at = first.find('>').unwrap_or(first.len());
    let open = first[..close_at].to_ascii_lowercase().contains(" open")
        || first[..close_at].to_ascii_lowercase().contains("\topen");
    let mut inner: Vec<String> = Vec::new();
    let mut nested = 0usize;
    let mut i = start;
    let mut rest = first.get(close_at + 1..).unwrap_or("").to_string();
    loop {
        // Inner details on this line count, so the right close ends it.
        // One pass from `<` to `<`: looking for both tags afresh at every
        // step read a line of many opens once per open.
        let mut cut = None;
        let mut from = 0;
        while let Some(k) = rest[from..].find('<') {
            let at = from + k;
            let b = &rest.as_bytes()[at..];
            if b.len() >= 8 && b[..8].eq_ignore_ascii_case(b"<details") {
                nested += 1;
                from = at + 8;
            } else if b.len() >= 10 && b[..10].eq_ignore_ascii_case(b"</details>") {
                if nested == 0 {
                    cut = Some(at);
                    break;
                }
                nested -= 1;
                from = at + 10;
            } else {
                from = at + 1;
            }
        }
        i += 1;
        if let Some(c) = cut {
            inner.push(rest[..c].to_string());
            break;
        }
        inner.push(rest);
        match lines.get(i) {
            Some(l) => rest = l.to_string(),
            None => break,
        }
    }
    let mut all = inner.join("\n");
    let mut summary = Inline::default();
    if let Some(s) = find_tag(&all, "<summary") {
        if let Some(gt) = all[s..].find('>') {
            let from = s + gt + 1;
            let (text, end) = match find_tag(&all[from..], "</summary>") {
                Some(e) => (&all[from..from + e], from + e + 10),
                None => (&all[from..], all.len()),
            };
            summary = parse_inline(&text.replace('\n', " "));
            all.replace_range(s..end, "");
        }
    }
    let body_lines: Vec<&str> = all.split('\n').collect();
    out.push(Block::Details {
        summary,
        body: blocks(&body_lines, depth + 1, code),
        open,
    });
    i
}

fn atx(t: &str) -> Option<(u8, &str)> {
    let n = t.bytes().take_while(|&b| b == b'#').count();
    if n == 0 || n > 6 {
        return None;
    }
    let rest = &t[n..];
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    let text = rest.trim();
    let bare = text.trim_end_matches('#');
    let text = if bare.is_empty() {
        ""
    } else if bare.ends_with([' ', '\t']) {
        bare.trim_end()
    } else {
        text
    };
    Some((n as u8, text))
}

fn is_rule(t: &str) -> bool {
    let mut ch = None;
    let mut n = 0;
    for b in t.bytes() {
        match b {
            b' ' | b'\t' => {}
            b'-' | b'*' | b'_' => {
                if ch.is_some_and(|c| c != b) {
                    return false;
                }
                ch = Some(b);
                n += 1;
            }
            _ => return false,
        }
    }
    n >= 3
}

struct Fence {
    ch: u8,
    len: usize,
    indent: usize,
    lang: String,
    info: String,
}

impl Fence {
    fn open(line: &str) -> Option<Fence> {
        let indent = indent(line);
        if indent >= 4 {
            return None;
        }
        let t = line.trim_start();
        let ch = *t.as_bytes().first()?;
        if ch != b'`' && ch != b'~' {
            return None;
        }
        let len = t.bytes().take_while(|&b| b == ch).count();
        let info = t[len..].trim();
        // "```x```" on one line is inline code, not a fence.
        if len < 3 || (ch == b'`' && info.contains('`')) {
            return None;
        }
        let lang = info.split_whitespace().next().unwrap_or("").to_string();
        Some(Fence {
            ch,
            len,
            indent,
            lang,
            info: info.to_string(),
        })
    }

    fn closes(&self, line: &str) -> bool {
        let t = line.trim();
        indent(line) < 4 && t.len() >= self.len && t.bytes().all(|b| b == self.ch)
    }
}

fn code_block(lines: &[&str], start: usize, fence: Fence, out: &mut Vec<Block>) -> usize {
    let mut i = start + 1;
    let mut body = Vec::new();
    let mut closed = false;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if fence.closes(line) {
            closed = true;
            break;
        }
        body.push(strip_cols(line, fence.indent));
    }
    // A last line of fewer fence characters is the close still arriving:
    // shown, it would blink into the code and out again a delta later.
    if !closed
        && body.last().is_some_and(|l| {
            let t = l.trim();
            !t.is_empty() && t.len() < fence.len && t.bytes().all(|b| b == fence.ch)
        })
    {
        body.pop();
    }
    out.push(Block::Code {
        lang: fence.lang,
        info: fence.info,
        text: body.join("\n"),
        closed,
    });
    i
}

fn quote(lines: &[&str], start: usize, depth: usize, code: bool, out: &mut Vec<Block>) -> usize {
    let mut inner: Vec<&str> = Vec::new();
    let mut i = start;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim_start();
        if indent(line) < 4 && t.starts_with('>') {
            let rest = &t[1..];
            inner.push(rest.strip_prefix([' ', '\t']).unwrap_or(rest));
        } else if inner.last().is_some_and(|p| !is_blank(p)) && !interrupts(lines, i) {
            // A lazy line: it carries on the quoted paragraph without a ">".
            inner.push(line);
        } else {
            break;
        }
        i += 1;
    }
    match callout(&mut inner) {
        Some((kind, title)) => out.push(Block::Callout {
            kind,
            title,
            body: blocks(&inner, depth + 1, code),
        }),
        None => out.push(Block::Quote(blocks(&inner, depth + 1, code))),
    }
    i
}

/// Whether a quote's lines open as a callout: `[!NOTE]` on the first line,
/// or `**Note:**` (or `**Note**:`) starting it. The marker is taken off the
/// lines, which then hold only the body.
fn callout(inner: &mut Vec<&str>) -> Option<(Callout, Inline)> {
    let first = (*inner.first()?).trim();
    if let Some(rest) = first.strip_prefix("[!") {
        let (name, after) = rest.split_once(']')?;
        let kind = Callout::named(name)?;
        let title = parse_inline(after);
        inner.remove(0);
        return Some((kind, title));
    }
    let rest = first.strip_prefix("**")?;
    let (name, after) = rest.split_once("**")?;
    let (name, after) = match name.strip_suffix(':') {
        Some(n) => (n, after),
        None => (name, after.strip_prefix(':')?),
    };
    if name.trim() != name || !(after.is_empty() || after.starts_with([' ', '\t'])) {
        return None;
    }
    let kind = Callout::named(name)?;
    inner[0] = after.trim_start();
    Some((kind, Inline::default()))
}

struct Marker<'a> {
    ordered: bool,
    ch: u8,
    start: u64,
    /// The column an item's own lines must reach to stay inside it.
    content: usize,
    empty: bool,
    body: &'a str,
}

impl<'a> Marker<'a> {
    fn read(line: &'a str) -> Option<Marker<'a>> {
        let indent = indent(line);
        let t = line.trim_start_matches([' ', '\t']);
        let b = t.as_bytes();
        let (ordered, ch, start, width) = match *b.first()? {
            c @ (b'-' | b'*' | b'+') => (false, c, 0, 1),
            b'0'..=b'9' => {
                let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
                let ch = *b.get(digits)?;
                if digits > 9 || (ch != b'.' && ch != b')') {
                    return None;
                }
                (true, ch, t[..digits].parse().ok()?, digits + 1)
            }
            _ => return None,
        };
        let after = &t[width..];
        if !(after.is_empty() || after.starts_with([' ', '\t'])) {
            return None;
        }
        let body = after.trim_start_matches([' ', '\t']);
        let spaces = after.len() - body.len();
        let empty = body.trim().is_empty();
        let pad = if empty || spaces > 4 { 1 } else { spaces };
        Some(Marker {
            ordered,
            ch,
            start,
            content: indent + width + pad,
            empty,
            body,
        })
    }
}

fn list<'a>(
    lines: &[&'a str],
    start: usize,
    first: Marker<'a>,
    depth: usize,
    out: &mut Vec<Block>,
) -> usize {
    let (ordered, ch, number) = (first.ordered, first.ch, first.start);
    let mut marker = first;
    let mut items = Vec::new();
    let mut tasks = Vec::new();
    let mut i = start;
    loop {
        let (task, first) = match task_box(marker.body) {
            Some((done, rest)) => (Some(done), rest),
            None => (None, marker.body),
        };
        tasks.push(task);
        let mut body = vec![first];
        i += 1;
        while i < lines.len() {
            let line = lines[i];
            if is_blank(line) {
                body.push("");
            } else if indent(line) >= marker.content {
                body.push(strip_cols(line, marker.content));
            } else if body.last().is_some_and(|p| !is_blank(p))
                && Marker::read(line).is_none()
                && !interrupts(lines, i)
            {
                body.push(line.trim_start());
            } else {
                break;
            }
            i += 1;
        }
        items.push(blocks(&body, depth + 1, false));
        match lines.get(i).and_then(|l| Marker::read(l)) {
            Some(next) if next.ordered == ordered && next.ch == ch => marker = next,
            _ => break,
        }
    }
    out.push(Block::List {
        ordered,
        start: number,
        items,
        tasks,
    });
    i
}

/// A task list item's `[ ]` or `[x]` and what follows it. The box needs
/// words after it, so a bare "[ ]" in a reply still streaming stays text
/// for the moment the words take to arrive.
fn task_box(body: &str) -> Option<(bool, &str)> {
    let b = body.as_bytes();
    if b.len() < 4 || b[0] != b'[' || b[2] != b']' || !matches!(b[3], b' ' | b'\t') {
        return None;
    }
    let done = match b[1] {
        b' ' => false,
        b'x' | b'X' => true,
        _ => return None,
    };
    let rest = body[3..].trim_start_matches([' ', '\t']);
    (!rest.trim().is_empty()).then_some((done, rest))
}

fn table_starts(lines: &[&str], i: usize) -> bool {
    let Some(next) = lines.get(i + 1) else {
        return false;
    };
    let head = lines[i];
    indent(head) < 4
        && head.contains('|')
        && delimiter_row(next).is_some_and(|a| a.len() == cells(head).len())
}

fn delimiter_row(line: &str) -> Option<Vec<Align>> {
    if indent(line) >= 4 || !line.contains('|') {
        return None;
    }
    cells(line)
        .iter()
        .map(|c| {
            let left = c.starts_with(':');
            let right = c.len() > 1 && c.ends_with(':');
            let dashes = c.trim_matches(':');
            if dashes.is_empty() || !dashes.bytes().all(|b| b == b'-') {
                return None;
            }
            Some(match (left, right) {
                (true, true) => Align::Center,
                (true, false) => Align::Left,
                (false, true) => Align::Right,
                (false, false) => Align::None,
            })
        })
        .collect()
}

/// A table row's cells. A pipe inside inline code does not split, since
/// replies put `a | b` in code without escaping it; `\|` is a pipe anywhere.
fn cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = match t.strip_suffix('|') {
        Some(inner) if !inner.ends_with('\\') => inner,
        _ => t,
    };
    let b = t.as_bytes();
    let mut out = Vec::new();
    let mut cell = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'`' => {
                let n = run(b, i, b'`');
                i = find_run(b, i + n, n).map_or(i + n, |close| close + n);
            }
            b'|' => {
                out.push(t[cell..i].trim().replace("\\|", "|"));
                cell = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    out.push(t[cell..].trim().replace("\\|", "|"));
    out
}

fn table(lines: &[&str], start: usize, out: &mut Vec<Block>) -> usize {
    let header: Vec<Inline> = cells(lines[start])
        .iter()
        .map(|c| parse_inline(c))
        .collect();
    let align = delimiter_row(lines[start + 1]).unwrap_or_default();
    let mut rows = Vec::new();
    let mut i = start + 2;
    while i < lines.len() && !interrupts(lines, i) {
        let mut row: Vec<Inline> = cells(lines[i]).iter().map(|c| parse_inline(c)).collect();
        row.resize(header.len(), Inline::default());
        rows.push(row);
        i += 1;
    }
    out.push(Block::Table {
        header,
        align,
        rows,
    });
    i
}

// Inline

/// Inline text cut into what emphasis matching needs to see. Delimiter
/// runs stay apart because whether they open, close or stay literal is only
/// known once the whole run of text has been seen.
struct Toks {
    toks: Vec<Tok>,
    delims: Vec<Delim>,
}

enum Tok {
    Text(String),
    /// An index into `Toks::delims`.
    Delim(usize),
    Code(String),
    Link(Box<Toks>, String),
    Url(String),
    Path(String, String, Option<u32>),
    Key(String),
    Image(Box<Toks>, String),
    Footnote(String),
    /// An HTML tag that styles what follows until its close, such as
    /// `<b>`. A close matches the nearest open of the same kind.
    Open(Style),
    Close(Style),
}

struct Delim {
    ch: u8,
    orig: usize,
    len: usize,
    open: bool,
    close: bool,
    active: bool,
}

fn run(b: &[u8], i: usize, c: u8) -> usize {
    b[i..].iter().take_while(|&&x| x == c).count()
}

/// The next run of exactly `n` backticks from `from`, as inline code closes.
fn find_run(b: &[u8], from: usize, n: usize) -> Option<usize> {
    let mut j = from;
    while j < b.len() {
        if b[j] == b'`' {
            let m = run(b, j, b'`');
            if m == n {
                return Some(j);
            }
            j += m;
        } else {
            j += 1;
        }
    }
    None
}

fn flush(buf: &mut String, toks: &mut Vec<Tok>) {
    if !buf.is_empty() {
        toks.push(Tok::Text(std::mem::take(buf)));
    }
}

/// A newline inside a paragraph stays a newline. CommonMark would make a
/// soft break a space, but the agent wrote the reply for a terminal that
/// shows its newlines, and the Folio should read the same.
fn line_break(buf: &mut String) {
    let kept = buf.trim_end_matches([' ', '\t']).len();
    buf.truncate(kept);
    buf.push('\n');
}

fn tokenize(s: &str, depth: usize) -> Toks {
    let b = s.as_bytes();
    let mut toks = Vec::new();
    let mut delims = Vec::new();
    let mut buf = String::new();
    let mut closes = HashMap::new();
    let mut lasts = Lasts::default();
    let mut no_path_before = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => {
                match b.get(i + 1) {
                    Some(b'\n') => {
                        line_break(&mut buf);
                        i += 2;
                    }
                    Some(&c) if c.is_ascii_punctuation() => {
                        buf.push(c as char);
                        i += 2;
                    }
                    _ => {
                        buf.push('\\');
                        i += 1;
                    }
                }
                continue;
            }
            b'\n' => {
                line_break(&mut buf);
                i += 1;
                while i < b.len() && matches!(b[i], b' ' | b'\t') {
                    i += 1;
                }
                continue;
            }
            b'`' => {
                let n = run(b, i, b'`');
                match find_run(b, i + n, n) {
                    Some(close) => {
                        flush(&mut buf, &mut toks);
                        toks.push(Tok::Code(code_text(&s[i + n..close])));
                        i = close + n;
                    }
                    None => {
                        buf.push_str(&s[i..i + n]);
                        i += n;
                    }
                }
                continue;
            }
            c @ (b'*' | b'_' | b'~') => {
                let n = run(b, i, c);
                flush(&mut buf, &mut toks);
                toks.push(Tok::Delim(delims.len()));
                delims.push(delim(s, i, n, c));
                i += n;
                continue;
            }
            b'[' | b'!' if depth < MAX_DEPTH => {
                let image = b[i] == b'!';
                let open = if image { i + 1 } else { i };
                if !image {
                    if let Some((label, end)) = footnote_at(s, i) {
                        flush(&mut buf, &mut toks);
                        toks.push(Tok::Footnote(label.to_string()));
                        i = end;
                        continue;
                    }
                }
                if b.get(open) == Some(&b'[') {
                    if let Some((text, url, end)) = link_at(s, open, &mut closes) {
                        flush(&mut buf, &mut toks);
                        let inner = Box::new(tokenize(&s[text], depth + 1));
                        toks.push(if image {
                            Tok::Image(inner, url)
                        } else {
                            Tok::Link(inner, url)
                        });
                        i = end;
                        continue;
                    }
                }
            }
            b'&' => {
                if let Some((c, end)) = entity_at(s, i) {
                    buf.push(c);
                    i = end;
                    continue;
                }
            }
            b'<' => {
                if let Some((key, end)) = kbd_at(s, i) {
                    flush(&mut buf, &mut toks);
                    toks.push(Tok::Key(key.to_string()));
                    i = end;
                    continue;
                }
                if let Some((url, end)) = angle_link(s, i) {
                    flush(&mut buf, &mut toks);
                    toks.push(Tok::Url(url.to_string()));
                    i = end;
                    continue;
                }
                if let Some(tag) = html_at(s, i, &mut lasts) {
                    i = html(s, tag, depth, &mut buf, &mut toks);
                    continue;
                }
            }
            _ => {}
        }
        // Inside a link's text the link is already the target. Right after
        // a `](` that made no link, the destination is still streaming in,
        // and linking its first half would flicker before the whole lands.
        if depth == 0 && at_word_start(s, i) && !s[..i].ends_with("](") {
            if let Some(end) = url_at(s, i) {
                flush(&mut buf, &mut toks);
                toks.push(Tok::Url(s[i..end].to_string()));
                i = end;
                continue;
            }
            if i >= no_path_before {
                match path_at(s, i) {
                    Ok((end, path, line)) => {
                        flush(&mut buf, &mut toks);
                        toks.push(Tok::Path(s[i..end].to_string(), path, line));
                        i = end;
                        continue;
                    }
                    // No path starts inside a run that is none, and trying
                    // each of its words would read a long snake_case run
                    // once per word.
                    Err(run_end) => no_path_before = run_end,
                }
            }
        }
        let Some(c) = s[i..].chars().next() else {
            break;
        };
        buf.push(c);
        i += c.len_utf8();
    }
    flush(&mut buf, &mut toks);
    Toks { toks, delims }
}

fn code_text(raw: &str) -> String {
    let t = raw.replace('\n', " ");
    if t.len() >= 2 && t.starts_with(' ') && t.ends_with(' ') && !t.trim().is_empty() {
        t[1..t.len() - 1].to_string()
    } else {
        t
    }
}

fn punct(c: char) -> bool {
    c.is_ascii_punctuation() || (!c.is_ascii() && !c.is_alphanumeric() && !c.is_whitespace())
}

/// A delimiter run and whether it may open or close, by CommonMark's
/// flanking rules. Strikethrough takes only `~~`: a single tilde is far more
/// often "~5 minutes" or "~/path" in a reply than a strike.
fn delim(s: &str, i: usize, n: usize, ch: u8) -> Delim {
    let before = s[..i].chars().next_back().unwrap_or(' ');
    let after = s[i + n..].chars().next().unwrap_or(' ');
    let left = !after.is_whitespace() && (!punct(after) || before.is_whitespace() || punct(before));
    let right =
        !before.is_whitespace() && (!punct(before) || after.is_whitespace() || punct(after));
    let (open, close) = match ch {
        b'_' => (
            left && (!right || punct(before)),
            right && (!left || punct(after)),
        ),
        b'~' if n == 2 => (left, right),
        b'~' => (false, false),
        _ => (left, right),
    };
    Delim {
        ch,
        orig: n,
        len: n,
        open,
        close,
        active: true,
    }
}

/// The `]` that closes the `[` at `open`, by way of `closes`: every bracket
/// a scan passes learns its close too, and a scan steps over a pair it
/// already knows and stops at a bracket known never to close. A streamed
/// reply full of unmatched brackets is so read once per parse, not once
/// per bracket.
fn close_of(s: &str, open: usize, closes: &mut HashMap<usize, Option<usize>>) -> Option<usize> {
    if let Some(&known) = closes.get(&open) {
        return known;
    }
    let b = s.as_bytes();
    let mut stack = vec![open];
    let mut j = open + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'`' => {
                let n = run(b, j, b'`');
                j = find_run(b, j + n, n).map_or(j + n, |c| c + n);
            }
            b'[' => match closes.get(&j) {
                Some(Some(c)) => j = c + 1,
                Some(None) => break,
                None => {
                    stack.push(j);
                    j += 1;
                }
            },
            b']' => {
                let o = stack
                    .pop()
                    .expect("the scan ends when its own bracket closes");
                closes.insert(o, Some(j));
                if stack.is_empty() {
                    return Some(j);
                }
                j += 1;
            }
            _ => j += 1,
        }
    }
    for o in stack {
        closes.insert(o, None);
    }
    None
}

/// Where a link's destination or title ends: at `stop`, or None when a
/// bracket or the end comes first. Stopping at a bracket keeps the scans
/// from each `](` apart, so "[](" repeated is read once, not once per link.
fn scan_to(b: &[u8], mut k: usize, stop: impl Fn(u8, &mut usize) -> bool) -> Option<usize> {
    let mut parens = 0usize;
    loop {
        let c = *b.get(k)?;
        if c == b'\\' && b.get(k + 1).is_some_and(|n| n.is_ascii_punctuation()) {
            k += 2;
            continue;
        }
        if matches!(c, b'[' | b']') {
            return None;
        }
        if stop(c, &mut parens) {
            return Some(k);
        }
        k += 1;
    }
}

/// `[text](url "title")` from the `[` at `open`: the text's range, the url
/// and where the link ends. None leaves the bracket as literal text.
fn link_at(
    s: &str,
    open: usize,
    closes: &mut HashMap<usize, Option<usize>>,
) -> Option<(Range<usize>, String, usize)> {
    let b = s.as_bytes();
    let close = close_of(s, open, closes)?;
    if b.get(close + 1) != Some(&b'(') {
        return reference(s, open, close);
    }
    let mut k = skip_space(b, close + 2);
    let url = if b.get(k) == Some(&b'<') {
        let end = scan_to(b, k + 1, |c, _| matches!(c, b'>' | b'\n'))?;
        if b[end] != b'>' {
            return None;
        }
        let url = s[k + 1..end].to_string();
        k = end + 1;
        url
    } else {
        let from = k;
        k = scan_to(b, k, |c, parens| match c {
            b'(' => {
                *parens += 1;
                false
            }
            b')' if *parens == 0 => true,
            b')' => {
                *parens -= 1;
                false
            }
            c => c.is_ascii_whitespace(),
        })?;
        s[from..k].to_string()
    };
    k = skip_space(b, k);
    if let Some(&quote @ (b'"' | b'\'' | b'(')) = b.get(k) {
        let end = if quote == b'(' { b')' } else { quote };
        k = scan_to(b, k + 1, |c, _| c == end)?;
        k = skip_space(b, k + 1);
    }
    if b.get(k) != Some(&b')') {
        return None;
    }
    Some((open + 1..close, url, k + 1))
}

/// `[text][id]`, `[text][]` or `[text]` for a defined `[id]: url`. An
/// undefined label leaves the brackets as written.
fn reference(s: &str, open: usize, close: usize) -> Option<(Range<usize>, String, usize)> {
    // Labels are short, and a reply full of brackets must not have each
    // one's whole text looked up, which would be read once per bracket.
    const LABEL_MAX: usize = 200;
    if REFS.with(|r| r.borrow().is_empty()) {
        return None;
    }
    let text = &s[open + 1..close];
    if s.as_bytes().get(close + 1) == Some(&b'[') {
        let rest = &s[close + 2..];
        let e = rest.bytes().take(LABEL_MAX).position(|c| c == b']')?;
        let label = &rest[..e];
        if label.contains('[') {
            return None;
        }
        let label = if label.trim().is_empty() { text } else { label };
        return lookup(label).map(|u| (open + 1..close, u, close + 2 + e + 1));
    }
    if text.len() > LABEL_MAX || text.trim().is_empty() || text.starts_with('^') {
        return None;
    }
    lookup(text).map(|u| (open + 1..close, u, close + 1))
}

/// `[^label]` at `i`, a footnote mark: its label and where it ends. A
/// label is a short word, so "[^ see above]" stays as typed.
fn footnote_at(s: &str, i: usize) -> Option<(&str, usize)> {
    let rest = s.get(i..)?.strip_prefix("[^")?;
    let e = rest.bytes().take(31).position(|c| c == b']')?;
    let label = &rest[..e];
    let ok = !label.is_empty()
        && label.len() <= 30
        && label
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'));
    ok.then_some((label, i + 2 + e + 1))
}

/// An HTML entity at `i`: what it stands for and where it ends. Only the
/// names replies use; any other stays as typed.
fn entity_at(s: &str, i: usize) -> Option<(char, usize)> {
    let rest = s.get(i + 1..)?;
    let semi = rest.bytes().take(12).position(|c| c == b';')?;
    let name = &rest[..semi];
    let end = i + 1 + semi + 1;
    if let Some(num) = name.strip_prefix('#') {
        let n = match num.strip_prefix(['x', 'X']) {
            Some(hex) if !hex.is_empty() && hex.len() <= 6 => u32::from_str_radix(hex, 16).ok()?,
            None if !num.is_empty() && num.len() <= 7 => num.parse().ok()?,
            _ => return None,
        };
        let c = match char::from_u32(n) {
            Some(c) if n != 0 => c,
            _ => '\u{FFFD}',
        };
        return Some((c, end));
    }
    let c = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "copy" => '\u{a9}',
        "reg" => '\u{ae}',
        "trade" => '\u{2122}',
        "deg" => '\u{b0}',
        "plusmn" => '\u{b1}',
        "times" => '\u{d7}',
        "divide" => '\u{f7}',
        "middot" => '\u{b7}',
        "bull" => '\u{2022}',
        "hellip" => '\u{2026}',
        "ndash" => '\u{2013}',
        "mdash" => '\u{2014}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "ldquo" => '\u{201c}',
        "rdquo" => '\u{201d}',
        "laquo" => '\u{ab}',
        "raquo" => '\u{bb}',
        "larr" => '\u{2190}',
        "uarr" => '\u{2191}',
        "rarr" => '\u{2192}',
        "darr" => '\u{2193}',
        "check" => '\u{2713}',
        _ => return None,
    };
    Some((c, end))
}

/// Text with its entities decoded, for HTML `<code>`'s text.
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s.as_bytes()[i] == b'&' {
            if let Some((c, end)) = entity_at(s, i) {
                out.push(c);
                i = end;
                continue;
            }
        }
        let c = s[i..].chars().next().expect("i is on a char boundary");
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// HTML elements a reply might write. A tag of another name is taken
/// as HTML only when it has attributes, so `Option<T>` or `Vec<u8>` in
/// prose stays as written.
const HTML_TAGS: &[&str] = &[
    "a",
    "abbr",
    "b",
    "big",
    "blockquote",
    "br",
    "center",
    "cite",
    "code",
    "dd",
    "del",
    "details",
    "dfn",
    "div",
    "dl",
    "dt",
    "em",
    "figcaption",
    "figure",
    "font",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hr",
    "i",
    "img",
    "ins",
    "kbd",
    "li",
    "mark",
    "ol",
    "p",
    "picture",
    "pre",
    "q",
    "s",
    "samp",
    "small",
    "source",
    "span",
    "strike",
    "strong",
    "sub",
    "summary",
    "sup",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "tr",
    "tt",
    "u",
    "ul",
    "var",
    "wbr",
];

/// Where each closing text a tag waits for is last seen in the text
/// being read, looked up once, so a reply of many tags that never close
/// is not read to its end once per tag.
#[derive(Default)]
struct Lasts(HashMap<String, Option<usize>>);

impl Lasts {
    /// Whether `close` comes anywhere at or after `from` in `s`.
    fn after(&mut self, s: &str, close: &str, from: usize) -> bool {
        let last = *self
            .0
            .entry(close.to_string())
            .or_insert_with(|| s.rfind(close));
        last.is_some_and(|l| l >= from)
    }
}

struct Tag<'a> {
    /// Lower case; empty for a comment.
    name: String,
    close: bool,
    attrs: &'a str,
    end: usize,
}

/// An HTML tag or comment at `i`.
fn html_at<'a>(s: &'a str, i: usize, lasts: &mut Lasts) -> Option<Tag<'a>> {
    let rest = s.get(i..)?;
    if let Some(body) = rest.strip_prefix("<!--") {
        // Only a closed comment hides, so a stray "<!--" in prose does not
        // take the rest of the paragraph with it.
        if !lasts.after(s, "-->", i + 4) {
            return None;
        }
        let end = i + 4 + body.find("-->")? + 3;
        return Some(Tag {
            name: String::new(),
            close: false,
            attrs: "",
            end,
        });
    }
    let (close, after) = match rest[1..].strip_prefix('/') {
        Some(a) => (true, a),
        None => (false, &rest[1..]),
    };
    let n = after
        .bytes()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == b'-')
        .count();
    if n == 0 || !after.as_bytes()[0].is_ascii_alphabetic() {
        return None;
    }
    let gt = after.find(['>', '<', '\n'])?;
    if gt > 400 || after.as_bytes()[gt] != b'>' {
        return None;
    }
    // Replies write HTML in lower case; `Foo<S>`, `Bar<I>` or `<T>` is a
    // type in prose.
    let name = &after[..n];
    if name.bytes().any(|c| c.is_ascii_uppercase()) {
        return None;
    }
    let attrs = after[n..gt].trim().trim_end_matches('/').trim();
    if n < gt && !matches!(after.as_bytes()[n], b' ' | b'\t' | b'/') {
        return None;
    }
    let known = HTML_TAGS.contains(&name);
    if !known && (close || !attrs.contains('=')) {
        return None;
    }
    let end = i + 1 + usize::from(close) + gt + 1;
    // `Vec<p>` or `Box<a>` is a generic too, unless the tag closes later.
    if n == 1 && !close && attrs.is_empty() && !lasts.after(s, &format!("</{name}>"), end) {
        return None;
    }
    let name = name.to_string();
    Some(Tag {
        name,
        close,
        attrs,
        end,
    })
}

/// An attribute's value in a tag, quoted or not.
fn attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let lower = attrs.to_ascii_lowercase();
    let mut from = 0;
    while let Some(k) = lower[from..].find(name) {
        let at = from + k;
        from = at + name.len();
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let after = attrs[from..].trim_start();
        if !before_ok || !after.starts_with('=') {
            continue;
        }
        let v = after[1..].trim_start();
        return Some(match v.chars().next() {
            Some(q @ ('"' | '\'')) => v[1..].split(q).next().unwrap_or(""),
            _ => v.split_whitespace().next().unwrap_or(""),
        });
    }
    None
}

/// What an HTML tag does to the text: a break, a style, an image, or
/// nothing, its text kept and the tag never shown. Returns where it ends.
fn html(s: &str, tag: Tag, depth: usize, buf: &mut String, toks: &mut Vec<Tok>) -> usize {
    let style = match tag.name.as_str() {
        "b" | "strong" => Some(Style::Bold),
        "i" | "em" | "cite" | "var" | "dfn" => Some(Style::Italic),
        "s" | "del" | "strike" => Some(Style::Strike),
        "sup" => Some(Style::Sup),
        "sub" => Some(Style::Sub),
        "a" if tag.close => Some(Style::Link(String::new())),
        "a" => attr(tag.attrs, "href")
            .filter(|u| is_web_link(u))
            .map(|u| Style::Link(u.to_string())),
        _ => None,
    };
    if let Some(style) = style {
        flush(buf, toks);
        toks.push(if tag.close {
            Tok::Close(style)
        } else {
            Tok::Open(style)
        });
        return tag.end;
    }
    match tag.name.as_str() {
        "br" => line_break(buf),
        "code" | "tt" | "samp" if !tag.close => {
            // Looked for only so far, so a reply of many unclosed tags is
            // not read to its end once per tag.
            let mut stop = (tag.end + 2000).min(s.len());
            while !s.is_char_boundary(stop) {
                stop -= 1;
            }
            if let Some(e) = find_tag(&s[tag.end..stop], &format!("</{}>", tag.name)) {
                flush(buf, toks);
                let text = decode_entities(&s[tag.end..tag.end + e]);
                toks.push(Tok::Code(text));
                return tag.end + e + tag.name.len() + 3;
            }
        }
        "img" if !tag.close && depth < MAX_DEPTH => {
            if let Some(src) = attr(tag.attrs, "src").filter(|v| !v.is_empty()) {
                flush(buf, toks);
                let alt = attr(tag.attrs, "alt").unwrap_or("");
                toks.push(Tok::Image(
                    Box::new(tokenize(alt, depth + 1)),
                    src.to_string(),
                ));
            }
        }
        _ => {}
    }
    tag.end
}

fn skip_space(b: &[u8], mut k: usize) -> usize {
    while b.get(k).is_some_and(|c| matches!(c, b' ' | b'\t' | b'\n')) {
        k += 1;
    }
    k
}

/// `<kbd>Esc</kbd>` at `i`: the key's name and where it ends. Only a
/// short name on one line, so a stray `<kbd>` stays as typed.
fn kbd_at(s: &str, i: usize) -> Option<(&str, usize)> {
    if !s.get(i..i + 5)?.eq_ignore_ascii_case("<kbd>") {
        return None;
    }
    let rest = &s[i + 5..];
    let end = rest.find('<')?;
    let name = rest[..end].trim();
    let ok = rest.get(end..end + 6)?.eq_ignore_ascii_case("</kbd>")
        && !name.is_empty()
        && name.chars().count() <= 24
        && !name.contains('\n');
    ok.then_some((name, i + 5 + end + 6))
}

/// Inline code that is a key combination and nothing else, such as
/// `Ctrl+Shift+P` or `Alt+F4`: the byte range of each key. It takes a
/// modifier and then a single key or a named one, so `a+b`, `C++` and
/// `i+1` stay code.
fn key_combo(s: &str) -> Option<Vec<Range<usize>>> {
    if s.len() > 40 || s.contains(char::is_whitespace) {
        return None;
    }
    // "Ctrl++" presses the plus key.
    let (head, last) = match s.strip_suffix("++") {
        Some(h) => (h, s.len() - 1..s.len()),
        None => {
            let (h, k) = s.rsplit_once('+')?;
            (h, h.len() + 1..h.len() + 1 + k.len())
        }
    };
    if !is_key(&s[last.clone()]) {
        return None;
    }
    let mut out = Vec::new();
    let mut at = 0;
    for part in head.split('+') {
        if !is_modifier(part) {
            return None;
        }
        out.push(at..at + part.len());
        at += part.len() + 1;
    }
    out.push(last);
    Some(out)
}

fn is_modifier(k: &str) -> bool {
    const MODS: &[&str] = &[
        "ctrl", "control", "shift", "alt", "altgr", "option", "opt", "cmd", "command", "win",
        "super", "meta", "fn", "\u{2318}", "\u{21e7}", "\u{2325}", "\u{2303}",
    ];
    MODS.iter().any(|m| m.eq_ignore_ascii_case(k))
}

fn is_key(k: &str) -> bool {
    const NAMED: &[&str] = &[
        "enter",
        "return",
        "esc",
        "escape",
        "tab",
        "space",
        "backspace",
        "delete",
        "del",
        "insert",
        "ins",
        "home",
        "end",
        "pgup",
        "pgdn",
        "pageup",
        "pagedown",
        "up",
        "down",
        "left",
        "right",
        "plus",
        "minus",
    ];
    let mut chars = k.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => c.is_ascii_graphic() || "\u{2190}\u{2191}\u{2192}\u{2193}".contains(c),
        (Some('F' | 'f'), Some(_)) => k[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)),
        _ => NAMED.iter().any(|n| n.eq_ignore_ascii_case(k)),
    }
}

/// `<https://...>` at `i`: the url and where it ends.
fn angle_link(s: &str, i: usize) -> Option<(&str, usize)> {
    let rest = &s[i + 1..];
    let end = rest.find(['>', '<', ' ', '\n', '\t'])?;
    let url = &rest[..end];
    let is_url = url.starts_with("https://") || url.starts_with("http://");
    (rest.as_bytes()[end] == b'>' && is_url && url.len() > 8).then_some((url, i + 1 + end + 1))
}

fn at_word_start(s: &str, i: usize) -> bool {
    match s[..i].chars().next_back() {
        None => true,
        Some(c) => c.is_whitespace() || "([{<\"'*_~".contains(c),
    }
}

/// A bare address at `i`, where it ends. Trailing punctuation and an
/// unbalanced closing parenthesis belong to the sentence, as GitHub reads it.
fn url_at(s: &str, i: usize) -> Option<usize> {
    let rest = &s[i..];
    let scheme = if rest.starts_with("https://") {
        8
    } else if rest.starts_with("http://") {
        7
    } else {
        return None;
    };
    let mut end = rest
        .find(|c: char| c.is_whitespace() || c == '<' || c == '`')
        .unwrap_or(rest.len());
    // Counted once and kept as the end moves, so a url followed by a long
    // run of `)` is not recounted for each one.
    let url = rest[..end].as_bytes();
    let opens = url.iter().filter(|&&c| c == b'(').count();
    let mut shuts = url.iter().filter(|&&c| c == b')').count();
    while end > scheme {
        let last = url[end - 1];
        let trailing = b"?!.,:;*_~'\"".contains(&last);
        let unbalanced = last == b')' && shuts > opens;
        if !(trailing || unbalanced) {
            break;
        }
        if last == b')' {
            shuts -= 1;
        }
        end -= 1;
    }
    (end > scheme).then_some(i + end)
}

fn path_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b'/' | b'\\')
}

/// A file path in running text at `i`: where it ends, the path and its
/// line. When there is none, where the run of path characters it would
/// have been read from ends.
fn path_at(s: &str, i: usize) -> Result<(usize, String, Option<u32>), usize> {
    let b = s.as_bytes();
    if !(b[i].is_ascii_alphanumeric() || matches!(b[i], b'.' | b'/' | b'\\')) {
        return Err(i);
    }
    let run = i + b[i..]
        .iter()
        .take_while(|&&c| path_char(c) || c == b':')
        .count();
    let mut end = run;
    while end > i && !b[end - 1].is_ascii_alphanumeric() {
        end -= 1;
    }
    let (path, line) = split_path(&s[i..end], true).ok_or(run)?;
    Ok((end, path, line))
}

/// Reads `dir/name.ext`, `name.ext:120` or `name.ext:120:5` (the column is
/// dropped). In running text a path needs a directory or a line number, so
/// "Node.js" stays a word; in inline code a lone name with a known
/// extension is enough.
fn split_path(s: &str, in_text: bool) -> Option<(String, Option<u32>)> {
    let mut p = s;
    let mut line = None;
    for _ in 0..2 {
        match p.rsplit_once(':') {
            Some((head, n))
                if !n.is_empty() && n.len() <= 9 && n.bytes().all(|c| c.is_ascii_digit()) =>
            {
                line = n.parse().ok();
                p = head;
            }
            _ => break,
        }
    }
    let pb = p.as_bytes();
    // `\\host\share` and `//host` reach out to another machine when opened,
    // which a reply has no business making the UI do.
    if pb.len() >= 2 && matches!(pb[0], b'/' | b'\\') && matches!(pb[1], b'/' | b'\\') {
        return None;
    }
    let drive = pb.len() > 2
        && pb[0].is_ascii_alphabetic()
        && pb[1] == b':'
        && matches!(pb[2], b'/' | b'\\');
    let rest = if drive { &p[2..] } else { p };
    if rest.is_empty() || !rest.bytes().all(path_char) {
        return None;
    }
    let first_sep = rest.find(['/', '\\']);
    if let Some(e) = first_sep {
        // "github.com/x.html" is an address, not a file here.
        if rest[..e].find('.').is_some_and(|d| d > 0) {
            return None;
        }
    }
    let name = rest.rsplit(['/', '\\']).next()?;
    let (stem, ext) = name.rsplit_once('.')?;
    let ext_ok = !ext.is_empty()
        && ext.len() <= 10
        && ext.as_bytes()[0].is_ascii_alphabetic()
        && ext.bytes().all(|c| c.is_ascii_alphanumeric());
    if !ext_ok || (stem.is_empty() && first_sep.is_none()) {
        return None;
    }
    let known = KNOWN_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str());
    let ok = first_sep.is_some() || line.is_some() || (!in_text && known);
    ok.then(|| (p.to_string(), line))
}

/// CommonMark's emphasis matching over one run of text: each closer takes
/// the nearest opener of its kind, delimiters left between them turn
/// literal, and whatever never matches stays as typed. Returns
/// (opener, closer, style) by index into `delims`.
fn match_emphasis(delims: &mut [Delim]) -> Vec<(usize, usize, Style)> {
    let mut found = Vec::new();
    for ci in 0..delims.len() {
        loop {
            let d = &delims[ci];
            if !d.active || !d.close || d.len == 0 {
                break;
            }
            let (ch, orig, can_open) = (d.ch, d.orig, d.open);
            let opener = (0..ci).rev().find(|&oi| {
                let o = &delims[oi];
                if !o.active || !o.open || o.len == 0 || o.ch != ch {
                    return false;
                }
                if ch == b'~' {
                    return o.orig == orig;
                }
                // CommonMark's rule of three, so "*a**b*" is not misread.
                let either = o.close || can_open;
                let sum_of_three = (o.orig + orig).is_multiple_of(3);
                !(either && sum_of_three && !(o.orig.is_multiple_of(3) && orig.is_multiple_of(3)))
            });
            let Some(oi) = opener else {
                if !can_open {
                    delims[ci].active = false;
                }
                break;
            };
            let (used, style) = match ch {
                b'~' => (2, Style::Strike),
                _ if delims[oi].len >= 2 && delims[ci].len >= 2 => (2, Style::Bold),
                _ => (1, Style::Italic),
            };
            delims[oi].len -= used;
            delims[ci].len -= used;
            for between in &mut delims[oi + 1..ci] {
                between.active = false;
            }
            found.push((oi, ci, style));
        }
    }
    found
}

fn emit(mut t: Toks, out: &mut Inline) {
    let pairs = match_emphasis(&mut t.delims);
    let mut opens = vec![Vec::new(); t.delims.len()];
    let mut closes = vec![Vec::new(); t.delims.len()];
    for (p, (o, c, _)) in pairs.iter().enumerate() {
        opens[*o].push(p);
        closes[*c].push(p);
    }
    let mut starts = vec![0; pairs.len()];
    // HTML styles open and where, innermost last.
    let mut tags: Vec<(Style, usize)> = Vec::new();
    for tok in t.toks {
        let start = out.text.len();
        match tok {
            Tok::Text(s) => out.text.push_str(&s),
            Tok::Delim(k) => {
                // What a delimiter did not use stays literal on its outer side.
                for &p in &closes[k] {
                    out.span(starts[p], pairs[p].2.clone());
                }
                let d = &t.delims[k];
                out.text.extend(std::iter::repeat_n(d.ch as char, d.len));
                for &p in &opens[k] {
                    starts[p] = out.text.len();
                }
            }
            Tok::Code(s) => {
                out.text.push_str(&s);
                if let Some(keys) = key_combo(&s) {
                    for k in keys {
                        out.spans.push(Span {
                            range: start + k.start..start + k.end,
                            style: Style::Key,
                        });
                    }
                    continue;
                }
                out.span(start, Style::Code);
                if !s.contains(char::is_whitespace) {
                    if let Some((path, line)) = split_path(&s, false) {
                        out.span(start, Style::Path { path, line });
                    }
                }
            }
            Tok::Link(inner, url) => {
                emit(*inner, out);
                if out.text.len() == start {
                    out.text.push_str(&url);
                }
                if is_web_link(&url) {
                    out.span(start, Style::Link(url));
                }
            }
            Tok::Url(url) => {
                out.text.push_str(&url);
                out.span(start, Style::Link(url));
            }
            Tok::Path(text, path, line) => {
                out.text.push_str(&text);
                out.span(start, Style::Path { path, line });
            }
            Tok::Key(name) => {
                out.text.push_str(&name);
                out.span(start, Style::Key);
            }
            Tok::Image(inner, src) => {
                emit(*inner, out);
                if out.text.len() == start {
                    out.text.push_str("image");
                }
                out.span(start, Style::Image(src));
            }
            Tok::Footnote(label) => {
                let shown = note_number(&label).map_or(label, |n| n.to_string());
                out.text.push_str(&shown);
                out.span(start, Style::Footnote(shown));
            }
            Tok::Open(style) => tags.push((style, start)),
            Tok::Close(style) => {
                let kind = std::mem::discriminant(&style);
                if let Some(k) = tags
                    .iter()
                    .rposition(|(s, _)| std::mem::discriminant(s) == kind)
                {
                    let (style, from) = tags.remove(k);
                    out.span(from, style);
                }
            }
        }
    }
    // A tag not closed yet, as while a reply streams, styles to the end.
    for (style, from) in tags {
        out.span(from, style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(src: &str) -> Inline {
        match parse(src).as_slice() {
            [Block::Paragraph(p)] => p.clone(),
            other => panic!("not one paragraph: {other:?}"),
        }
    }

    /// The text under each span of the given style.
    fn under<'a>(inl: &'a Inline, want: &Style) -> Vec<&'a str> {
        inl.spans
            .iter()
            .filter(|s| &s.style == want)
            .map(|s| &inl.text[s.range.clone()])
            .collect()
    }

    fn links(inl: &Inline) -> Vec<(&str, &str)> {
        inl.spans
            .iter()
            .filter_map(|s| match &s.style {
                Style::Link(u) => Some((&inl.text[s.range.clone()], u.as_str())),
                _ => None,
            })
            .collect()
    }

    fn paths(inl: &Inline) -> Vec<(&str, String, Option<u32>)> {
        inl.spans
            .iter()
            .filter_map(|s| match &s.style {
                Style::Path { path, line } => {
                    Some((&inl.text[s.range.clone()], path.clone(), *line))
                }
                _ => None,
            })
            .collect()
    }

    fn plain(text: &str) -> Inline {
        Inline {
            text: text.to_string(),
            spans: Vec::new(),
        }
    }

    fn check_inline(inl: &Inline) {
        for s in &inl.spans {
            assert!(s.range.start < s.range.end, "{inl:?}");
            assert!(s.range.end <= inl.text.len(), "{inl:?}");
            assert!(inl.text.is_char_boundary(s.range.start), "{inl:?}");
            assert!(inl.text.is_char_boundary(s.range.end), "{inl:?}");
        }
        assert_eq!(inl.utf16_spans().len(), inl.spans.len());
    }

    fn check(blocks: &[Block]) {
        for b in blocks {
            match b {
                Block::Paragraph(i) | Block::Heading { text: i, .. } => check_inline(i),
                Block::List { items, .. } => items.iter().for_each(|it| check(it)),
                Block::Quote(inner) => check(inner),
                Block::Callout { title, body, .. } => {
                    check_inline(title);
                    check(body);
                }
                Block::Table {
                    header,
                    align,
                    rows,
                } => {
                    assert_eq!(header.len(), align.len());
                    header.iter().for_each(check_inline);
                    for r in rows {
                        assert_eq!(r.len(), header.len());
                        r.iter().for_each(check_inline);
                    }
                }
                Block::Details { summary, body, .. } => {
                    check_inline(summary);
                    check(body);
                }
                Block::Footnotes(notes) => notes.iter().for_each(|(_, t)| check_inline(t)),
                Block::Code { .. } | Block::Rule => {}
            }
        }
    }

    // Paragraphs and breaks

    #[test]
    fn empty_input_has_no_blocks() {
        assert!(parse("").is_empty());
        assert!(parse("\n\n  \n").is_empty());
    }

    #[test]
    fn plain_paragraph() {
        assert_eq!(
            parse("hello world"),
            vec![Block::Paragraph(plain("hello world"))]
        );
    }

    #[test]
    fn blank_line_splits_paragraphs() {
        assert_eq!(
            parse("one\n\ntwo"),
            vec![
                Block::Paragraph(plain("one")),
                Block::Paragraph(plain("two"))
            ]
        );
    }

    #[test]
    fn newline_in_paragraph_stays() {
        assert_eq!(para("one\ntwo").text, "one\ntwo");
    }

    #[test]
    fn hard_break_by_spaces_drops_them() {
        assert_eq!(para("one  \ntwo").text, "one\ntwo");
    }

    #[test]
    fn hard_break_by_backslash() {
        assert_eq!(para("one\\\ntwo").text, "one\ntwo");
    }

    #[test]
    fn paragraph_lines_lose_their_indent() {
        assert_eq!(para("one\n   two").text, "one\ntwo");
    }

    #[test]
    fn trailing_spaces_at_end_are_dropped() {
        assert_eq!(para("end   ").text, "end");
    }

    // Headings

    #[test]
    fn heading_levels() {
        for level in 1..=6u8 {
            let src = format!("{} Title", "#".repeat(level as usize));
            assert_eq!(
                parse(&src),
                vec![Block::Heading {
                    level,
                    text: plain("Title")
                }]
            );
        }
    }

    #[test]
    fn seven_hashes_is_text() {
        assert_eq!(para("####### x").text, "####### x");
    }

    #[test]
    fn hashtag_is_text() {
        assert_eq!(para("#tag").text, "#tag");
    }

    #[test]
    fn heading_closing_hashes_go() {
        assert_eq!(
            parse("## Title ##"),
            vec![Block::Heading {
                level: 2,
                text: plain("Title")
            }]
        );
    }

    #[test]
    fn heading_keeps_hash_inside_word() {
        match &parse("# C#")[0] {
            Block::Heading { text, .. } => assert_eq!(text.text, "C#"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn heading_has_inline_styles() {
        match &parse("### The **bold** part")[0] {
            Block::Heading { level: 3, text } => {
                assert_eq!(text.text, "The bold part");
                assert_eq!(under(text, &Style::Bold), vec!["bold"]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn heading_interrupts_paragraph() {
        let b = parse("text\n# Head");
        assert!(matches!(b[1], Block::Heading { level: 1, .. }));
    }

    #[test]
    fn escaped_hash_is_not_heading() {
        assert_eq!(para("\\# not").text, "# not");
    }

    // Lists

    fn items(b: &Block) -> &Vec<Vec<Block>> {
        match b {
            Block::List { items, .. } => items,
            other => panic!("not a list: {other:?}"),
        }
    }

    fn item_text(item: &[Block]) -> &str {
        match item.first() {
            Some(Block::Paragraph(p)) => &p.text,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn bullet_markers() {
        for m in ["-", "*", "+"] {
            let src = format!("{m} a\n{m} b");
            let b = parse(&src);
            assert_eq!(b.len(), 1);
            let it = items(&b[0]);
            assert_eq!(it.len(), 2);
            assert_eq!(item_text(&it[0]), "a");
            assert_eq!(item_text(&it[1]), "b");
        }
    }

    #[test]
    fn ordered_list_keeps_start() {
        match &parse("3. c\n4. d")[0] {
            Block::List {
                ordered: true,
                start: 3,
                items,
                ..
            } => assert_eq!(items.len(), 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ordered_list_with_paren() {
        assert!(matches!(
            parse("1) a\n2) b")[0],
            Block::List {
                ordered: true,
                start: 1,
                ..
            }
        ));
    }

    #[test]
    fn nested_by_two_spaces() {
        let b = parse("- a\n  - b\n  - c\n- d");
        let it = items(&b[0]);
        assert_eq!(it.len(), 2);
        assert_eq!(item_text(&it[0]), "a");
        assert_eq!(items(&it[0][1]).len(), 2);
        assert_eq!(item_text(&it[1]), "d");
    }

    #[test]
    fn nested_by_four_spaces() {
        let b = parse("- a\n    - b\n        - c");
        let inner = items(&items(&b[0])[0][1]);
        assert_eq!(item_text(&inner[0]), "b");
        assert_eq!(item_text(&items(&inner[0][1])[0]), "c");
    }

    #[test]
    fn ordered_with_nested_bullets() {
        let b = parse("1. one\n   - x\n2. two");
        let it = items(&b[0]);
        assert_eq!(it.len(), 2);
        assert!(matches!(it[0][1], Block::List { ordered: false, .. }));
    }

    #[test]
    fn different_bullet_starts_new_list() {
        let b = parse("- a\n* b");
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn ordered_then_bullet_are_two_lists() {
        let b = parse("1. a\n- b");
        assert!(matches!(b[0], Block::List { ordered: true, .. }));
        assert!(matches!(b[1], Block::List { ordered: false, .. }));
    }

    #[test]
    fn lazy_continuation_line() {
        let b = parse("- a\nstill a");
        assert_eq!(item_text(&items(&b[0])[0]), "a\nstill a");
    }

    #[test]
    fn item_with_two_paragraphs() {
        let b = parse("- a\n\n  more\n- b");
        let it = items(&b[0]);
        assert_eq!(it.len(), 2);
        assert_eq!(it[0].len(), 2);
    }

    #[test]
    fn blank_between_items_keeps_one_list() {
        assert_eq!(items(&parse("- a\n\n- b")[0]).len(), 2);
    }

    #[test]
    fn unindented_text_after_blank_ends_list() {
        let b = parse("- a\n\nafter");
        assert_eq!(b.len(), 2);
        assert_eq!(b[1], Block::Paragraph(plain("after")));
    }

    #[test]
    fn list_interrupts_paragraph() {
        let b = parse("Steps:\n- one\n- two");
        assert_eq!(b[0], Block::Paragraph(plain("Steps:")));
        assert_eq!(items(&b[1]).len(), 2);
    }

    #[test]
    fn year_does_not_interrupt_paragraph() {
        assert_eq!(para("it was\n2024. a year").text, "it was\n2024. a year");
    }

    #[test]
    fn dash_without_space_is_text() {
        assert_eq!(para("-1 degrees").text, "-1 degrees");
    }

    #[test]
    fn empty_item() {
        let b = parse("-");
        assert_eq!(items(&b[0]), &vec![Vec::<Block>::new()]);
    }

    #[test]
    fn code_fence_inside_item() {
        let b = parse("- run:\n  ```sh\n  cargo test\n  ```\n- done");
        let it = items(&b[0]);
        assert_eq!(
            it[0][1],
            Block::Code {
                lang: "sh".into(),
                info: "sh".into(),
                text: "cargo test".into(),
                closed: true
            }
        );
        assert_eq!(item_text(&it[1]), "done");
    }

    #[test]
    fn list_item_has_inline_styles() {
        let b = parse("- **bold** and `code`");
        match &items(&b[0])[0][0] {
            Block::Paragraph(p) => {
                assert_eq!(under(p, &Style::Bold), vec!["bold"]);
                assert_eq!(under(p, &Style::Code), vec!["code"]);
            }
            other => panic!("{other:?}"),
        }
    }

    fn tasks(b: &Block) -> &Vec<Option<bool>> {
        match b {
            Block::List { tasks, .. } => tasks,
            other => panic!("not a list: {other:?}"),
        }
    }

    #[test]
    fn task_boxes_are_read_and_taken_off_the_text() {
        let b = parse("- [ ] todo\n- [x] done\n- plain\n- [X] loud");
        assert_eq!(tasks(&b[0]), &[Some(false), Some(true), None, Some(true)]);
        let it = items(&b[0]);
        assert_eq!(item_text(&it[0]), "todo");
        assert_eq!(item_text(&it[1]), "done");
        assert_eq!(item_text(&it[2]), "plain");
        let star = parse("* [x] **bold** task");
        assert_eq!(tasks(&star[0]), &[Some(true)]);
        let ordered = parse("1. [ ] first");
        assert_eq!(tasks(&ordered[0]), &[Some(false)]);
    }

    #[test]
    fn a_box_without_words_or_a_link_stays_text() {
        assert_eq!(tasks(&parse("- [ ]")[0]), &[None]);
        assert_eq!(item_text(&items(&parse("- [ ]")[0])[0]), "[ ]");
        assert_eq!(tasks(&parse("- [y] no")[0]), &[None]);
        assert_eq!(tasks(&parse("- [x]no")[0]), &[None]);
        assert_eq!(tasks(&parse("- [a](https://x.io) link")[0]), &[None]);
    }

    #[test]
    fn every_item_has_its_task_slot() {
        let b = parse("- a\n  - [ ] inner\n- b");
        assert_eq!(tasks(&b[0]).len(), items(&b[0]).len());
        assert_eq!(tasks(&items(&b[0])[0][1]), &[Some(false)]);
    }

    // Code blocks

    #[test]
    fn fenced_code_with_lang() {
        assert_eq!(
            parse("```rust\nfn main() {}\n```"),
            vec![Block::Code {
                lang: "rust".into(),
                info: "rust".into(),
                text: "fn main() {}".into(),
                closed: true
            }]
        );
    }

    #[test]
    fn tilde_fence() {
        assert!(matches!(
            &parse("~~~\nx\n~~~")[0],
            Block::Code { closed: true, .. }
        ));
    }

    #[test]
    fn fence_lang_is_first_word() {
        match &parse("```rust ignore\nx\n```")[0] {
            Block::Code { lang, .. } => assert_eq!(lang, "rust"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unclosed_fence_runs_to_end() {
        assert_eq!(
            parse("```py\nprint(1)\n\nprint(2)"),
            vec![Block::Code {
                lang: "py".into(),
                info: "py".into(),
                text: "print(1)\n\nprint(2)".into(),
                closed: false
            }]
        );
    }

    #[test]
    fn unclosed_fence_ignores_trailing_newline() {
        match &parse("```\nx\n")[0] {
            Block::Code { text, closed, .. } => {
                assert_eq!(text, "x");
                assert!(!closed);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn code_keeps_markdown_raw() {
        match &parse("```\n# not a heading\n**not bold**\n- no list\n```")[0] {
            Block::Code { text, .. } => {
                assert_eq!(text, "# not a heading\n**not bold**\n- no list")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn shorter_fence_does_not_close() {
        match &parse("````\n```\ninner\n```\n````")[0] {
            Block::Code { text, closed, .. } => {
                assert_eq!(text, "```\ninner\n```");
                assert!(closed);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn indented_fence_strips_its_indent() {
        match &parse("  ```\n    a\n  b\n  ```")[0] {
            Block::Code { text, .. } => assert_eq!(text, "  a\nb"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn one_line_triple_backticks_are_inline_code() {
        let p = para("```inline```");
        assert_eq!(under(&p, &Style::Code), vec!["inline"]);
    }

    #[test]
    fn fence_interrupts_paragraph() {
        let b = parse("look:\n```\nx\n```\nafter");
        assert_eq!(b.len(), 3);
        assert!(matches!(b[1], Block::Code { .. }));
    }

    // Inline code

    #[test]
    fn inline_code() {
        let p = para("run `cargo test` now");
        assert_eq!(p.text, "run cargo test now");
        assert_eq!(under(&p, &Style::Code), vec!["cargo test"]);
    }

    #[test]
    fn double_backticks_hold_a_backtick() {
        let p = para("``a ` b``");
        assert_eq!(under(&p, &Style::Code), vec!["a ` b"]);
    }

    #[test]
    fn code_strips_one_space_each_side() {
        let p = para("`` `x` ``");
        assert_eq!(under(&p, &Style::Code), vec!["`x`"]);
    }

    #[test]
    fn unclosed_backtick_is_literal() {
        let p = para("a `b c");
        assert_eq!(p.text, "a `b c");
        assert!(p.spans.is_empty());
    }

    #[test]
    fn markdown_inside_inline_code_is_raw() {
        let p = para("`**x** [a](b) \\*`");
        assert_eq!(p.text, "**x** [a](b) \\*");
        assert_eq!(p.spans.len(), 1);
    }

    #[test]
    fn inline_code_over_a_newline_is_a_space() {
        assert_eq!(under(&para("`a\nb`"), &Style::Code), vec!["a b"]);
    }

    // Emphasis

    #[test]
    fn bold() {
        let p = para("a **b** c");
        assert_eq!(p.text, "a b c");
        assert_eq!(under(&p, &Style::Bold), vec!["b"]);
    }

    #[test]
    fn bold_underscores() {
        assert_eq!(under(&para("__b__"), &Style::Bold), vec!["b"]);
    }

    #[test]
    fn italic_star_and_underscore() {
        assert_eq!(under(&para("*i*"), &Style::Italic), vec!["i"]);
        assert_eq!(under(&para("_i_"), &Style::Italic), vec!["i"]);
    }

    #[test]
    fn bold_italic() {
        let p = para("***both***");
        assert_eq!(p.text, "both");
        assert_eq!(under(&p, &Style::Bold), vec!["both"]);
        assert_eq!(under(&p, &Style::Italic), vec!["both"]);
    }

    #[test]
    fn italic_inside_bold() {
        let p = para("**bold *it* bold**");
        assert_eq!(p.text, "bold it bold");
        assert_eq!(under(&p, &Style::Bold), vec!["bold it bold"]);
        assert_eq!(under(&p, &Style::Italic), vec!["it"]);
    }

    #[test]
    fn bold_inside_italic() {
        let p = para("*it **b** it*");
        assert_eq!(p.text, "it b it");
        assert_eq!(under(&p, &Style::Italic), vec!["it b it"]);
        assert_eq!(under(&p, &Style::Bold), vec!["b"]);
    }

    #[test]
    fn bold_then_italic_end_together() {
        let p = para("***a** b*");
        assert_eq!(p.text, "a b");
        assert_eq!(under(&p, &Style::Bold), vec!["a"]);
        assert_eq!(under(&p, &Style::Italic), vec!["a b"]);
    }

    #[test]
    fn snake_case_stays() {
        let p = para("call snake_case_name here");
        assert_eq!(p.text, "call snake_case_name here");
        assert!(p.spans.is_empty());
    }

    #[test]
    fn intraword_star_emphasis() {
        assert_eq!(under(&para("a*b*c"), &Style::Italic), vec!["b"]);
    }

    #[test]
    fn unclosed_bold_is_literal() {
        let p = para("this is **bold");
        assert_eq!(p.text, "this is **bold");
        assert!(p.spans.is_empty());
    }

    #[test]
    fn spaced_stars_are_literal() {
        let p = para("2 * 3 * 4");
        assert_eq!(p.text, "2 * 3 * 4");
        assert!(p.spans.is_empty());
    }

    #[test]
    fn lone_closer_is_literal() {
        assert_eq!(para("a** b").text, "a** b");
    }

    #[test]
    fn extra_star_stays_outside() {
        let p = para("***a**");
        assert_eq!(p.text, "*a");
        assert_eq!(under(&p, &Style::Bold), vec!["a"]);
    }

    #[test]
    fn strikethrough() {
        let p = para("~~gone~~ kept");
        assert_eq!(p.text, "gone kept");
        assert_eq!(under(&p, &Style::Strike), vec!["gone"]);
    }

    #[test]
    fn single_tilde_is_literal() {
        let p = para("~5 minutes or ~10~");
        assert_eq!(p.text, "~5 minutes or ~10~");
        assert!(p.spans.is_empty());
    }

    #[test]
    fn emphasis_does_not_cross_code() {
        let p = para("*a `b*` c");
        assert_eq!(p.text, "*a b* c");
        assert_eq!(under(&p, &Style::Code), vec!["b*"]);
        assert!(under(&p, &Style::Italic).is_empty());
    }

    // Links

    #[test]
    fn link() {
        let p = para("see [the docs](https://x.dev/a) now");
        assert_eq!(p.text, "see the docs now");
        assert_eq!(links(&p), vec![("the docs", "https://x.dev/a")]);
    }

    #[test]
    fn link_with_title() {
        let p = para("[t](https://x.dev \"Title\")");
        assert_eq!(links(&p), vec![("t", "https://x.dev")]);
    }

    #[test]
    fn link_url_with_parens() {
        let p = para("[w](https://en.wikipedia.org/wiki/Rust_(language))");
        assert_eq!(
            links(&p),
            vec![("w", "https://en.wikipedia.org/wiki/Rust_(language)")]
        );
    }

    #[test]
    fn link_angle_destination() {
        assert_eq!(
            links(&para("[a](<https://x.dev/a?b>)")),
            vec![("a", "https://x.dev/a?b")]
        );
    }

    #[test]
    fn link_with_bold_text() {
        let p = para("[**big** link](https://u.dev)");
        assert_eq!(p.text, "big link");
        assert_eq!(links(&p), vec![("big link", "https://u.dev")]);
        assert_eq!(under(&p, &Style::Bold), vec!["big"]);
    }

    #[test]
    fn link_with_brackets_in_text() {
        assert_eq!(
            links(&para("[a [b] c](https://u.dev)")),
            vec![("a [b] c", "https://u.dev")]
        );
    }

    #[test]
    fn empty_link_text_shows_url() {
        assert_eq!(
            links(&para("[](https://x.dev)")),
            vec![("https://x.dev", "https://x.dev")]
        );
    }

    #[test]
    fn image_is_its_alt_text_marked_with_its_source() {
        let p = para("see ![logo](https://x.dev/a.png) and ![](c:/x.png)");
        assert_eq!(p.text, "see logo and image");
        assert_eq!(
            under(&p, &Style::Image("https://x.dev/a.png".into())),
            ["logo"]
        );
        assert_eq!(under(&p, &Style::Image("c:/x.png".into())), ["image"]);
        assert!(links(&p).is_empty());
    }

    #[test]
    fn unclosed_link_is_literal() {
        let p = para("[text](https://x.d");
        assert_eq!(p.text, "[text](https://x.d");
        assert!(links(&p).iter().all(|(t, _)| *t != "text"));
    }

    #[test]
    fn brackets_without_url_are_literal() {
        let p = para("[not a link] and [ref][1]");
        assert_eq!(p.text, "[not a link] and [ref][1]");
        assert!(p.spans.is_empty());
    }

    #[test]
    fn emphasis_does_not_cross_link() {
        let p = para("*a [b* c](u)");
        assert!(under(&p, &Style::Italic).is_empty());
    }

    #[test]
    fn only_web_and_mail_links_link() {
        for target in [
            "C:\\x\\a.exe",
            "\\\\host\\share\\y",
            "ms-msdt:/id PCWDiagnostic",
            "ms-settings:",
            "file:///C:/x/a.exe",
            "javascript:alert(1)",
            "search-ms:query=x",
            "a.exe",
            "https:",
            "http://",
            "<https://a b>",
        ] {
            let p = para(&format!("[docs]({target})"));
            assert!(links(&p).is_empty(), "{target}");
        }
        let p = para("[x](C:\\x\\a.exe) after");
        assert_eq!(p.text, "x after");
        assert!(p.spans.is_empty());
        for target in ["https://a.dev", "HTTP://a.dev", "mailto:me@a.dev"] {
            assert_eq!(links(&para(&format!("[t]({target})")))[0].1, target);
        }
    }

    #[test]
    fn web_link_check() {
        assert!(is_web_link("https://a.dev/x"));
        assert!(is_web_link("Mailto:a@b.c"));
        for url in [
            "https://",
            "ms-msdt:x",
            "C:\\a.exe",
            "\\\\h\\s",
            "https://a.dev/\u{0}",
            "https://a b",
            " https://a.dev",
            "xhttps://a.dev",
        ] {
            assert!(!is_web_link(url), "{url}");
        }
    }

    // Autolinks

    #[test]
    fn bare_url() {
        let p = para("go to https://horadric.dev/x?y=1 now");
        assert_eq!(
            links(&p),
            vec![("https://horadric.dev/x?y=1", "https://horadric.dev/x?y=1")]
        );
    }

    #[test]
    fn bare_url_drops_trailing_period() {
        let p = para("See http://a.dev.");
        assert_eq!(links(&p), vec![("http://a.dev", "http://a.dev")]);
        assert_eq!(p.text, "See http://a.dev.");
    }

    #[test]
    fn bare_url_in_parens() {
        let p = para("(see https://a.dev/x_(y))");
        assert_eq!(links(&p)[0].1, "https://a.dev/x_(y)");
    }

    #[test]
    fn bare_url_with_underscores_is_whole() {
        let p = para("https://a.dev/a_b_c");
        assert_eq!(links(&p)[0].1, "https://a.dev/a_b_c");
        assert!(under(&p, &Style::Italic).is_empty());
    }

    #[test]
    fn bare_url_inside_bold() {
        let p = para("**https://a.dev**");
        assert_eq!(links(&p)[0].1, "https://a.dev");
        assert_eq!(under(&p, &Style::Bold), vec!["https://a.dev"]);
    }

    #[test]
    fn angle_autolink() {
        let p = para("<https://a.dev/x>");
        assert_eq!(p.text, "https://a.dev/x");
        assert_eq!(links(&p)[0].1, "https://a.dev/x");
    }

    #[test]
    fn url_in_code_is_not_a_link() {
        assert!(links(&para("`https://a.dev`")).is_empty());
    }

    #[test]
    fn scheme_alone_is_not_a_link() {
        assert!(links(&para("https:// is a scheme")).is_empty());
    }

    // Paths

    #[test]
    fn path_with_line_in_text() {
        let p = para("look at crates/x/src/y.rs:120 first");
        assert_eq!(
            paths(&p),
            vec![(
                "crates/x/src/y.rs:120",
                "crates/x/src/y.rs".to_string(),
                Some(120)
            )]
        );
    }

    #[test]
    fn path_in_code() {
        let p = para("`crates/x/src/y.rs:120`");
        assert_eq!(
            paths(&p),
            vec![(
                "crates/x/src/y.rs:120",
                "crates/x/src/y.rs".to_string(),
                Some(120)
            )]
        );
        assert_eq!(under(&p, &Style::Code), vec!["crates/x/src/y.rs:120"]);
    }

    #[test]
    fn lone_known_file_in_code() {
        assert_eq!(paths(&para("`Cargo.toml`"))[0].1, "Cargo.toml");
    }

    #[test]
    fn lone_file_in_text_needs_more() {
        assert!(paths(&para("README.md and Node.js")).is_empty());
    }

    #[test]
    fn lone_file_with_line_in_text() {
        assert_eq!(
            paths(&para("main.rs:7")),
            vec![("main.rs:7", "main.rs".to_string(), Some(7))]
        );
    }

    #[test]
    fn path_column_is_dropped() {
        assert_eq!(paths(&para("src/a.rs:12:5"))[0].2, Some(12));
    }

    #[test]
    fn path_without_line() {
        assert_eq!(paths(&para("edit docs/plan/headless.md"))[0].2, None);
    }

    #[test]
    fn windows_path() {
        let p = para("C:\\Users\\me\\a.rs:3");
        assert_eq!(
            paths(&p),
            vec![(
                "C:\\Users\\me\\a.rs:3",
                "C:\\Users\\me\\a.rs".to_string(),
                Some(3)
            )]
        );
    }

    #[test]
    fn path_drops_trailing_punctuation() {
        let p = para("see src/lib.rs.");
        assert_eq!(paths(&p)[0].0, "src/lib.rs");
        assert_eq!(p.text, "see src/lib.rs.");
    }

    #[test]
    fn path_in_parens_and_bold() {
        assert_eq!(paths(&para("(src/a.rs)"))[0].0, "src/a.rs");
        let p = para("**src/a.rs**");
        assert_eq!(paths(&p)[0].0, "src/a.rs");
        assert_eq!(under(&p, &Style::Bold), vec!["src/a.rs"]);
    }

    #[test]
    fn path_with_underscores_is_whole() {
        let p = para("crates/my_mod/a_b.rs");
        assert_eq!(paths(&p)[0].0, "crates/my_mod/a_b.rs");
        assert!(under(&p, &Style::Italic).is_empty());
    }

    #[test]
    fn not_paths() {
        for text in [
            "and/or",
            "github.com/a/b.html",
            "1/2.5",
            "docs/plan",
            "`foo.bar()`",
            "`std::fs::read`",
            "`self.x`",
        ] {
            assert!(paths(&para(text)).is_empty(), "{text}");
        }
    }

    #[test]
    fn share_paths_are_not_paths() {
        for text in [
            "`\\\\host\\share\\a.rs`",
            "`//host/share/a.rs`",
            "`\\/host/a.rs`",
            "`\\\\?\\C:\\a.rs`",
            "see //host/share/a.rs:3 now",
            "see \\\\\\\\host\\\\a.rs:3 now",
        ] {
            assert!(paths(&para(text)).is_empty(), "{text}");
        }
        assert_eq!(paths(&para("`/etc/a.rs`"))[0].1, "/etc/a.rs");
    }

    // Quotes

    #[test]
    fn quote() {
        assert_eq!(
            parse("> hi\n> there"),
            vec![Block::Quote(vec![Block::Paragraph(plain("hi\nthere"))])]
        );
    }

    #[test]
    fn nested_quote() {
        assert_eq!(
            parse("> a\n>> b"),
            vec![Block::Quote(vec![
                Block::Paragraph(plain("a")),
                Block::Quote(vec![Block::Paragraph(plain("b"))])
            ])]
        );
    }

    #[test]
    fn quote_holds_list_and_code() {
        match &parse("> - x\n> ```\n> c\n> ```")[0] {
            Block::Quote(inner) => {
                assert_eq!(items(&inner[0]).len(), 1);
                assert!(matches!(inner[1], Block::Code { .. }));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn quote_lazy_line() {
        assert_eq!(
            parse("> a\nb"),
            vec![Block::Quote(vec![Block::Paragraph(plain("a\nb"))])]
        );
    }

    #[test]
    fn blank_line_ends_quote() {
        let b = parse("> a\n\nb");
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn bare_gt_is_empty_quote() {
        assert_eq!(parse(">"), vec![Block::Quote(vec![])]);
    }

    // Callouts

    #[test]
    fn github_alerts_are_callouts() {
        for (name, kind) in [
            ("NOTE", Callout::Note),
            ("TIP", Callout::Tip),
            ("IMPORTANT", Callout::Important),
            ("WARNING", Callout::Warning),
            ("caution", Callout::Caution),
        ] {
            let b = parse(&format!("> [!{name}]\n> Mind the **gap**.\n>\n> More."));
            check(&b);
            match &b[..] {
                [Block::Callout {
                    kind: k,
                    title,
                    body,
                }] => {
                    assert_eq!(*k, kind);
                    assert!(title.text.is_empty());
                    assert_eq!(body.len(), 2);
                    assert!(matches!(&body[0], Block::Paragraph(p) if p.text == "Mind the gap."));
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn an_alert_may_carry_a_title() {
        match &parse("> [!TIP] Faster builds\n> Use the cache.")[0] {
            Block::Callout { kind, title, body } => {
                assert_eq!(*kind, Callout::Tip);
                assert_eq!(title.text, "Faster builds");
                assert_eq!(body.len(), 1);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_alert_still_streaming_is_a_callout_with_no_body() {
        assert_eq!(
            parse("> [!WARNING]"),
            vec![Block::Callout {
                kind: Callout::Warning,
                title: Inline::default(),
                body: vec![]
            }]
        );
    }

    #[test]
    fn a_bold_lead_word_makes_a_callout() {
        for src in ["> **Note:** keep it short", "> **Note**: keep it short"] {
            match &parse(src)[0] {
                Block::Callout { kind, body, .. } => {
                    assert_eq!(*kind, Callout::Note);
                    assert_eq!(body, &vec![Block::Paragraph(plain("keep it short"))]);
                }
                other => panic!("{src}: {other:?}"),
            }
        }
        match &parse("> **Warning:**\n> on its own line")[0] {
            Block::Callout { kind, body, .. } => {
                assert_eq!(*kind, Callout::Warning);
                assert_eq!(body, &vec![Block::Paragraph(plain("on its own line"))]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn other_quotes_stay_quotes() {
        for src in [
            "> [!FOO]\n> x",
            "> **Notes:** x",
            "> **Note** x",
            "> **Note:**x",
            "> a\n> [!NOTE]",
            "> [!NOTE",
            "**Note:** not quoted",
        ] {
            let b = parse(src);
            assert!(
                !b.iter().any(|x| matches!(x, Block::Callout { .. })),
                "{src}: {b:?}"
            );
        }
    }

    #[test]
    fn a_callout_inside_a_list_item() {
        let b = parse("- step\n  > [!NOTE]\n  > careful");
        assert!(matches!(items(&b[0])[0][1], Block::Callout { .. }));
    }

    // Keys

    #[test]
    fn kbd_tags_are_keys() {
        let p = para("Press <kbd>Ctrl</kbd>+<kbd>K</kbd> now");
        assert_eq!(p.text, "Press Ctrl+K now");
        assert_eq!(under(&p, &Style::Key), vec!["Ctrl", "K"]);
        assert_eq!(under(&para("<KBD> Esc </KBD>"), &Style::Key), vec!["Esc"]);
    }

    #[test]
    fn a_stray_kbd_stays_text() {
        // Not a key, and as HTML its tags are taken off, never shown.
        for (src, text) in [
            ("<kbd>", None),
            ("<kbd></kbd>", None),
            ("<kbd>a<b></kbd>", Some("a<b>")),
            ("<kbd>Ctrl", Some("Ctrl")),
        ] {
            match (parse(src).as_slice(), text) {
                ([], None) => {}
                ([Block::Paragraph(p)], Some(t)) => {
                    assert_eq!(p.text, t, "{src}");
                    assert!(under(p, &Style::Key).is_empty(), "{src}");
                    check_inline(p);
                }
                (other, _) => panic!("{src}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_key_combo_in_code_is_keys() {
        let p = para("Open it with `Ctrl+Shift+P` or `Alt+F4` or `cmd+k`.");
        assert_eq!(
            under(&p, &Style::Key),
            vec!["Ctrl", "Shift", "P", "Alt", "F4", "cmd", "k"]
        );
        assert!(under(&p, &Style::Code).is_empty());
        assert_eq!(p.text, "Open it with Ctrl+Shift+P or Alt+F4 or cmd+k.");
        assert_eq!(under(&para("`Ctrl++`"), &Style::Key), vec!["Ctrl", "+"]);
        assert_eq!(
            under(&para("`Shift+Enter`"), &Style::Key),
            vec!["Shift", "Enter"]
        );
        check_inline(&p);
    }

    #[test]
    fn code_that_only_looks_like_keys_stays_code() {
        for src in [
            "`a+b`",
            "`C++`",
            "`i+1`",
            "`Ctrl`",
            "`Ctrl+`",
            "`Ctrl+Foo`",
            "`Ctrl + K`",
            "`x+Ctrl+K`",
            "`Ctrl+F99`",
            "`+K`",
        ] {
            let p = para(src);
            assert!(under(&p, &Style::Key).is_empty(), "{src}");
            assert_eq!(under(&p, &Style::Code).len(), 1, "{src}");
        }
    }

    // Tables

    fn table_of(src: &str) -> (Vec<Inline>, Vec<Align>, Vec<Vec<Inline>>) {
        match parse(src).into_iter().next() {
            Some(Block::Table {
                header,
                align,
                rows,
            }) => (header, align, rows),
            other => panic!("{other:?}"),
        }
    }

    fn texts(cells: &[Inline]) -> Vec<&str> {
        cells.iter().map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn table() {
        let (h, a, r) = table_of("| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |");
        assert_eq!(texts(&h), vec!["a", "b"]);
        assert_eq!(a, vec![Align::None, Align::None]);
        assert_eq!(r.len(), 2);
        assert_eq!(texts(&r[1]), vec!["3", "4"]);
    }

    #[test]
    fn table_alignment() {
        let (_, a, _) = table_of("|a|b|c|d|\n|:--|:-:|--:|-|");
        assert_eq!(
            a,
            vec![Align::Left, Align::Center, Align::Right, Align::None]
        );
    }

    #[test]
    fn table_without_outer_pipes() {
        let (h, _, r) = table_of("a | b\n--|--\n1 | 2");
        assert_eq!(texts(&h), vec!["a", "b"]);
        assert_eq!(texts(&r[0]), vec!["1", "2"]);
    }

    #[test]
    fn table_cell_with_code_holding_pipe() {
        let (_, _, r) = table_of("| op | x |\n|---|---|\n| `a | b` | **y** |");
        assert_eq!(texts(&r[0]), vec!["a | b", "y"]);
        assert_eq!(under(&r[0][0], &Style::Code), vec!["a | b"]);
        assert_eq!(under(&r[0][1], &Style::Bold), vec!["y"]);
    }

    #[test]
    fn table_escaped_pipe() {
        let (_, _, r) = table_of("| a |\n|---|\n| x \\| y |");
        assert_eq!(texts(&r[0]), vec!["x | y"]);
    }

    #[test]
    fn table_rows_padded_and_cut() {
        let (_, _, r) = table_of("| a | b |\n|---|---|\n| 1 |\n| 1 | 2 | 3 |");
        assert_eq!(texts(&r[0]), vec!["1", ""]);
        assert_eq!(texts(&r[1]), vec!["1", "2"]);
    }

    #[test]
    fn table_with_header_only() {
        let (h, _, r) = table_of("| a | b |\n|---|---|");
        assert_eq!(h.len(), 2);
        assert!(r.is_empty());
    }

    #[test]
    fn table_needs_matching_delimiter() {
        assert!(matches!(parse("| a | b |\n|---|")[0], Block::Paragraph(_)));
    }

    #[test]
    fn table_ends_at_blank_line() {
        let b = parse("|a|\n|-|\n|1|\n\nafter");
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn table_interrupts_paragraph() {
        let b = parse("Here:\n| a |\n|---|\n| 1 |");
        assert_eq!(b[0], Block::Paragraph(plain("Here:")));
        assert!(matches!(b[1], Block::Table { .. }));
    }

    #[test]
    fn table_cell_path() {
        let (_, _, r) = table_of("| file |\n|---|\n| `src/a.rs:4` |");
        assert_eq!(paths(&r[0][0])[0].2, Some(4));
    }

    // Rules

    #[test]
    fn rules() {
        for src in ["---", "***", "___", "- - -", " * * * ", "-----"] {
            assert_eq!(parse(src), vec![Block::Rule], "{src}");
        }
    }

    #[test]
    fn two_dashes_are_text() {
        assert_eq!(para("--").text, "--");
    }

    #[test]
    fn rule_after_blank_line() {
        assert_eq!(
            parse("text\n\n---"),
            vec![Block::Paragraph(plain("text")), Block::Rule]
        );
    }

    /// Decided: replies use `---` as a separator, and a streaming one
    /// would flip the text above into a heading and back.
    #[test]
    fn dashes_under_a_paragraph_stay_a_rule() {
        assert_eq!(
            parse("text\n---"),
            vec![Block::Paragraph(plain("text")), Block::Rule]
        );
    }

    #[test]
    fn setext_headings() {
        assert_eq!(
            parse("Title\n=====\n\nSub **bold**\n---\nBody"),
            vec![
                Block::Heading {
                    level: 1,
                    text: plain("Title")
                },
                Block::Paragraph(para("Sub **bold**")),
                Block::Rule,
                Block::Paragraph(plain("Body")),
            ]
        );
        // Two lines of text under one underline are one heading.
        assert!(
            matches!(&parse("a\nb\n===")[..], [Block::Heading { level: 1, text }] if text.text == "a\nb")
        );
    }

    #[test]
    fn short_or_spaced_dashes_are_not_setext() {
        assert_eq!(para("text\n--").text, "text\n--");
        assert_eq!(
            parse("text\n- - -"),
            vec![Block::Paragraph(plain("text")), Block::Rule]
        );
        // A list item's rule is not its heading.
        assert!(matches!(parse("- item\n---")[1], Block::Rule));
    }

    #[test]
    fn mixed_rule_chars_are_not_a_rule() {
        assert!(!matches!(parse("-*-")[0], Block::Rule));
    }

    // Escapes and entities

    #[test]
    fn backslash_escapes() {
        let p = para("\\*not\\* \\_no\\_ \\[x\\](y) \\`c\\`");
        assert_eq!(p.text, "*not* _no_ [x](y) `c`");
        assert!(p.spans.is_empty());
    }

    #[test]
    fn backslash_before_letter_stays() {
        assert_eq!(para("a\\b C:\\x").text, "a\\b C:\\x");
    }

    #[test]
    fn double_backslash_is_one() {
        assert_eq!(para("a\\\\b").text, "a\\b");
    }

    #[test]
    fn entities_are_decoded() {
        let p = para("&amp; &lt;b&gt; &#42;x&#42; &#x1F600; &quot;&#39;&nbsp;.");
        assert_eq!(p.text, "& <b> *x* \u{1F600} \"'\u{a0}.");
        // What an entity stands for is never markup.
        assert!(p.spans.is_empty());
    }

    #[test]
    fn entities_stay_in_code_and_unknown_ones_stay() {
        assert_eq!(para("`&amp;`").text, "&amp;");
        assert_eq!(para("&nosuch; & a &amp").text, "&nosuch; & a &amp");
        assert_eq!(para("&#0; &#xZZ;").text, "\u{FFFD} &#xZZ;");
        assert_eq!(para("\\&amp;").text, "&amp;");
    }

    #[test]
    fn html_styles() {
        let p = para("<b>bold</b> <strong>s</strong> <i>it</i> <em>e</em> <s>x</s>");
        assert_eq!(p.text, "bold s it e x");
        assert_eq!(under(&p, &Style::Bold), ["bold", "s"]);
        assert_eq!(under(&p, &Style::Italic), ["it", "e"]);
        assert_eq!(under(&p, &Style::Strike), ["x"]);
        let p = para("H<sub>2</sub>O and x<sup>2</sup>");
        assert_eq!(p.text, "H2O and x2");
        assert_eq!(under(&p, &Style::Sub), ["2"]);
        assert_eq!(under(&p, &Style::Sup), ["2"]);
    }

    #[test]
    fn html_br_breaks_the_line() {
        assert_eq!(
            para("a line<br>break<br/>and <br />more").text,
            "a line\nbreak\nand\nmore"
        );
    }

    #[test]
    fn html_code_is_code_and_raw() {
        let p = para("run <code>a *b* &lt;c&gt;</code> now");
        assert_eq!(p.text, "run a *b* <c> now");
        assert_eq!(under(&p, &Style::Code), ["a *b* <c>"]);
    }

    #[test]
    fn html_link_and_image() {
        let p = para("<a href=\"https://x.dev\">site</a> and <a href=\"file:///c\">no</a>");
        assert_eq!(p.text, "site and no");
        assert_eq!(links(&p), [("site", "https://x.dev")]);
        let p = para("<img src=\"a.png\" alt=\"the logo\">");
        assert_eq!(p.text, "the logo");
        assert_eq!(under(&p, &Style::Image("a.png".into())), ["the logo"]);
    }

    #[test]
    fn unknown_html_is_stripped_but_generics_stay() {
        assert_eq!(
            para("<span class=\"x\">kept</span> <div>d</div>").text,
            "kept d"
        );
        assert_eq!(
            para("<custom-el data=\"1\">t</custom-el>").text,
            "t</custom-el>"
        );
        assert_eq!(
            para("Option<T> and Vec<u8> and a < b > c").text,
            "Option<T> and Vec<u8> and a < b > c"
        );
        assert_eq!(para("a <!-- hidden --> b").text, "a  b");
    }

    #[test]
    fn an_unclosed_tag_styles_to_the_end() {
        let p = para("so <strong>far");
        assert_eq!(p.text, "so far");
        assert_eq!(under(&p, &Style::Bold), ["far"]);
        assert_eq!(para("a </b> b").text, "a  b");
        // A one letter tag waits for its close, as it may be a generic.
        assert_eq!(para("so <b>far").text, "so <b>far");
    }

    #[test]
    fn capitalised_and_one_letter_generics_stay_text() {
        for src in [
            "Foo<S> keeps the rest",
            "a Bar<I> here",
            "<B>not bold",
            "<P>not a paragraph",
            "<A>, <U> and <T>",
            "a Vec<p> of points",
            "Box<a> then more",
        ] {
            let p = para(src);
            assert_eq!(p.text, src);
            assert!(p.spans.is_empty(), "{src}: {:?}", p.spans);
        }
        // Closed later in the paragraph, it is HTML.
        let p = para("a <b>bold</b> and <u>u</u>");
        assert_eq!(p.text, "a bold and u");
        assert_eq!(under(&p, &Style::Bold), ["bold"]);
    }

    #[test]
    fn an_unclosed_comment_stays_text() {
        assert_eq!(para("a <!-- not closed b").text, "a <!-- not closed b");
        assert_eq!(para("x <!-- y --> z <!-- w").text, "x  z <!-- w");
    }

    #[test]
    fn a_line_of_only_tags_leaves_no_paragraph() {
        assert_eq!(
            parse("<div align=\"center\">\n\ntext\n\n</div>"),
            vec![Block::Paragraph(plain("text"))]
        );
    }

    // Details

    #[test]
    fn details_fold_their_body_under_the_summary() {
        let src = "<details>\n<summary>A **folded** section</summary>\n\nInside it.\n\n- one\n\n</details>\n\nAfter.";
        let b = parse(src);
        match &b[..] {
            [Block::Details {
                summary,
                body,
                open,
            }, Block::Paragraph(after)] => {
                assert_eq!(summary.text, "A folded section");
                assert_eq!(under(summary, &Style::Bold), ["folded"]);
                assert!(!open);
                assert_eq!(body.len(), 2);
                assert!(matches!(&body[0], Block::Paragraph(p) if p.text == "Inside it."));
                assert!(matches!(&body[1], Block::List { .. }));
                assert_eq!(after.text, "After.");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn details_on_one_line_and_open() {
        match &parse("<details open><summary>S</summary>body text</details>")[..] {
            [Block::Details {
                summary,
                body,
                open: true,
            }] => {
                assert_eq!(summary.text, "S");
                assert!(matches!(&body[..], [Block::Paragraph(p)] if p.text == "body text"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn nested_details_close_in_order() {
        let src = "<details>\n<summary>Out</summary>\n<details>\n<summary>In</summary>\ninner\n</details>\nouter\n</details>\nafter";
        match &parse(src)[..] {
            [Block::Details { body, .. }, Block::Paragraph(p)] => {
                assert!(matches!(&body[0], Block::Details { summary, .. } if summary.text == "In"));
                assert!(matches!(&body[1], Block::Paragraph(p) if p.text == "outer"));
                assert_eq!(p.text, "after");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn details_still_streaming_run_to_the_end() {
        match &parse("<details>\n<summary>Wait")[..] {
            [Block::Details { summary, body, .. }] => {
                assert_eq!(summary.text, "Wait");
                assert!(body.is_empty());
            }
            other => panic!("{other:?}"),
        }
    }

    // Indented code and math

    #[test]
    fn indented_code_block() {
        assert_eq!(
            parse("Text.\n\n    let a = 1;\n\n    \tb\n\nAfter."),
            vec![
                Block::Paragraph(plain("Text.")),
                Block::Code {
                    lang: String::new(),
                    info: String::new(),
                    text: "let a = 1;\n\n\tb".into(),
                    closed: true
                },
                Block::Paragraph(plain("After.")),
            ]
        );
    }

    #[test]
    fn indented_lines_carry_on_a_paragraph() {
        assert_eq!(para("one\n    two").text, "one\ntwo");
    }

    #[test]
    fn indented_lines_in_a_list_are_not_code() {
        let b = parse("- item\n\n      - deep");
        assert!(matches!(&b[..], [Block::List { .. }]));
        let items = items(&b[0]);
        assert!(!items[0].iter().any(|b| matches!(b, Block::Code { .. })));
    }

    #[test]
    fn display_math_is_a_math_code_block() {
        let want = |text: &str, closed| Block::Code {
            lang: "math".into(),
            info: "math".into(),
            text: text.into(),
            closed,
        };
        assert_eq!(parse("$$\n\\sum_i i\n$$"), vec![want("\\sum_i i", true)]);
        assert_eq!(parse("$$ x^2 $$"), vec![want("x^2", true)]);
        assert_eq!(parse("$$\na"), vec![want("a", false)]);
        assert_eq!(parse("so:\n$$\na\n$$")[1], want("a", true));
        // Inline dollars are money as often as maths.
        assert_eq!(
            para("costs $5 or $E = mc^2$").text,
            "costs $5 or $E = mc^2$"
        );
    }

    #[test]
    fn fence_keeps_its_whole_info() {
        match &parse("```rust title=\"src/x.rs\"\nx\n```")[0] {
            Block::Code { lang, info, .. } => {
                assert_eq!(lang, "rust");
                assert_eq!(info, "rust title=\"src/x.rs\"");
            }
            other => panic!("{other:?}"),
        }
    }

    // References and footnotes

    #[test]
    fn reference_links() {
        let p = para_of(
            "See [the docs][docs], [Docs][] and [docs].\n\n[docs]: https://x.dev/d \"Title\"",
        );
        assert_eq!(p.text, "See the docs, Docs and docs.");
        assert_eq!(
            links(&p),
            [
                ("the docs", "https://x.dev/d"),
                ("Docs", "https://x.dev/d"),
                ("docs", "https://x.dev/d")
            ]
        );
    }

    #[test]
    fn undefined_references_stay_as_written() {
        assert_eq!(
            para("[a][nope] and [b] and [x]").text,
            "[a][nope] and [b] and [x]"
        );
    }

    #[test]
    fn definitions_in_code_stay() {
        let b = parse("```\n[a]: https://x.dev\n```");
        assert!(matches!(&b[..], [Block::Code { text, .. }] if text == "[a]: https://x.dev"));
    }

    #[test]
    fn reference_images() {
        let p = para_of("![logo][l]\n\n[l]: assets/logo.png");
        assert_eq!(under(&p, &Style::Image("assets/logo.png".into())), ["logo"]);
    }

    #[test]
    fn footnotes_are_marked_and_gathered_at_the_end() {
        let b = parse("A claim[^1] and[^note].\n\n[^1]: The **first**.\n[^note]: Second,\n  on two lines.\n\nMore.");
        match &b[..] {
            [Block::Paragraph(p), Block::Paragraph(more), Block::Footnotes(notes)] => {
                assert_eq!(p.text, "A claim1 and2.");
                assert_eq!(under(p, &Style::Footnote("1".into())), ["1"]);
                assert_eq!(under(p, &Style::Footnote("2".into())), ["2"]);
                assert_eq!(more.text, "More.");
                assert_eq!(notes.len(), 2);
                assert_eq!((notes[0].0.as_str(), notes[1].0.as_str()), ("1", "2"));
                assert_eq!(notes[0].1.text, "The first.");
                assert_eq!(notes[1].1.text, "Second,\non two lines.");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn footnotes_are_numbered_by_first_mark() {
        let src = "B[^second] then A[^first], B[^second] again.\n\n[^first]: F.\n[^unused]: U.\n[^second]: S.";
        match &parse(src)[..] {
            [Block::Paragraph(p), Block::Footnotes(notes)] => {
                assert_eq!(p.text, "B1 then A2, B1 again.");
                let got: Vec<_> = notes
                    .iter()
                    .map(|(n, t)| (n.as_str(), t.text.as_str()))
                    .collect();
                assert_eq!(got, [("1", "S."), ("2", "F."), ("3", "U.")]);
            }
            other => panic!("{other:?}"),
        }
        // Alone, as a heading's text is read, the label is all there is.
        assert_eq!(parse_inline("x[^note]").text, "xnote");
    }

    #[test]
    fn a_sentence_after_a_label_is_not_a_definition() {
        for src in [
            "[Note]: this is important",
            "[Note]: this",
            "[a]: https://x.dev trailing words",
        ] {
            assert_eq!(para(src).text, src);
        }
        let p = para_of("[a][x] [b][y]\n\n[x]: https://x.dev \"Title\"\n[y]: <https://y.dev> (t)");
        assert_eq!(links(&p), [("a", "https://x.dev"), ("b", "https://y.dev")]);
    }

    #[test]
    fn dollars_in_prose_open_no_math() {
        assert_eq!(
            para("cost $$5 vs\n$$10 more").text,
            "cost $$5 vs\n$$10 more"
        );
        assert_eq!(parse("$$10 more\nand on").len(), 1);
        assert!(matches!(&parse("$$10 more")[..], [Block::Paragraph(_)]));
    }

    #[test]
    fn spaced_carets_are_not_footnotes() {
        assert_eq!(para("[^ x] and [^]").text, "[^ x] and [^]");
    }

    fn para_of(src: &str) -> Inline {
        match parse(src).as_slice() {
            [Block::Paragraph(p), ..] => p.clone(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn showcase_edges_parse() {
        let src = include_str!("../tests/fixtures/folio_showcase.md");
        let b = parse(src);
        check(&b);
        let has = |f: &dyn Fn(&Block) -> bool| b.iter().any(f);
        assert!(has(
            &|b| matches!(b, Block::Heading { level: 1, text } if text.text == "Setext heading, level one")
        ));
        assert!(has(
            &|b| matches!(b, Block::Paragraph(p) if p.text == "Setext heading, level two")
        ));
        assert!(has(&|b| matches!(b, Block::Details { .. })));
        assert!(has(&|b| matches!(b, Block::Footnotes(n) if n.len() == 1)));
        assert!(has(
            &|b| matches!(b, Block::Code { lang, .. } if lang == "math")
        ));
        assert!(has(
            &|b| matches!(b, Block::Code { text, .. } if text.starts_with("An indented code block"))
        ));
        assert!(!src.is_empty() && !b.iter().any(|b| matches!(b, Block::Paragraph(p) if p.text.contains("&amp;") || p.text.contains("<b>"))));
    }

    const STRESS: &str = include_str!("../tests/fixtures/folio_stress.md");

    #[test]
    fn stress_fixture_parses() {
        let b = parse(STRESS);
        check(&b);
        let tables: Vec<usize> = (b.iter())
            .filter_map(|b| match b {
                Block::Table { header, .. } => Some(header.len()),
                _ => None,
            })
            .collect();
        assert_eq!(tables, vec![20, 3]);
        assert!(b.iter().any(
            |b| matches!(b, Block::Code { text, closed: true, .. } if text.lines().count() == 500)
        ));
        assert!(
            matches!(&b[b.len() - 2], Block::Code { lang, closed: false, .. } if lang == "python")
        );
        assert!(matches!(b.last(), Some(Block::Footnotes(n)) if n.len() == 40));
        // Code inside a list inside a quote, and again two levels further.
        let Some(Block::Quote(q)) = b.iter().find(|b| matches!(b, Block::Quote(_))) else {
            panic!("no quote")
        };
        let item = &items(&q[1])[0];
        assert!(matches!(&item[1], Block::Code { lang, closed: true, .. } if lang == "rust"));
        let Block::Quote(inner) = &items(&item[2])[0][1] else {
            panic!("{item:?}")
        };
        assert!(matches!(&items(&inner[1])[0][1], Block::Code { lang, .. } if lang == "sh"));
        let Block::Paragraph(refs) = &b[1] else {
            panic!("{:?}", b[1])
        };
        let lines: Vec<_> = paths(refs).into_iter().map(|(_, p, l)| (p, l)).collect();
        assert_eq!(
            lines,
            vec![
                (
                    "crates/horadric-core/src/markdown.rs".to_string(),
                    Some(812)
                ),
                ("src/main.rs".to_string(), Some(3)),
                (
                    "crates/horadric-ui/src/folio/rich.rs".to_string(),
                    Some(457)
                ),
                ("C:\\Users\\me\\repo\\build.rs".to_string(), Some(7)),
                ("main.rs".to_string(), Some(42)),
            ]
        );
    }

    #[test]
    fn stress_fixture_streams_stably() {
        // Every line end and the middle of every line: each char would
        // reparse the fixture thirty thousand times.
        let cuts = STRESS.match_indices('\n').flat_map(|(nl, _)| {
            let start = STRESS[..nl].rfind('\n').map_or(0, |p| p + 1);
            let mut mid = start + (nl - start) / 2;
            while !STRESS.is_char_boundary(mid) {
                mid += 1;
            }
            [mid, nl]
        });
        assert_streams_stably(STRESS, cuts);
    }

    // Line endings and text

    #[test]
    fn crlf_reads_like_lf() {
        let lf = "# T\n\n- a\n- b\n\n```rs\nx\n```\n\n| a |\n|---|\n| 1 |\n\none  \ntwo\n";
        assert_eq!(parse(&lf.replace('\n', "\r\n")), parse(lf));
    }

    #[test]
    fn utf16_ranges_count_surrogates() {
        let p = para("é😀 **b**");
        assert_eq!(p.text, "é😀 b");
        let spans = p.utf16_spans();
        assert_eq!(spans, vec![(4..5, &Style::Bold)]);
    }

    #[test]
    fn spans_sorted_outer_first() {
        let p = para("***x*** **y**");
        let starts: Vec<usize> = p.spans.iter().map(|s| s.range.start).collect();
        let mut sorted = starts.clone();
        sorted.sort();
        assert_eq!(starts, sorted);
    }

    // Streaming

    const SAMPLE: &str = "# Plan for the **Folio**\n\
        \n\
        Here is what changed in `crates/horadric-core/src/folio.rs:120` and why.\n\
        See https://horadric.dev/docs (the docs) and [the plan](docs/PLAN.md).\n\
        \n\
        1. First *step* with ~~old~~ new\n\
        2. Second step\n\
        \x20  - nested `code`\n\
        \x20  - nested **bold _and italic_**\n\
        3. Third\n\
        \n\
        > A quote with a [link](https://x.dev)\n\
        > and a second line\n\
        \n\
        ```rust\n\
        fn main() {\n\
        \x20   println!(\"**not bold**\");\n\
        }\n\
        ```\n\
        \n\
        | File | Lines | Note |\n\
        |:-----|------:|:----:|\n\
        | `src/a.rs` | 12 | `a | b` |\n\
        | src/b.rs:40 | 3 | **ok** |\n\
        \n\
        ---\n\
        \n\
        Done: \\*escaped\\*, &amp; snake_case, é😀 text.  \n\
        Last line.\n";

    #[test]
    fn sample_parses_fully() {
        let b = parse(SAMPLE);
        check(&b);
        assert!(matches!(b[0], Block::Heading { level: 1, .. }));
        assert!(b.iter().any(|x| matches!(x, Block::Table { .. })));
        assert!(b
            .iter()
            .any(|x| matches!(x, Block::Code { closed: true, .. })));
        assert!(b.iter().any(|x| matches!(x, Block::Quote(_))));
        assert!(b.iter().any(|x| matches!(x, Block::Rule)));
    }

    #[test]
    fn every_prefix_parses() {
        for (i, _) in SAMPLE.char_indices() {
            check(&parse(&SAMPLE[..i]));
        }
    }

    #[test]
    fn streaming_bold_shows_literal_until_closed() {
        assert_eq!(para("so **bo").text, "so **bo");
        assert_eq!(under(&para("so **bold**"), &Style::Bold), vec!["bold"]);
    }

    #[test]
    fn streaming_code_block_is_open() {
        assert_eq!(
            parse("text\n\n```rust\nfn"),
            vec![
                Block::Paragraph(plain("text")),
                Block::Code {
                    lang: "rust".into(),
                    info: "rust".into(),
                    text: "fn".into(),
                    closed: false
                }
            ]
        );
    }

    #[test]
    fn streaming_fence_marker_alone() {
        assert!(matches!(
            &parse("```")[0],
            Block::Code { closed: false, .. }
        ));
        assert_eq!(para("``").text, "``");
    }

    #[test]
    fn streaming_table_waits_for_delimiter() {
        assert!(matches!(parse("| a | b |\n|--")[0], Block::Paragraph(_)));
        assert!(matches!(parse("| a | b |\n|--|--")[0], Block::Table { .. }));
    }

    #[test]
    fn streaming_list_item() {
        let b = parse("- item\n  - ne");
        assert_eq!(item_text(&items(&items(&b[0])[0][1])[0]), "ne");
    }

    /// The block that holds the code while a reply streams in.
    fn code_of(b: &[Block]) -> (&str, bool) {
        match b.last() {
            Some(Block::Code { text, closed, .. }) => (text, *closed),
            other => panic!("no code block last: {other:?}"),
        }
    }

    #[test]
    fn streaming_fence_stays_code_on_every_prefix() {
        let src = "Intro.\n\n```rust\nlet a = 1;\n\nlet b = \"**not bold**\";\n``";
        let start = src.find("```").unwrap() + 3;
        for (i, _) in src.char_indices().filter(|&(i, _)| i >= start) {
            let b = parse(&src[..i]);
            assert!(matches!(b[0], Block::Paragraph(_)), "{i}: {b:?}");
            assert_eq!(b.len(), 2, "{i}: {b:?}");
            assert!(!code_of(&b).1, "{i}");
        }
    }

    #[test]
    fn streaming_closing_fence_half_typed_is_not_shown_as_code() {
        assert_eq!(code_of(&parse("```\nx\n`")), ("x", false));
        assert_eq!(code_of(&parse("```\nx\n``")), ("x", false));
        assert_eq!(code_of(&parse("```\nx\n```")), ("x", true));
        // A longer fence closes only on as many, so a shorter run on its
        // last line may still be the close arriving.
        assert_eq!(code_of(&parse("````\nx\n```")), ("x", false));
        // Not on the last line, the backticks are the code's own.
        assert_eq!(code_of(&parse("````\n``\nx")), ("``\nx", false));
    }

    #[test]
    fn streaming_fence_in_list_and_quote_runs_to_the_end() {
        let b = parse("- item\n\n  ```sh\n  cargo ch");
        let (text, closed) = code_of(&items(&b[0])[0]);
        assert_eq!((text, closed), ("cargo ch", false));
        let b = parse("> ```\n> let x\n> ``");
        let Block::Quote(inner) = &b[0] else {
            panic!("{b:?}")
        };
        assert_eq!(code_of(inner), ("let x", false));
    }

    #[test]
    fn streaming_inline_openers_stay_plain() {
        for src in [
            "so **bo",
            "so __bo",
            "a *it",
            "a ~~gone",
            "run `cargo te",
            "run ``a ` b",
            "see [the plan",
            "see [the plan]",
            "see [the plan](",
            "see [the plan](docs/PL",
            "see [the plan](https://x.dev/a \"ti",
            "see ![alt](https://x.dev/a.p",
            "costs $5 and $",
            "math $x^2",
            "a note[^",
            "a note[^lon",
            "press <kb",
            "a <b",
            "a <details",
        ] {
            let p = para(src);
            check_inline(&p);
            assert_eq!(p.text, src, "{src}");
            for s in &p.spans {
                assert!(
                    !matches!(
                        s.style,
                        Style::Bold | Style::Italic | Style::Strike | Style::Code
                    ),
                    "{src}: {s:?}"
                );
            }
        }
    }

    #[test]
    fn streaming_opener_does_not_swallow_what_came_before() {
        let p = para("**done** then **open");
        assert_eq!(under(&p, &Style::Bold), vec!["done"]);
        assert_eq!(p.text, "done then **open");
        let p = para("`a` then `b");
        assert_eq!(under(&p, &Style::Code), vec!["a"]);
        let p = para("[a](https://a.dev) and [b](https://b");
        assert_eq!(links(&p), vec![("a", "https://a.dev")]);
        assert!(paths(&para("see [the plan](docs/PLAN.md")).is_empty());
        // Once closed it is a link again, and a bare one in parens still is.
        assert!(links(&para("[b](https://b.dev)")).len() == 1);
        assert_eq!(links(&para("(https://b.dev)")).len(), 1);
    }

    #[test]
    fn file_references_in_prose_code_and_cells() {
        let p = para("broke at src/a.rs:12:5, see `crates/b/c.rs:9` and main.rs:3.");
        assert_eq!(
            paths(&p),
            vec![
                ("src/a.rs:12:5", "src/a.rs".to_string(), Some(12)),
                ("crates/b/c.rs:9", "crates/b/c.rs".to_string(), Some(9)),
                ("main.rs:3", "main.rs".to_string(), Some(3)),
            ]
        );
        // A reference streaming in is a path at each step, its line
        // growing: never text that turns into one late.
        assert_eq!(paths(&para("src/a.rs:1"))[0].2, Some(1));
        assert_eq!(paths(&para("src/a.rs:12"))[0].2, Some(12));
        assert_eq!(paths(&para("src/a.rs:"))[0].2, None);
        assert_eq!(paths(&para("src/a.rs:12:"))[0].2, Some(12));
    }

    #[test]
    fn streaming_task_quote_and_footnote() {
        // A box without words yet is text, then a task once they arrive.
        let b = parse("- [ ]");
        assert_eq!(tasks(&b[0]), &vec![None]);
        let b = parse("- [ ] wr");
        assert_eq!(tasks(&b[0]), &vec![Some(false)]);
        // A quote streaming a second line keeps the first.
        let b = parse("> one\n> tw");
        let Block::Quote(inner) = &b[0] else {
            panic!("{b:?}")
        };
        assert!(matches!(&inner[..], [Block::Paragraph(p)] if p.text == "one\ntw"));
        // A note's definition half typed never shows its marker as text.
        let b = parse("Claim[^1].\n\n[^1]: The so");
        assert!(
            matches!(&b[..], [Block::Paragraph(_), Block::Footnotes(n)] if n[0].1.text == "The so")
        );
        // A math block streaming runs to the end, and a half close is hidden.
        assert_eq!(code_of(&parse("$$\nx^2\n$")), ("x^2", false));
        assert_eq!(code_of(&parse("$$\nx^2\n$$")), ("x^2", true));
    }

    /// Everything before the block still streaming in is final: the reply
    /// cut short anywhere shows its earlier blocks as the whole reply does.
    fn assert_streams_stably(src: &str, cuts: impl Iterator<Item = usize>) {
        let whole: Vec<Block> = (parse(src).into_iter())
            .filter(|b| !matches!(b, Block::Footnotes(_)))
            .collect();
        for cut in cuts {
            let mut part: Vec<Block> = (parse(&src[..cut]).into_iter())
                .filter(|b| !matches!(b, Block::Footnotes(_)))
                .collect();
            check(&part);
            part.pop();
            assert!(part.len() <= whole.len(), "cut at {cut}");
            for (k, b) in part.iter().enumerate() {
                assert_eq!(b, &whole[k], "block {k}, cut at {cut}");
            }
        }
    }

    #[test]
    fn sample_streams_stably() {
        assert_streams_stably(SAMPLE, SAMPLE.char_indices().map(|(i, _)| i));
    }

    // Robustness

    fn check_all(src: &str) {
        check(&parse(src));
        check_inline(&parse_inline(src));
    }

    #[test]
    fn pathological_inputs() {
        check_all(&"*".repeat(5000));
        check_all(&"*a".repeat(2000));
        check_all(&"_a_".repeat(2000));
        check_all(&"[".repeat(3000));
        check_all(&"[a](".repeat(1000));
        check_all(&"`".repeat(3000));
        check_all(&"` ``".repeat(1000));
        check_all(&">".repeat(500));
        check_all(&"- ".repeat(500));
        check_all(&"  ".repeat(500).replace("  ", "- x\n  "));
        check_all(
            &"[".repeat(100)
                .chars()
                .chain("a](u)".chars().cycle().take(500))
                .collect::<String>(),
        );
        check_all("|\n|-|\n|");
        check_all("\\");
        check_all("a\\");
        check_all("[a](<");
        check_all("[a](u \"t");
        check_all("<http://");
        check_all("1.");
        check_all("1234567890. x");
        check_all("\r");
        check_all("\t-\tx\n\t\t-\ty");
    }

    /// Each input would take seconds if every bracket rescanned the rest.
    #[test]
    fn unmatched_brackets_parse_in_linear_time() {
        let n = 200_000;
        let inputs = [
            "[".repeat(n),
            "[a".repeat(n / 2),
            "[a](".repeat(n / 4),
            "[](".repeat(n / 3),
            "[a](b".repeat(n / 5),
            "[a](b \"".repeat(n / 7),
            "[a](<".repeat(n / 5),
            "](".repeat(n / 2),
            "[[a]".repeat(n / 4),
            "[".repeat(n / 2) + &"]".repeat(n / 2),
            "[x](".repeat(n / 8) + &")".repeat(n / 8),
            "<details>".repeat(11_111),
            "a_".repeat(50_000),
            "see https://x.dev/a".to_string() + &")".repeat(50_000),
            "<b>".repeat(n / 3),
            "<!--".repeat(n / 4),
        ];
        for src in &inputs {
            let t = std::time::Instant::now();
            check_all(src);
            let took = t.elapsed();
            assert!(
                took < std::time::Duration::from_secs(2),
                "{:?} took {took:?}",
                &src[..8]
            );
        }
    }

    #[test]
    fn brackets_still_match_after_unmatched_ones() {
        let p = para("[[a](https://a.dev) [b [c](https://c.dev)");
        assert_eq!(
            links(&p),
            vec![("a", "https://a.dev"), ("c", "https://c.dev")]
        );
        assert_eq!(p.text, "[a [b c");
    }

    #[test]
    fn deep_nesting_is_capped() {
        let quotes = ">".repeat(200) + " x";
        check_all(&quotes);
        let mut list = String::new();
        for d in 0..200 {
            list.push_str(&"  ".repeat(d));
            list.push_str("- x\n");
        }
        check_all(&list);
        let links = "[".repeat(200) + "x" + &"](u)".repeat(200);
        check_all(&links);
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    #[test]
    fn random_slices_never_panic() {
        let bytes = SAMPLE.as_bytes();
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for _ in 0..3000 {
            let a = rng.below(bytes.len() + 1);
            let b = rng.below(bytes.len() + 1);
            let slice = String::from_utf8_lossy(&bytes[a.min(b)..a.max(b)]);
            check_all(&slice);
        }
    }

    #[test]
    fn random_mutations_never_panic() {
        const NOISE: &[char] = &[
            '*', '_', '~', '`', '[', ']', '(', ')', '<', '>', '|', '#', '-', '\\', '\n', ' ', '!',
            ':', '/', '1', '.', '\t', '\r', 'é', '😀', 'h',
        ];
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        for _ in 0..2000 {
            let mut chars: Vec<char> = SAMPLE.chars().collect();
            for _ in 0..rng.below(40) + 1 {
                let at = rng.below(chars.len() + 1);
                let c = NOISE[rng.below(NOISE.len())];
                if rng.below(3) == 0 && at < chars.len() {
                    chars.remove(at);
                } else {
                    chars.insert(at, c);
                }
            }
            let start = rng.below(chars.len() + 1);
            let text: String = chars[start..].iter().collect();
            check_all(&text);
        }
    }

    // Fixtures

    const FIXTURES: &[&str] = &[
        include_str!("../tests/fixtures/headless/ask_interrupt.jsonl"),
        include_str!("../tests/fixtures/headless/bash_allow.jsonl"),
        include_str!("../tests/fixtures/headless/bash_deny.jsonl"),
        include_str!("../tests/fixtures/headless/interrupt.jsonl"),
        include_str!("../tests/fixtures/headless/plain.jsonl"),
        include_str!("../tests/fixtures/headless/tools.jsonl"),
        include_str!("../tests/fixtures/headless/two_turns.jsonl"),
    ];

    #[test]
    fn every_fixture_reply_parses() {
        let mut replies = 0;
        for fixture in FIXTURES {
            let mut streamed = String::new();
            for line in fixture.lines() {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                if v["type"] == "assistant" {
                    for part in v["message"]["content"].as_array().into_iter().flatten() {
                        if part["type"] == "text" {
                            if let Some(text) = part["text"].as_str() {
                                let blocks = parse(text);
                                check(&blocks);
                                assert!(!blocks.is_empty() || text.trim().is_empty());
                                replies += 1;
                            }
                        }
                    }
                }
                let delta = &v["event"]["delta"];
                if v["type"] == "stream_event" && delta["type"] == "text_delta" {
                    streamed.push_str(delta["text"].as_str().unwrap_or(""));
                    check(&parse(&streamed));
                }
            }
        }
        assert!(replies > 0);
    }
}
