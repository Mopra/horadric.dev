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
    /// Set for a field that must give back exactly what it was given, a
    /// file's text to change: see [`Field::verbatim`].
    pub verbatim: Option<Verbatim>,
    history: History,
}

/// What a verbatim field was given, so [`Field::value`] can hand an
/// unedited text back byte for byte and an edited one with its line breaks.
#[derive(Debug, Clone, PartialEq)]
pub struct Verbatim {
    original: String,
    crlf: bool,
}

/// Each change to the text, for Ctrl+Z and Ctrl+Y. A change is kept as what
/// it took out and put in, not a copy of the text, since a field can hold a
/// whole file.
#[derive(Debug, Clone, PartialEq, Default)]
struct History {
    undo: Vec<Edit>,
    redo: Vec<Edit>,
}

#[derive(Debug, Clone, PartialEq)]
struct Edit {
    at: usize,
    removed: String,
    inserted: String,
    /// The caret and anchor before it, where an undo puts them back.
    caret: usize,
    anchor: usize,
}

const UNDO_DEPTH: usize = 500;

impl History {
    fn record(&mut self, e: Edit) {
        self.redo.clear();
        if let Some(last) = self.undo.last_mut() {
            // Typing a run, or holding Backspace or Delete, undoes as one.
            let typed = |x: &Edit| x.removed.is_empty() && x.inserted.chars().count() == 1;
            let erased = |x: &Edit| x.inserted.is_empty() && !x.removed.is_empty();
            if typed(&e)
                && !last.inserted.is_empty()
                && last.removed.is_empty()
                && e.at == last.at + last.inserted.len()
                && e.inserted != "\n"
                && !last.inserted.ends_with('\n')
            {
                last.inserted.push_str(&e.inserted);
                return;
            }
            if erased(&e) && erased(last) && e.at + e.removed.len() == last.at {
                last.removed.insert_str(0, &e.removed);
                last.at = e.at;
                return;
            }
            if erased(&e) && erased(last) && e.at == last.at {
                last.removed.push_str(&e.removed);
                return;
            }
        }
        if self.undo.len() == UNDO_DEPTH {
            self.undo.remove(0);
        }
        self.undo.push(e);
    }
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
            verbatim: None,
            history: History::default(),
        };
        f.insert(text);
        f.anchor = 0;
        f.history = History::default();
        f
    }

    /// A multiline field for text that must come back exactly as given, a
    /// file Claude is about to write: tabs and control characters are kept
    /// and [`Field::value`] restores the line breaks. None when `text` is
    /// over `max`, since cutting it would cut the file. The caret starts at
    /// the top with nothing selected, so a stray key cannot replace it all.
    pub fn verbatim(text: &str, max: usize) -> Option<Self> {
        if text.chars().count() > max {
            return None;
        }
        let breaks = text.matches('\n').count();
        let crlf = breaks > 0 && text.matches("\r\n").count() * 2 > breaks;
        let mut f = Field {
            text: String::new(),
            caret: 0,
            anchor: 0,
            multiline: true,
            max,
            verbatim: Some(Verbatim {
                original: text.to_string(),
                crlf,
            }),
            history: History::default(),
        };
        f.insert(text);
        f.set_caret(0, false);
        f.history = History::default();
        Some(f)
    }

    /// What the field holds, for sending. A verbatim field gives back what
    /// it was given when it was not changed, and otherwise uses the line
    /// breaks most of the original used.
    pub fn value(&self) -> String {
        let Some(v) = &self.verbatim else {
            return self.text.clone();
        };
        if self.text == clean_verbatim(&v.original) {
            v.original.clone()
        } else if v.crlf {
            self.text.replace('\n', "\r\n")
        } else {
            self.text.clone()
        }
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

    /// How many characters typing or pasting over the selection may add.
    pub fn room(&self) -> usize {
        let (a, b) = self.selection();
        self.max
            .saturating_sub(self.text.chars().count() - self.text[a..b].chars().count())
    }

    /// Types or pastes `s` over the selection, fitted to the field. A
    /// verbatim field takes all of it or, when it would not fit, none.
    pub fn insert(&mut self, s: &str) {
        let (a, b) = self.selection();
        let room = self.room();
        let s: String = if self.verbatim.is_some() {
            let s = clean_verbatim(s);
            if s.chars().count() > room {
                return;
            }
            s
        } else {
            clean(s, self.multiline).chars().take(room).collect()
        };
        if a == b && s.is_empty() {
            return;
        }
        self.history.record(Edit {
            at: a,
            removed: self.text[a..b].to_string(),
            inserted: s.clone(),
            caret: self.caret,
            anchor: self.anchor,
        });
        self.text.replace_range(a..b, &s);
        self.caret = a + s.len();
        self.anchor = self.caret;
    }

    /// Ctrl+Z. False when there is nothing to undo.
    pub fn undo(&mut self) -> bool {
        let Some(e) = self.history.undo.pop() else {
            return false;
        };
        let end = e.at + e.inserted.len();
        // The text is public: one set from outside makes the history moot.
        if self.text.get(e.at..end) != Some(e.inserted.as_str()) {
            self.history = History::default();
            return false;
        }
        self.text.replace_range(e.at..end, &e.removed);
        self.caret = e.caret;
        self.anchor = e.anchor;
        self.history.redo.push(e);
        true
    }

    /// Ctrl+Y. False when there is nothing to redo.
    pub fn redo(&mut self) -> bool {
        let Some(e) = self.history.redo.pop() else {
            return false;
        };
        let end = e.at + e.removed.len();
        if self.text.get(e.at..end) != Some(e.removed.as_str()) {
            self.history = History::default();
            return false;
        }
        self.text.replace_range(e.at..end, &e.inserted);
        self.caret = e.at + e.inserted.len();
        self.anchor = self.caret;
        self.history.undo.push(e);
        true
    }

    /// Up or Down: the same column of the line before or after, or the
    /// start or end of the text from the first or last line. Lines are the
    /// text's, not the wrapped ones on screen.
    pub fn line(&mut self, down: bool, extend: bool) {
        let start = self.text[..self.caret].rfind('\n').map_or(0, |i| i + 1);
        let col = self.text[start..self.caret].chars().count();
        let target = if down {
            match self.text[self.caret..].find('\n') {
                Some(i) => self.caret + i + 1,
                None => return self.set_caret(self.text.len(), extend),
            }
        } else if start == 0 {
            return self.set_caret(0, extend);
        } else {
            self.text[..start - 1].rfind('\n').map_or(0, |i| i + 1)
        };
        let end = self.text[target..]
            .find('\n')
            .map_or(self.text.len(), |i| target + i);
        let at = self.text[target..end]
            .char_indices()
            .nth(col)
            .map_or(end, |(i, _)| target + i);
        self.set_caret(at, extend);
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

/// `s` as a verbatim field takes it: only the line breaks made one, so the
/// caret never stands inside a CR LF. [`Field::value`] puts them back.
fn clean_verbatim(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
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
    fn a_verbatim_field_gives_back_tabs_and_crlf_unchanged() {
        let make = "all:\r\n\tcc -o a a.c\r\n\r\nclean:\r\n\trm a\x0c\r\n";
        let f = Field::verbatim(make, 1000).unwrap();
        assert!(f.text.contains('\t'));
        assert!(!f.text.contains('\r'));
        assert_eq!(f.value(), make);
        assert_eq!((f.caret, f.anchor), (0, 0));
        // Mixed endings come back as they were when nothing changed.
        let mixed = "a\r\nb\nc\r";
        assert_eq!(Field::verbatim(mixed, 100).unwrap().value(), mixed);
    }

    #[test]
    fn an_edited_verbatim_field_keeps_the_original_line_breaks() {
        let mut f = Field::verbatim("one\r\n\ttwo\r\n", 1000).unwrap();
        f.end(false, false);
        f.insert("\n\tnew");
        assert_eq!(f.value(), "one\r\n\tnew\r\n\ttwo\r\n");
        let mut f = Field::verbatim("x\ny\n", 1000).unwrap();
        f.insert("\t");
        assert_eq!(f.value(), "\tx\ny\n");
    }

    #[test]
    fn a_verbatim_field_refuses_rather_than_cuts() {
        assert!(Field::verbatim("abcdef", 5).is_none());
        let mut f = Field::verbatim("abcd", 5).unwrap();
        f.insert("xy");
        assert_eq!(f.text, "abcd");
        f.insert("x");
        assert_eq!(f.text, "xabcd");
    }

    #[test]
    fn undo_and_redo_step_through_runs_of_typing_and_erasing() {
        let mut f = Field::verbatim("ab", 100).unwrap();
        f.end(true, false);
        for c in ["c", "d", "e"] {
            f.insert(c);
        }
        f.backspace(false);
        f.backspace(false);
        assert_eq!(f.text, "abc");
        assert!(f.undo());
        assert_eq!(f.text, "abcde");
        assert!(f.undo());
        assert_eq!((f.text.as_str(), f.caret), ("ab", 2));
        assert!(!f.undo());
        assert!(f.redo());
        assert_eq!(f.text, "abcde");
        f.insert("z");
        assert!(!f.redo(), "a new edit drops what could be redone");
        // Text set from outside: undo gives up rather than garbles.
        f.text = String::from("other");
        assert!(!f.undo());
        assert_eq!(f.text, "other");
    }

    #[test]
    fn up_and_down_keep_the_column_across_lines() {
        let mut f = Field::new("hello\nhi\nworld", true, 200);
        f.set_caret(4, false);
        f.line(true, false);
        assert_eq!(f.caret, 8, "past a short line's end, to its end");
        f.line(true, false);
        assert_eq!(f.caret, 11);
        f.line(true, true);
        assert_eq!(f.caret, f.text.len());
        f.set_caret(4, false);
        f.line(false, false);
        assert_eq!(f.caret, 0);
        let mut f = Field::new("ab\n\ncd", true, 200);
        f.set_caret(6, false);
        f.line(false, false);
        assert_eq!(f.caret, 3);
        f.line(false, false);
        assert_eq!(f.caret, 0);
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
