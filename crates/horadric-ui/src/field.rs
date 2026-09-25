//! The text in an input field and what the keys do to it: a caret, a
//! selection, and moving by character, word and line. Positions are byte
//! offsets on character boundaries; DirectWrite counts in UTF-16, so
//! [`utf16_at`] and [`byte_at`] turn one into the other.

/// One field's text, with the caret and the other end of the selection.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub text: String,
    pub caret: usize,
    /// Where the selection started. Equal to `caret` when nothing is
    /// selected.
    pub anchor: usize,
    /// Whether Enter with Shift makes a new line. A single line field turns
    /// every line break it is given into a space.
    pub multiline: bool,
    /// In characters. Past it, typing and pasting are cut short.
    pub max: usize,
}

impl Field {
    /// A field holding `text`, all of it selected, so typing replaces it
    /// and the arrows keep it.
    pub fn new(text: &str, multiline: bool, max: usize) -> Self {
        let mut f = Field {
            text: String::new(),
            caret: 0,
            anchor: 0,
            multiline,
            max,
        };
        f.insert(text);
        f.anchor = 0;
        f
    }

    /// The selection, start first.
    pub fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    pub fn selected(&self) -> &str {
        let (a, b) = self.selection();
        &self.text[a..b]
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    /// Puts the caret at `at`, carrying the selection along when `extend`.
    pub fn set_caret(&mut self, at: usize, extend: bool) {
        self.caret = floor_boundary(&self.text, at);
        if !extend {
            self.anchor = self.caret;
        }
    }

    /// Types or pastes `s` over the selection, fitted to the field.
    pub fn insert(&mut self, s: &str) {
        let (a, b) = self.selection();
        let room = self
            .max
            .saturating_sub(self.text.chars().count() - self.text[a..b].chars().count());
        let s = clean(s, self.multiline);
        let s: String = s.chars().take(room).collect();
        self.text.replace_range(a..b, &s);
        self.caret = a + s.len();
        self.anchor = self.caret;
    }

    /// Takes the selection out, for a cut.
    pub fn cut(&mut self) -> String {
        let taken = self.selected().to_string();
        self.insert("");
        taken
    }

    /// Backspace: the selection, else the character or word before the
    /// caret.
    pub fn backspace(&mut self, word: bool) {
        if self.caret == self.anchor {
            self.anchor = if word {
                word_before(&self.text, self.caret)
            } else {
                char_before(&self.text, self.caret)
            };
        }
        self.insert("");
    }

    /// Delete: the selection, else the character or word after the caret.
    pub fn delete(&mut self, word: bool) {
        if self.caret == self.anchor {
            self.anchor = if word {
                word_after(&self.text, self.caret)
            } else {
                char_after(&self.text, self.caret)
            };
        }
        self.insert("");
    }

    /// Left arrow. With a selection and no Shift, the caret goes to its
    /// start, as in any Windows edit box.
    pub fn left(&mut self, word: bool, extend: bool) {
        let at = if !extend && self.caret != self.anchor && !word {
            self.selection().0
        } else if word {
            word_before(&self.text, self.caret)
        } else {
            char_before(&self.text, self.caret)
        };
        self.set_caret(at, extend);
    }

    pub fn right(&mut self, word: bool, extend: bool) {
        let at = if !extend && self.caret != self.anchor && !word {
            self.selection().1
        } else if word {
            word_after(&self.text, self.caret)
        } else {
            char_after(&self.text, self.caret)
        };
        self.set_caret(at, extend);
    }

    /// Home: the start of the caret's line, or of all of it with Ctrl.
    pub fn home(&mut self, all: bool, extend: bool) {
        let at = if all {
            0
        } else {
            self.text[..self.caret].rfind('\n').map_or(0, |i| i + 1)
        };
        self.set_caret(at, extend);
    }

    pub fn end(&mut self, all: bool, extend: bool) {
        let at = if all {
            self.text.len()
        } else {
            self.text[self.caret..]
                .find('\n')
                .map_or(self.text.len(), |i| self.caret + i)
        };
        self.set_caret(at, extend);
    }

    /// The word under `at`, for a double click. Space between words
    /// selects the space.
    pub fn select_word(&mut self, at: usize) {
        let at = floor_boundary(&self.text, at);
        let space = |c: char| c.is_whitespace();
        let here = self.text[at..].chars().next();
        let kind = here.map(space).unwrap_or(false);
        let start = self.text[..at]
            .char_indices()
            .rev()
            .find(|&(_, c)| space(c) != kind)
            .map_or(0, |(i, c)| i + c.len_utf8());
        let end = self.text[at..]
            .char_indices()
            .find(|&(_, c)| space(c) != kind)
            .map_or(self.text.len(), |(i, _)| at + i);
        self.anchor = start;
        self.caret = end;
    }
}

/// `s` as a field takes it: Windows line breaks made one, tabs made a
/// space, and in a single line field every line break a space too. Other
/// control characters are dropped.
fn clean(s: &str, multiline: bool) -> String {
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    s.chars()
        .filter_map(|c| match c {
            '\n' if multiline => Some('\n'),
            '\n' | '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

fn floor_boundary(s: &str, mut at: usize) -> usize {
    at = at.min(s.len());
    while !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn char_before(s: &str, at: usize) -> usize {
    s[..at].char_indices().next_back().map_or(0, |(i, _)| i)
}

fn char_after(s: &str, at: usize) -> usize {
    s[at..].chars().next().map_or(at, |c| at + c.len_utf8())
}

/// Ctrl+Left: back over any space, then over the word.
fn word_before(s: &str, at: usize) -> usize {
    let head = s[..at].trim_end();
    head.char_indices()
        .rev()
        .find(|&(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8())
}

/// Ctrl+Right: over the word, then over the space after it, to the start
/// of the next.
fn word_after(s: &str, at: usize) -> usize {
    let rest = &s[at..];
    let word = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let space = rest[word..]
        .find(|c: char| !c.is_whitespace())
        .unwrap_or(rest.len() - word);
    at + word + space
}

/// The UTF-16 offset of byte offset `at`.
pub fn utf16_at(s: &str, at: usize) -> u32 {
    s[..floor_boundary(s, at)].encode_utf16().count() as u32
}

/// The byte offset of UTF-16 offset `at`. Half a surrogate pair rounds
/// down to the character.
pub fn byte_at(s: &str, at: u32) -> usize {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16() as u32;
        if units > at {
            return i;
        }
    }
    s.len()
}

/// Where a view `view` long scrolled by `scroll` must scroll to so the
/// span `at` to `at + extent` shows, over content `content` long. It moves
/// only as far as it has to, and never past the end of the content.
pub fn follow(scroll: f32, at: f32, extent: f32, view: f32, content: f32) -> f32 {
    let mut s = scroll;
    if at + extent > s + view {
        s = at + extent - view;
    }
    if at < s {
        s = at;
    }
    s.min((content - view).max(0.0)).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(text: &str) -> Field {
        let mut f = Field::new(text, false, 200);
        f.set_caret(text.len(), false);
        f
    }

    #[test]
    fn a_new_field_selects_its_text_so_typing_replaces_it() {
        let mut f = Field::new("old name", false, 200);
        assert_eq!(f.selected(), "old name");
        f.insert("new");
        assert_eq!(f.text, "new");
        assert_eq!(f.caret, 3);
    }

    #[test]
    fn a_single_line_field_makes_line_breaks_spaces() {
        let mut f = field("");
        f.insert("one\r\ntwo\tthree\u{7}");
        assert_eq!(f.text, "one two three");
        let mut m = Field::new("", true, 200);
        m.insert("one\r\ntwo\rthree");
        assert_eq!(m.text, "one\ntwo\nthree");
    }

    #[test]
    fn typing_stops_at_the_limit_counting_what_it_replaces() {
        let mut f = Field::new("abcd", false, 5);
        f.insert("xyzuvw");
        assert_eq!(f.text, "xyzuv");
        f.insert("q");
        assert_eq!(f.text, "xyzuv");
        f.select_all();
        f.insert("ééééééé");
        assert_eq!(f.text.chars().count(), 5);
    }

    #[test]
    fn backspace_and_delete_take_a_character_a_word_or_the_selection() {
        let mut f = field("hello big world");
        f.backspace(false);
        assert_eq!(f.text, "hello big worl");
        f.backspace(true);
        assert_eq!(f.text, "hello big ");
        f.backspace(true);
        assert_eq!(f.text, "hello ");
        f.home(true, false);
        f.delete(true);
        assert_eq!(f.text, "");
        let mut f = field("abc");
        f.set_caret(1, false);
        f.set_caret(3, true);
        f.delete(false);
        assert_eq!(f.text, "a");
    }

    #[test]
    fn the_caret_steps_over_whole_characters() {
        let mut f = field("aé😀b");
        f.left(false, false);
        f.left(false, false);
        assert_eq!(&f.text[f.caret..], "😀b");
        f.backspace(false);
        assert_eq!(f.text, "a😀b");
        f.right(false, true);
        assert_eq!(f.selected(), "😀");
    }

    #[test]
    fn an_arrow_without_shift_collapses_the_selection_to_its_side() {
        let mut f = field("one two");
        f.set_caret(1, false);
        f.set_caret(5, true);
        f.left(false, false);
        assert_eq!((f.caret, f.anchor), (1, 1));
        f.set_caret(5, true);
        f.right(false, false);
        assert_eq!((f.caret, f.anchor), (5, 5));
    }

    #[test]
    fn ctrl_arrows_jump_to_the_starts_of_words() {
        let mut f = field("one  two three");
        f.left(true, false);
        assert_eq!(f.caret, 9);
        f.left(true, false);
        assert_eq!(f.caret, 5);
        f.left(true, false);
        assert_eq!(f.caret, 0);
        f.right(true, false);
        assert_eq!(f.caret, 5);
        f.right(true, true);
        assert_eq!(f.selected(), "two ");
    }

    #[test]
    fn home_and_end_go_to_the_line_or_with_ctrl_to_the_whole() {
        let mut f = Field::new("first\nsecond\nthird", true, 200);
        f.set_caret(8, false);
        f.home(false, false);
        assert_eq!(f.caret, 6);
        f.end(false, true);
        assert_eq!(f.selected(), "second");
        f.end(true, false);
        assert_eq!(f.caret, f.text.len());
        f.home(true, false);
        assert_eq!(f.caret, 0);
    }

    #[test]
    fn a_double_click_selects_the_word_under_it() {
        let mut f = field("say hello there");
        f.select_word(6);
        assert_eq!(f.selected(), "hello");
        f.select_word(3);
        assert_eq!(f.selected(), " ");
        f.select_word(15);
        assert_eq!(f.selected(), "there");
    }

    #[test]
    fn a_cut_returns_the_selection_and_takes_it_out() {
        let mut f = field("keep this");
        f.set_caret(4, true);
        assert_eq!(f.cut(), " this");
        assert_eq!(f.text, "keep");
    }

    #[test]
    fn offsets_go_between_bytes_and_utf16() {
        let s = "aé😀b";
        assert_eq!(utf16_at(s, 0), 0);
        assert_eq!(utf16_at(s, 3), 2);
        assert_eq!(utf16_at(s, 7), 4);
        assert_eq!(byte_at(s, 2), 3);
        assert_eq!(byte_at(s, 3), 3, "half a pair rounds down");
        assert_eq!(byte_at(s, 4), 7);
        assert_eq!(byte_at(s, 99), s.len());
    }

    #[test]
    fn the_view_scrolls_only_as_far_as_the_caret_needs() {
        // Fits: no scroll.
        assert_eq!(follow(0.0, 50.0, 1.0, 100.0, 80.0), 0.0);
        // Past the right edge: just enough to show it.
        assert_eq!(follow(0.0, 150.0, 1.0, 100.0, 151.0), 51.0);
        // Back left of the view: to it.
        assert_eq!(follow(51.0, 20.0, 1.0, 100.0, 151.0), 20.0);
        // Text deleted from the end: the scroll comes back.
        assert_eq!(follow(51.0, 60.0, 1.0, 100.0, 61.0), 0.0);
    }
}
