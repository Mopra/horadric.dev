//! A file shown read only in a pane, the way an editor shows it: line
//! numbers in a gutter, colours from [`crate::highlight`], long lines
//! wrapped under their text rather than under the numbers.
//!
//! The pane is a terminal grid with no program behind it. This module turns
//! the file into the escape sequences that paint that grid, and keeps the
//! map from grid rows back to lines, which scrolling across a resize and
//! copying without the line numbers both need.

/// Spaces a tab stands for. What VS Code shows by default.
const TAB: usize = 4;
/// A file longer than this shows its start and says so.
pub const MAX_LINES: usize = 10_000;
/// A file bigger than this is not read at all.
pub const MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Grid rows at most, however narrow the pane makes the wrapping. Each row
/// costs a full width of cells.
pub const MAX_ROWS: usize = 20_000;
/// Enough room for text beside the gutter in a very narrow pane.
const MIN_TEXT: usize = 8;

/// VS Code's dark theme: its text and its line numbers.
pub const TEXT: Style = Style::plain([0xD4, 0xD4, 0xD4]);
const NUMBER: Style = Style::plain([0x6E, 0x76, 0x81]);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub fg: [u8; 3],
    pub bold: bool,
    pub italic: bool,
}

impl Style {
    pub const fn plain(fg: [u8; 3]) -> Style {
        Style {
            fg,
            bold: false,
            italic: false,
        }
    }

    fn sgr(self, out: &mut Vec<u8>) {
        let [r, g, b] = self.fg;
        let mut s = format!("\x1b[0;38;2;{r};{g};{b}");
        if self.bold {
            s.push_str(";1");
        }
        if self.italic {
            s.push_str(";3");
        }
        s.push('m');
        out.extend_from_slice(s.as_bytes());
    }
}

/// A run of one style, `len` bytes long. A line's spans follow each other
/// from its start; text past the last one is [`TEXT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub len: usize,
    pub style: Style,
}

/// One grid row: which line it shows, and which bytes of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

pub struct Rendered {
    /// What to feed the terminal.
    pub bytes: Vec<u8>,
    pub rows: Vec<Row>,
    /// Columns the line numbers take, spacing included.
    pub gutter: usize,
}

/// Whether the file is better not shown as text.
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|&b| b == 0)
}

/// The file's lines, ready to draw: tabs expanded, control characters made
/// visible so none can reach the terminal as a command, at most
/// [`MAX_LINES`]. Also the number of lines the file really has.
pub fn lines(text: &str) -> (Vec<String>, usize) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut all: Vec<&str> = text.split('\n').collect();
    if all.last() == Some(&"") {
        all.pop();
    }
    let total = all.len();
    let shown = all
        .into_iter()
        .take(MAX_LINES)
        .map(|l| clean(l.strip_suffix('\r').unwrap_or(l)))
        .collect();
    (shown, total)
}

fn clean(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut col = 0;
    for c in line.chars() {
        match c {
            '\t' => {
                let n = TAB - col % TAB;
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            // The Unicode pictures of the control characters.
            '\0'..='\x1f' => {
                out.push(char::from_u32(0x2400 + c as u32).unwrap_or('?'));
                col += 1;
            }
            '\x7f' => {
                out.push('\u{2421}');
                col += 1;
            }
            '\u{80}'..='\u{9f}' => {
                out.push('\u{fffd}');
                col += 1;
            }
            _ => {
                out.push(c);
                col += width(c);
            }
        }
    }
    out
}

/// Cells a character takes: two for the wide ranges of East Asian scripts
/// and emoji, otherwise one. Close enough for code; a miss only moves a
/// wrap by a cell.
pub fn width(c: char) -> usize {
    match c as u32 {
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
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// Columns for the line numbers of a file this long: the widest number,
/// never under three digits, a space before and two after.
pub fn gutter(lines: usize) -> usize {
    lines.max(1).to_string().len().max(3) + 3
}

/// Lays the lines out for a grid `cols` wide. `spans` may cover fewer
/// lines than there are, while highlighting is still running.
pub fn render(lines: &[String], spans: &[Vec<Span>], cols: usize) -> Rendered {
    let gutter = gutter(lines.len());
    let avail = cols.saturating_sub(gutter).max(MIN_TEXT);
    let digits = gutter - 3;
    // No wrapping by the terminal, and no cursor: this is not a prompt.
    let mut bytes = b"\x1b[?7l\x1b[?25l".to_vec();
    let mut rows = Vec::new();
    'lines: for (n, line) in lines.iter().enumerate() {
        let mut styles = Styles::new(spans.get(n).map_or(&[][..], Vec::as_slice));
        let mut start = 0;
        loop {
            if rows.len() == MAX_ROWS {
                break 'lines;
            }
            let end = wrap(line, start, avail);
            if !rows.is_empty() {
                bytes.extend_from_slice(b"\r\n");
            }
            NUMBER.sgr(&mut bytes);
            let number = if start == 0 {
                format!(" {:>digits$}  ", n + 1)
            } else {
                " ".repeat(gutter)
            };
            bytes.extend_from_slice(number.as_bytes());
            let mut current = None;
            for (i, c) in line[start..end].char_indices() {
                let style = styles.at(start + i);
                if current != Some(style) {
                    style.sgr(&mut bytes);
                    current = Some(style);
                }
                let mut buf = [0u8; 4];
                bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            rows.push(Row {
                line: n,
                start,
                end,
            });
            if end >= line.len() {
                break;
            }
            start = end;
        }
    }
    Rendered {
        bytes,
        rows,
        gutter,
    }
}

/// Where the row starting at `start` ends, `avail` cells later: after the
/// last space that fits, as an editor wraps words, or mid word when one
/// word is wider than the row. At least one character, so a character
/// wider than the space still moves on.
fn wrap(line: &str, start: usize, avail: usize) -> usize {
    let mut cells = 0;
    let mut after_space = None;
    for (i, c) in line[start..].char_indices() {
        cells += width(c);
        if cells > avail && i > 0 {
            return after_space.unwrap_or(start + i);
        }
        if c == ' ' {
            after_space = Some(start + i + 1);
        }
    }
    line.len()
}

/// Walks a line's spans in order, answering the style at a byte.
struct Styles<'a> {
    spans: &'a [Span],
    next: usize,
    /// Where the span at `next` ends.
    end: usize,
}

impl<'a> Styles<'a> {
    fn new(spans: &'a [Span]) -> Self {
        Styles {
            spans,
            next: 0,
            end: spans.first().map_or(0, |s| s.len),
        }
    }

    fn at(&mut self, byte: usize) -> Style {
        while byte >= self.end {
            self.next += 1;
            match self.spans.get(self.next) {
                Some(s) => self.end += s.len,
                None => return TEXT,
            }
        }
        self.spans[self.next].style
    }
}

/// The first row showing this line, or the last row when it is past them.
pub fn row_of(rows: &[Row], line: usize) -> usize {
    rows.partition_point(|r| r.line < line)
        .min(rows.len().saturating_sub(1))
}

/// A cell in the grid, as the selection names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub row: usize,
    pub col: usize,
}

/// The text between two cells, both included, as it is in the file: no line
/// numbers, and a wrapped line comes out whole.
pub fn copy(lines: &[String], rows: &[Row], gutter: usize, from: Cell, to: Cell) -> String {
    let last = rows.len().saturating_sub(1);
    let (Some(a), Some(b)) = (rows.get(from.row), rows.get(to.row.min(last))) else {
        return String::new();
    };
    let start = byte_at(&lines[a.line], a, from.col.saturating_sub(gutter), false);
    let end = if to.row >= rows.len() {
        b.end
    } else if to.col < gutter {
        // Only numbers selected on the last row: none of its text.
        b.start
    } else {
        byte_at(&lines[b.line], b, to.col - gutter, true)
    };
    let mut out = String::new();
    for (n, line) in lines.iter().enumerate().take(b.line + 1).skip(a.line) {
        let s = if n == a.line { start } else { 0 };
        let e = if n == b.line { end } else { line.len() };
        if n > a.line {
            out.push('\n');
        }
        if s < e {
            out.push_str(&line[s..e]);
        }
    }
    out
}

/// The byte where the character covering a cell of the row starts, or with
/// `after`, where it ends.
fn byte_at(line: &str, row: &Row, col: usize, after: bool) -> usize {
    let mut cells = 0;
    for (i, c) in line[row.start..row.end].char_indices() {
        cells += width(c);
        if cells > col {
            let at = row.start + i;
            return if after { at + c.len_utf8() } else { at };
        }
    }
    row.end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// What the grid would show, one string per row, escapes dropped.
    fn screen(r: &Rendered) -> Vec<String> {
        let text = String::from_utf8(r.bytes.clone()).unwrap();
        let mut plain = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else if c != '\r' {
                plain.push(c);
            }
        }
        plain.split('\n').map(str::to_string).collect()
    }

    #[test]
    fn lines_drop_the_last_newline_and_carriage_returns() {
        assert_eq!(lines("a\r\nb\n"), (s(&["a", "b"]), 2));
        assert_eq!(lines("a\n\n"), (s(&["a", ""]), 2));
        assert_eq!(lines(""), (s(&[]), 0));
        assert_eq!(lines("\u{feff}x"), (s(&["x"]), 1));
    }

    #[test]
    fn tabs_line_up_and_controls_cannot_reach_the_terminal() {
        assert_eq!(lines("\tx\nab\tc").0, s(&["    x", "ab  c"]));
        let (l, _) = lines("a\x1b[2Jb\x7f");
        assert_eq!(l, s(&["a\u{241b}[2Jb\u{2421}"]));
    }

    #[test]
    fn a_long_file_shows_its_start_and_counts_the_rest() {
        let text = "x\n".repeat(MAX_LINES + 5);
        let (l, total) = lines(&text);
        assert_eq!((l.len(), total), (MAX_LINES, MAX_LINES + 5));
    }

    #[test]
    fn binary_is_a_nul_near_the_start() {
        assert!(is_binary(b"PNG\0\x01"));
        assert!(!is_binary("plain ö text".as_bytes()));
    }

    #[test]
    fn the_gutter_fits_the_widest_number() {
        assert_eq!(gutter(9), 6);
        assert_eq!(gutter(1000), 7);
    }

    #[test]
    fn numbers_sit_in_the_gutter_and_wraps_indent_under_the_text() {
        let r = render(&s(&["abcdefghijkl", "", "xy"]), &[], 6 + 8);
        assert_eq!(
            screen(&r),
            s(&["   1  abcdefgh", "      ijkl", "   2  ", "   3  xy"])
        );
        assert_eq!(
            r.rows,
            vec![
                Row {
                    line: 0,
                    start: 0,
                    end: 8
                },
                Row {
                    line: 0,
                    start: 8,
                    end: 12
                },
                Row {
                    line: 1,
                    start: 0,
                    end: 0
                },
                Row {
                    line: 2,
                    start: 0,
                    end: 2
                },
            ]
        );
    }

    #[test]
    fn words_wrap_whole_unless_one_is_wider_than_the_row() {
        let r = render(&s(&["ab cd efgh ijklmnopq"]), &[], 6 + 8);
        assert_eq!(
            screen(&r),
            s(&["   1  ab cd ", "      efgh ", "      ijklmnop", "      q"])
        );
    }

    #[test]
    fn a_wide_character_wraps_whole() {
        let r = render(&s(&["abcdefg\u{4e00}z"]), &[], 6 + 8);
        assert_eq!(screen(&r), s(&["   1  abcdefg", "      \u{4e00}z"]));
    }

    #[test]
    fn colours_change_only_where_the_style_does() {
        let red = Style::plain([255, 0, 0]);
        let spans = vec![vec![
            Span { len: 2, style: red },
            Span { len: 1, style: red },
            Span {
                len: 1,
                style: TEXT,
            },
        ]];
        let r = render(&s(&["fn x"]), &spans, 40);
        let text = String::from_utf8(r.bytes).unwrap();
        assert!(text.ends_with("\x1b[0;38;2;255;0;0mfn \x1b[0;38;2;212;212;212mx"));
    }

    #[test]
    fn text_past_the_spans_is_plain() {
        let bold = Style { bold: true, ..TEXT };
        let r = render(
            &s(&["ab"]),
            &[vec![Span {
                len: 1,
                style: bold,
            }]],
            40,
        );
        let text = String::from_utf8(r.bytes).unwrap();
        assert!(text.ends_with("\x1b[0;38;2;212;212;212;1ma\x1b[0;38;2;212;212;212mb"));
    }

    #[test]
    fn rows_stop_at_the_limit() {
        let many = vec!["x".to_string(); MAX_ROWS + 10];
        assert_eq!(render(&many, &[], 40).rows.len(), MAX_ROWS);
    }

    #[test]
    fn row_of_finds_a_line_after_a_resize() {
        let r = render(&s(&["abcdefghijkl", "", "xy"]), &[], 6 + 8);
        assert_eq!(row_of(&r.rows, 0), 0);
        assert_eq!(row_of(&r.rows, 1), 2);
        assert_eq!(row_of(&r.rows, 2), 3);
        assert_eq!(row_of(&r.rows, 99), 3);
    }

    #[test]
    fn copy_leaves_the_numbers_and_joins_a_wrapped_line() {
        let l = s(&["abcdefghijkl", "", "xy"]);
        let r = render(&l, &[], 6 + 8);
        let g = r.gutter;
        let c = |row, col| Cell { row, col };
        // From the gutter of the first row to the end of the last.
        assert_eq!(
            copy(&l, &r.rows, g, c(0, 0), c(3, 20)),
            "abcdefghijkl\n\nxy"
        );
        // Inside one wrapped line, across the wrap.
        assert_eq!(copy(&l, &r.rows, g, c(0, g + 6), c(1, g + 1)), "ghij");
        // Only numbers on the last row.
        assert_eq!(copy(&l, &r.rows, g, c(1, g + 2), c(3, 1)), "kl\n\n");
        // Past the last row.
        assert_eq!(copy(&l, &r.rows, g, c(3, 0), c(9, 0)), "xy");
    }

    #[test]
    fn copy_takes_a_wide_character_whole() {
        let l = s(&["a\u{4e00}b"]);
        let r = render(&l, &[], 40);
        let g = r.gutter;
        let c = |col| Cell { row: 0, col };
        assert_eq!(copy(&l, &r.rows, g, c(g + 2), c(g + 2)), "\u{4e00}");
        assert_eq!(copy(&l, &r.rows, g, c(g + 1), c(g + 3)), "\u{4e00}b");
    }
}
