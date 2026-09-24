//! Which waiting session the hotkey brings up next.
//!
//! Pressing it again without answering moves on to the next one, so a
//! session you are not ready for does not block the rest.

/// The session after `current` in `waiting`, oldest wait first, wrapping
/// round. The oldest when `current` is not waiting, or nothing is shown.
pub fn next<'a>(waiting: &'a [String], current: Option<&str>) -> Option<&'a str> {
    let at = current.and_then(|c| waiting.iter().position(|w| w == c));
    let i = at.map_or(0, |i| (i + 1) % waiting.len());
    waiting.get(i).map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn nothing_waiting_is_nothing() {
        assert_eq!(next(&[], Some("a")), None);
        assert_eq!(next(&[], None), None);
    }

    #[test]
    fn starts_with_the_oldest() {
        let w = ids(&["old", "new"]);
        assert_eq!(next(&w, None), Some("old"));
        assert_eq!(next(&w, Some("working-one")), Some("old"));
    }

    #[test]
    fn moves_on_and_wraps() {
        let w = ids(&["a", "b", "c"]);
        assert_eq!(next(&w, Some("a")), Some("b"));
        assert_eq!(next(&w, Some("c")), Some("a"));
        assert_eq!(next(&ids(&["a"]), Some("a")), Some("a"));
    }
}
