//! The Forge's pure logic: which files it shows and how, and turning what
//! the human points at back into words and source (docs/plan/the-forge.md).
//!
//! Nothing here touches a window or a web view. The UI serves what these
//! return to the browser pane and writes the files they produce.

mod csv;
mod diff;
mod edit;
mod html;
mod md;
mod pins;
pub mod versions;

use std::io::Read;
use std::path::Path;

pub use csv::csv_html;
pub use diff::{diff_lines, DiffLine};
pub use edit::{replace_lines, replace_range, utf16_to_byte};
pub use html::LOOK;
pub use md::{markdown_body, markdown_html};
pub use pins::{pin_message, At, Pin, Rect};

/// What a file is to the Forge, which decides how it is shown and what a
/// pin on it lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Markdown,
    Html,
    /// HTML with a `<section class="slide">` per slide.
    Deck,
    Jsx,
    Pptx,
    Docx,
    Pdf,
    Image,
    Svg,
    Mermaid,
    Csv,
}

/// How much of an HTML file is read to tell a deck from a page. A deck's
/// first slide comes after its stylesheet, which can be long when inline.
const DECK_PREFIX: u64 = 1 << 20;

/// The kind of the file at `path`, by its extension. An HTML file is read
/// (its first megabyte) to see whether it is a deck.
pub fn kind_of(path: &Path) -> Option<Kind> {
    let kind = by_extension(path)?;
    if kind != Kind::Html {
        return Some(kind);
    }
    let mut head = Vec::new();
    let read = std::fs::File::open(path).and_then(|f| f.take(DECK_PREFIX).read_to_end(&mut head));
    Some(if read.is_ok() && is_deck(&head) {
        Kind::Deck
    } else {
        Kind::Html
    })
}

/// [`kind_of`] for a caller that already holds the file's bytes.
pub fn kind_of_bytes(path: &Path, bytes: &[u8]) -> Option<Kind> {
    match by_extension(path)? {
        Kind::Html if is_deck(bytes) => Some(Kind::Deck),
        kind => Some(kind),
    }
}

fn by_extension(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "md" | "markdown" => Kind::Markdown,
        "html" | "htm" => Kind::Html,
        "jsx" | "tsx" => Kind::Jsx,
        "pptx" => Kind::Pptx,
        "docx" => Kind::Docx,
        "pdf" => Kind::Pdf,
        "png" | "jpg" | "jpeg" | "gif" | "webp" => Kind::Image,
        "svg" => Kind::Svg,
        "mmd" | "mermaid" => Kind::Mermaid,
        "csv" => Kind::Csv,
        _ => return None,
    })
}

/// Whether the HTML holds a `<section>` whose class list has `slide`. The
/// class may share the attribute with others (`class="slide title"`) and
/// either quote, so a plain substring search would miss real decks.
fn is_deck(bytes: &[u8]) -> bool {
    let lower = bytes.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = find(&lower[from..], b"<section") {
        let start = from + at + b"<section".len();
        let end = lower[start..]
            .iter()
            .position(|&b| b == b'>')
            .map_or(lower.len(), |e| start + e);
        if class_has_slide(&lower[start..end]) {
            return true;
        }
        from = start;
    }
    false
}

fn class_has_slide(tag: &[u8]) -> bool {
    let mut from = 0;
    while let Some(at) = find(&tag[from..], b"class") {
        let mut i = from + at + b"class".len();
        let before = (from + at).checked_sub(1).map(|p| tag[p]);
        from = i;
        if !matches!(before, Some(b' ' | b'\t' | b'\n' | b'\r')) {
            continue;
        }
        while tag.get(i).is_some_and(|b| b.is_ascii_whitespace()) {
            i += 1;
        }
        if tag.get(i) != Some(&b'=') {
            continue;
        }
        i += 1;
        while tag.get(i).is_some_and(|b| b.is_ascii_whitespace()) {
            i += 1;
        }
        let value = match tag.get(i) {
            Some(&q @ (b'"' | b'\'')) => {
                let rest = &tag[i + 1..];
                &rest[..rest.iter().position(|&b| b == q).unwrap_or(rest.len())]
            }
            _ => {
                let rest = &tag[i..];
                &rest[..rest
                    .iter()
                    .position(|b| b.is_ascii_whitespace())
                    .unwrap_or(rest.len())]
            }
        };
        if value
            .split(|b| b.is_ascii_whitespace())
            .any(|w| w == b"slide")
        {
            return true;
        }
    }
    false
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forge_kind_by_extension() {
        assert_eq!(kind_of_bytes(Path::new("a/B.MD"), b""), Some(Kind::Markdown));
        assert_eq!(kind_of_bytes(Path::new("x.htm"), b"<p>"), Some(Kind::Html));
        assert_eq!(kind_of_bytes(Path::new("x.tsx"), b""), Some(Kind::Jsx));
        assert_eq!(kind_of_bytes(Path::new("x.jpeg"), b""), Some(Kind::Image));
        assert_eq!(kind_of_bytes(Path::new("x.svg"), b""), Some(Kind::Svg));
        assert_eq!(kind_of_bytes(Path::new("x.mmd"), b""), Some(Kind::Mermaid));
        assert_eq!(kind_of_bytes(Path::new("x.csv"), b""), Some(Kind::Csv));
        assert_eq!(kind_of_bytes(Path::new("x.rs"), b""), None);
        assert_eq!(kind_of_bytes(Path::new("Makefile"), b""), None);
    }

    #[test]
    fn forge_deck_detection() {
        let deck = |s: &str| kind_of_bytes(Path::new("d.html"), s.as_bytes());
        assert_eq!(deck(r#"<section class="slide">"#), Some(Kind::Deck));
        assert_eq!(deck(r#"<SECTION id=a CLASS='title slide'>"#), Some(Kind::Deck));
        assert_eq!(deck("<section class=slide>"), Some(Kind::Deck));
        assert_eq!(deck(r#"<section class="slides">"#), Some(Kind::Html));
        assert_eq!(deck(r#"<section data-class="slide">"#), Some(Kind::Html));
        assert_eq!(deck(r#"<div class="slide"><section>"#), Some(Kind::Html));
    }
}
