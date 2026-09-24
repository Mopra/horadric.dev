//! What a paste or a drop should put in front of the agent.
//!
//! A terminal can only carry text, so an image reaches the agent as the path
//! of a file holding it. Claude Code turns a pasted image path into an
//! attachment, which is what it does for a file dragged onto any terminal.

/// What the clipboard holds that a paste can use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Text(String),
    /// A bitmap, to be saved as a file and pasted as its path.
    Image,
    /// Files copied in Explorer, pasted as their paths.
    Files,
    Nothing,
}

/// Picks what to paste. Text wins, because a spreadsheet or a document puts
/// a picture of the copied cells beside the text and the text is what you
/// meant. The exception is a browser's Copy Image, which puts the image
/// address beside the image.
pub fn choose(text: Option<String>, image: bool, files: bool) -> Source {
    match text {
        Some(t) if image && (t.trim().is_empty() || is_url(t.trim())) => Source::Image,
        Some(t) if !t.is_empty() => Source::Text(t),
        _ if image => Source::Image,
        _ if files => Source::Files,
        _ => Source::Nothing,
    }
}

fn is_url(text: &str) -> bool {
    let scheme = ["http://", "https://", "file://", "data:"].iter().any(|s| {
        text.len() > s.len()
            && text
                .get(..s.len())
                .is_some_and(|h| h.eq_ignore_ascii_case(s))
    });
    scheme && !text.contains(char::is_whitespace)
}

/// Paths the way Windows Terminal pastes a drop: quoted when they hold a
/// space, separated by spaces.
pub fn quote_paths<S: AsRef<str>>(paths: &[S]) -> String {
    paths
        .iter()
        .map(|p| {
            let p = p.as_ref();
            if p.contains(' ') {
                format!("\"{p}\"")
            } else {
                p.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    #[test]
    fn text_beats_the_picture_of_it() {
        assert_eq!(
            choose(text("a\tb"), true, false),
            Source::Text("a\tb".into())
        );
        assert_eq!(
            choose(text("hello"), false, true),
            Source::Text("hello".into())
        );
    }

    #[test]
    fn an_image_beats_its_own_address() {
        assert_eq!(
            choose(text("https://x.dk/a.png"), true, false),
            Source::Image
        );
        assert_eq!(choose(text("  \r\n"), true, false), Source::Image);
        assert_eq!(choose(None, true, true), Source::Image);
        assert_eq!(
            choose(text("https://x.dk/a.png"), false, false),
            Source::Text("https://x.dk/a.png".into())
        );
    }

    #[test]
    fn files_then_nothing() {
        assert_eq!(choose(None, false, true), Source::Files);
        assert_eq!(choose(text(""), false, true), Source::Files);
        assert_eq!(choose(None, false, false), Source::Nothing);
    }

    #[test]
    fn urls_are_one_word_with_a_scheme() {
        assert!(is_url("HTTPS://x.dk"));
        assert!(is_url("data:image/png;base64,AAA"));
        assert!(!is_url("https://x.dk and more"));
        assert!(!is_url("https://"));
        assert!(!is_url("see https://x.dk"));
        assert!(!is_url("\u{e6}\u{f8}\u{e5}\u{e6}\u{f8}\u{e5}"));
    }

    #[test]
    fn paths_are_quoted_only_when_they_must_be() {
        assert_eq!(quote_paths(&["C:\\a.png"]), "C:\\a.png");
        assert_eq!(
            quote_paths(&["C:\\My Pics\\b.png", "C:\\c.png"]),
            "\"C:\\My Pics\\b.png\" C:\\c.png"
        );
        assert_eq!(quote_paths::<&str>(&[]), "");
    }
}
