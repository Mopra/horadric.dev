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
    },
    /// `closed` is false while a streaming reply has not sent the closing
    /// fence yet.
    Code {
        lang: String,
        text: String,
        closed: bool,
    },
    Quote(Vec<Block>),
    /// Every row has exactly as many cells as `header` and `align`.
    Table {
        header: Vec<Inline>,
        align: Vec<Align>,
        rows: Vec<Vec<Inline>>,
    },
    Rule,
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
    blocks(&lines, 0)
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

fn blocks(lines: &[&str], depth: usize) -> Vec<Block> {
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
        if shallow {
            if let Some(fence) = Fence::open(line) {
                i = code_block(lines, i, fence, &mut out);
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
                i = quote(lines, i, depth, &mut out);
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
        if Fence::open(line).is_some() || atx(t).is_some() || is_rule(t) || t.starts_with('>') {
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
    while i < lines.len() && !interrupts(lines, i) {
        i += 1;
    }
    let text = lines[start..i]
        .iter()
        .map(|l| l.trim_start())
        .collect::<Vec<_>>()
        .join("\n");
    out.push(Block::Paragraph(parse_inline(&text)));
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
    out.push(Block::Code {
        lang: fence.lang,
        text: body.join("\n"),
        closed,
    });
    i
}

fn quote(lines: &[&str], start: usize, depth: usize, out: &mut Vec<Block>) -> usize {
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
    out.push(Block::Quote(blocks(&inner, depth + 1)));
    i
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
    let mut i = start;
    loop {
        let mut body = vec![marker.body];
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
        items.push(blocks(&body, depth + 1));
        match lines.get(i).and_then(|l| Marker::read(l)) {
            Some(next) if next.ordered == ordered && next.ch == ch => marker = next,
            _ => break,
        }
    }
    out.push(Block::List {
        ordered,
        start: number,
        items,
    });
    i
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
                let open = if b[i] == b'!' { i + 1 } else { i };
                if b.get(open) == Some(&b'[') {
                    if let Some((text, url, end)) = link_at(s, open, &mut closes) {
                        flush(&mut buf, &mut toks);
                        toks.push(Tok::Link(Box::new(tokenize(&s[text], depth + 1)), url));
                        i = end;
                        continue;
                    }
                }
            }
            b'<' => {
                if let Some((url, end)) = angle_link(s, i) {
                    flush(&mut buf, &mut toks);
                    toks.push(Tok::Url(url.to_string()));
                    i = end;
                    continue;
                }
            }
            _ => {}
        }
        // Inside a link's text the link is already the target.
        if depth == 0 && at_word_start(s, i) {
            if let Some(end) = url_at(s, i) {
                flush(&mut buf, &mut toks);
                toks.push(Tok::Url(s[i..end].to_string()));
                i = end;
                continue;
            }
            if let Some((end, path, line)) = path_at(s, i) {
                flush(&mut buf, &mut toks);
                toks.push(Tok::Path(s[i..end].to_string(), path, line));
                i = end;
                continue;
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
        return None;
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

fn skip_space(b: &[u8], mut k: usize) -> usize {
    while b.get(k).is_some_and(|c| matches!(c, b' ' | b'\t' | b'\n')) {
        k += 1;
    }
    k
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
    while end > scheme {
        let url = &rest[..end];
        let last = url.as_bytes()[end - 1];
        let trailing = b"?!.,:;*_~'\"".contains(&last);
        let unbalanced = last == b')' && url.matches(')').count() > url.matches('(').count();
        if trailing || unbalanced {
            end -= 1;
        } else {
            break;
        }
    }
    (end > scheme).then_some(i + end)
}

fn path_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b'/' | b'\\')
}

/// A file path in running text at `i`: where it ends, the path and its line.
fn path_at(s: &str, i: usize) -> Option<(usize, String, Option<u32>)> {
    let b = s.as_bytes();
    if !(b[i].is_ascii_alphanumeric() || matches!(b[i], b'.' | b'/' | b'\\')) {
        return None;
    }
    let mut end = i + b[i..]
        .iter()
        .take_while(|&&c| path_char(c) || c == b':')
        .count();
    while end > i && !b[end - 1].is_ascii_alphanumeric() {
        end -= 1;
    }
    let (path, line) = split_path(&s[i..end], true)?;
    Some((end, path, line))
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
        }
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

    #[test]
    fn task_box_stays_text() {
        assert_eq!(item_text(&items(&parse("- [ ] todo")[0])[0]), "[ ] todo");
    }

    // Code blocks

    #[test]
    fn fenced_code_with_lang() {
        assert_eq!(
            parse("```rust\nfn main() {}\n```"),
            vec![Block::Code {
                lang: "rust".into(),
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
    fn image_shows_as_link() {
        assert_eq!(
            links(&para("![logo](https://x.dev/a.png)")),
            vec![("logo", "https://x.dev/a.png")]
        );
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
    fn rule_after_paragraph() {
        assert_eq!(
            parse("text\n---"),
            vec![Block::Paragraph(plain("text")), Block::Rule]
        );
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
    fn entities_stay_as_text() {
        assert_eq!(para("&amp; &lt;b&gt; &#42;").text, "&amp; &lt;b&gt; &#42;");
    }

    #[test]
    fn html_stays_as_text() {
        assert_eq!(para("<b>hi</b>").text, "<b>hi</b>");
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
