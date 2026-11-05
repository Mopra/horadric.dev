//! Putting a hand edit back into the source it was rendered from.

/// The line ending `src` uses, so an edit typed in the page (always `\n`)
/// does not leave a CRLF file with mixed endings.
fn eol(src: &str) -> &'static str {
    if src.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

fn with_eol(text: &str, eol: &str) -> String {
    let unix = text.replace("\r\n", "\n");
    if eol == "\n" {
        unix
    } else {
        unix.replace('\n', eol)
    }
}

/// `src` with lines `a` to `b` (1 based, inclusive, as a markdown block's
/// `data-lines`) replaced by `new_text`. An empty `new_text` deletes the
/// lines; `b < a` inserts before line `a`. Line endings follow the file's.
pub fn replace_lines(src: &str, a: usize, b: usize, new_text: &str) -> String {
    let lines: Vec<&str> = src.split_inclusive('\n').collect();
    let n = lines.len();
    let from = a.saturating_sub(1).min(n);
    let to = b.min(n).max(from);
    let before = lines[..from].concat();
    let after = lines[to..].concat();
    let eol = eol(src);
    let mut out = String::with_capacity(src.len() + new_text.len());
    out.push_str(&before);
    if !new_text.is_empty() {
        if !before.is_empty() && !before.ends_with('\n') {
            out.push_str(eol);
        }
        let body = new_text.strip_suffix('\n').unwrap_or(new_text);
        let body = body.strip_suffix('\r').unwrap_or(body);
        out.push_str(&with_eol(body, eol));
        let ended = to > from && lines[to - 1].ends_with('\n');
        if !after.is_empty() || ended {
            out.push_str(eol);
        }
    }
    out.push_str(&after);
    out
}

/// `src` with the bytes from `start` to `end` replaced by `new`, as an
/// HTML element's offsets recorded when the file was served. Offsets past
/// the end are clamped and ones inside a character move back to its start,
/// so a stale offset cannot panic. Line endings in `new` follow the file's.
pub fn replace_range(src: &str, start: usize, end: usize, new: &str) -> String {
    let start = floor(src, start);
    let end = floor(src, end).max(start);
    let mut out = String::with_capacity(src.len() + new.len());
    out.push_str(&src[..start]);
    out.push_str(&with_eol(new, eol(src)));
    out.push_str(&src[end..]);
    out
}

/// The byte offset in `src` of a UTF-16 offset, which is what the page's
/// JavaScript counts in. Clamped to the end.
pub fn utf16_to_byte(src: &str, units: usize) -> usize {
    let mut count = 0;
    for (i, c) in src.char_indices() {
        if count >= units {
            return i;
        }
        count += c.len_utf16();
    }
    src.len()
}

fn floor(s: &str, mut at: usize) -> usize {
    at = at.min(s.len());
    while !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forge_replace_lines_lf() {
        let src = "a\nb\nc\nd\n";
        assert_eq!(replace_lines(src, 2, 3, "X\nY\nZ"), "a\nX\nY\nZ\nd\n");
        assert_eq!(replace_lines(src, 2, 3, "X\n"), "a\nX\nd\n");
        assert_eq!(replace_lines(src, 2, 3, ""), "a\nd\n");
        assert_eq!(replace_lines(src, 1, 1, "Z"), "Z\nb\nc\nd\n");
        assert_eq!(replace_lines(src, 3, 2, "new"), "a\nb\nnew\nc\nd\n");
        assert_eq!(replace_lines(src, 4, 9, "end"), "a\nb\nc\nend\n");
    }

    #[test]
    fn forge_replace_lines_crlf_and_no_final_newline() {
        let src = "# T\r\n\r\nold\r\ntext\r\n\r\nafter";
        assert_eq!(replace_lines(src, 3, 4, "new\ntext"), "# T\r\n\r\nnew\r\ntext\r\n\r\nafter");
        assert_eq!(replace_lines(src, 6, 6, "last"), "# T\r\n\r\nold\r\ntext\r\n\r\nlast");
        assert_eq!(replace_lines("a", 2, 1, "b"), "a\nb");
        assert_eq!(replace_lines("", 1, 1, "x"), "x");
    }

    #[test]
    fn forge_replace_range_unicode() {
        let src = "<p>Grüße</p>\r\n<p>ø</p>";
        let s = src.find("Grüße").unwrap();
        assert_eq!(replace_range(src, s, s + "Grüße".len(), "Hej"), "<p>Hej</p>\r\n<p>ø</p>");
        assert_eq!(replace_range(src, 0, 0, "a\nb"), "a\r\nb<p>Grüße</p>\r\n<p>ø</p>");
        // Inside the two byte ü: moves back to its start, never panics.
        assert_eq!(replace_range("ü", 1, 1, "x"), "xü");
        assert_eq!(replace_range("ab", 5, 9, "c"), "abc");
        assert_eq!(replace_range("abc", 2, 1, "X"), "abXc");
    }

    #[test]
    fn forge_utf16_offsets() {
        let src = "a😀bø";
        assert_eq!(utf16_to_byte(src, 0), 0);
        assert_eq!(utf16_to_byte(src, 1), 1);
        assert_eq!(utf16_to_byte(src, 3), 5);
        assert_eq!(utf16_to_byte(src, 4), 6);
        assert_eq!(utf16_to_byte(src, 99), src.len());
    }
}
