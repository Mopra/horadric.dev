//! A whole file written from the page's editor: whether the file is still
//! what the edit started from, and the bytes that go to disk.

use std::borrow::Cow;

/// `s` as the editor compares it: line endings as `\n` and no byte order
/// mark. The page reads the file with `fetch`, whose text drops the mark,
/// and an editor may keep `\r\n` or not; neither is a change the human made.
pub fn normalise(s: &str) -> Cow<'_, str> {
    let s = s.strip_prefix('\u{feff}').unwrap_or(s);
    if !s.contains('\r') {
        return Cow::Borrowed(s);
    }
    Cow::Owned(s.replace("\r\n", "\n").replace('\r', "\n"))
}

/// Whether the file on disk, `disk`, is still the text the page's edit
/// started from. Anything else means someone wrote it since, and writing
/// over it would lose their change.
pub fn unchanged(disk: &str, base: &str) -> bool {
    normalise(disk) == normalise(base)
}

/// The bytes for `text`, the page's new file, as it sent them, line
/// endings included. A mark the file began with stays: the page never saw
/// it, and a program that wrote it may want it.
pub fn bytes(disk: &str, text: &str) -> Vec<u8> {
    let mark = disk.starts_with('\u{feff}') && !text.starts_with('\u{feff}');
    let mut out = Vec::with_capacity(text.len() + 3);
    if mark {
        out.extend_from_slice("\u{feff}".as_bytes());
    }
    out.extend_from_slice(text.as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_endings_and_the_mark_are_not_a_change() {
        assert!(unchanged("a\r\nb\r\n", "a\nb\n"));
        assert!(unchanged("a\nb", "a\r\nb"));
        assert!(unchanged("a\rb", "a\nb"));
        assert!(unchanged("\u{feff}# t\n", "# t\n"));
        assert!(unchanged("", ""));
    }

    #[test]
    fn a_changed_file_is_refused() {
        assert!(!unchanged("a\nb\n", "a\nc\n"));
        assert!(!unchanged("a\nb\n", "a\nb"));
        assert!(!unchanged("a\n\nb", "a\nb"));
        assert!(!unchanged("a b", "a  b"));
    }

    #[test]
    fn the_text_is_written_as_sent_and_keeps_the_mark() {
        assert_eq!(bytes("x\r\n", "y\r\nz\r\n"), b"y\r\nz\r\n");
        assert_eq!(bytes("x\n", "y\r\n"), b"y\r\n");
        assert_eq!(bytes("\u{feff}x", "y"), "\u{feff}y".as_bytes());
        assert_eq!(bytes("\u{feff}x", "\u{feff}y"), "\u{feff}y".as_bytes());
        assert_eq!(bytes("x", "y"), b"y");
    }
}
