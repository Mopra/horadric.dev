//! Searching a pane's history, the part that needs no window.
//!
//! The search itself is `alacritty_terminal`'s, which takes a regular
//! expression. What you type is looked for as it is, so every character
//! that means something to a regex is escaped first.

/// A pattern that matches `query` literally. The terminal's search ignores
/// case unless the query has a capital, as in an editor.
pub fn pattern(query: &str) -> String {
    let mut out = String::with_capacity(query.len() * 2);
    for c in query.chars() {
        if r"\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// What the bar says after the query, in a terminal or a file view.
pub fn status(query: &str, found: bool, file: bool) -> &'static str {
    match (query.is_empty(), found) {
        (true, _) if file => "Search the file",
        (true, _) => "Search the history",
        (false, true) => "",
        (false, false) => "No match",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regex_characters_match_themselves() {
        assert_eq!(pattern("plain words"), "plain words");
        assert_eq!(pattern("a.b*c"), r"a\.b\*c");
        assert_eq!(pattern(r"C:\x (1)"), r"C:\\x \(1\)");
        assert_eq!(pattern("[x]{2}^$|?+"), r"\[x\]\{2\}\^\$\|\?\+");
    }

    #[test]
    fn the_bar_says_what_it_found() {
        assert_eq!(status("", false, false), "Search the history");
        assert_eq!(status("", false, true), "Search the file");
        assert_eq!(status("x", true, true), "");
        assert_eq!(status("x", false, false), "No match");
    }
}
