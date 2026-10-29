//! The composer's text as it is edited, without any window: undo and redo
//! over a [`Field`], big pastes folded into a chip that opens again on
//! send, and the prompts sent before, walked with Up and Down.
//!
//! A folded paste is a token in the text, `[Pasted 120 lines #1]`, with its
//! whole text kept aside. The token is plain text, so the caret, selection
//! and layout need nothing new; edited by hand into something else it is
//! just text again.

use crate::field::Field;

/// Past this many characters, a paste folds.
const FOLD_CHARS: usize = 800;
/// Past this many lines, a paste folds.
const FOLD_LINES: usize = 15;
const UNDO_MAX: usize = 200;
/// The prompts history keeps, newest last.
pub const HISTORY_MAX: usize = 200;
/// In characters, as in the CLI's own prompt.
pub const MAX: usize = 1_000_000;

#[derive(Debug, Clone, PartialEq)]
struct Snap {
    text: String,
    caret: usize,
    anchor: usize,
}

/// What an edit was, so a run of typing undoes as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Type,
    Delete,
    Other,
}

#[derive(Debug, Clone)]
pub struct Editor {
    pub field: Field,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    last: Kind,
    /// Folded pastes, by their number less one.
    pastes: Vec<String>,
}

impl Default for Editor {
    fn default() -> Self {
        Editor::new("")
    }
}

impl Editor {
    pub fn new(text: &str) -> Editor {
        let mut field = Field::new(text, true, MAX);
        field.set_caret(field.text.len(), false);
        Editor {
            field,
            undo: Vec::new(),
            redo: Vec::new(),
            last: Kind::Other,
            pastes: Vec::new(),
        }
    }

    /// An editor holding a draft kept from before, its folded pastes with
    /// it.
    pub fn from_parts(text: &str, pastes: Vec<String>) -> Editor {
        let mut e = Editor::new(text);
        e.pastes = pastes;
        e
    }

    /// The folded pastes, for keeping a draft.
    pub fn pastes(&self) -> &[String] {
        &self.pastes
    }

    pub fn text(&self) -> &str {
        &self.field.text
    }

    pub fn is_empty(&self) -> bool {
        self.field.text.is_empty()
    }

    fn snap(&self) -> Snap {
        Snap {
            text: self.field.text.clone(),
            caret: self.field.caret,
            anchor: self.field.anchor,
        }
    }

    fn restore(&mut self, s: Snap) {
        self.field.text = s.text;
        self.field.caret = s.caret;
        self.field.anchor = s.anchor;
    }

    /// Runs an edit, keeping what was before it for undo. Typing after
    /// typing joins the step before, until a space or a line break.
    fn edit(&mut self, kind: Kind, f: impl FnOnce(&mut Field)) {
        let before = self.snap();
        f(&mut self.field);
        if self.field.text == before.text {
            return;
        }
        let joins = kind != Kind::Other && kind == self.last && !self.undo.is_empty();
        if !joins {
            self.undo.push(before);
            if self.undo.len() > UNDO_MAX {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last = kind;
    }

    /// A key typed.
    pub fn type_str(&mut self, s: &str) {
        // A word ends at a space, so undo takes back a word at a time.
        let breaks = s.chars().any(char::is_whitespace);
        self.edit(Kind::Type, |f| f.insert(s));
        if breaks {
            self.last = Kind::Other;
        }
    }

    /// Pasted text: as it is, or folded into a chip when big.
    pub fn paste(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n");
        let token = fold(&s).map(|summary| {
            self.pastes.push(s.clone());
            format!("[Pasted {summary} #{}]", self.pastes.len())
        });
        let put = token.as_deref().unwrap_or(&s);
        self.edit(Kind::Other, |f| f.insert(put));
    }

    /// Text put in as one step, such as a picked command or file.
    pub fn insert(&mut self, s: &str) {
        self.edit(Kind::Other, |f| f.insert(s));
    }

    /// Replaces the bytes `a..b` with `s`, the caret after it.
    pub fn replace(&mut self, a: usize, b: usize, s: &str) {
        self.edit(Kind::Other, |f| {
            f.anchor = a.min(f.text.len());
            f.caret = b.min(f.text.len());
            f.insert(s);
        });
    }

    /// Backspace. Just after a chip it takes the whole chip.
    pub fn backspace(&mut self, word: bool) {
        let f = &self.field;
        if f.caret == f.anchor {
            if let Some((a, _)) = self.chips().into_iter().find(|&(_, b)| b == f.caret) {
                let at = f.caret;
                self.edit(Kind::Other, |f| {
                    f.anchor = a;
                    f.caret = at;
                    f.insert("");
                });
                return;
            }
        }
        self.edit(Kind::Delete, |f| f.backspace(word));
    }

    /// Delete. Just before a chip it takes the whole chip.
    pub fn delete(&mut self, word: bool) {
        let f = &self.field;
        if f.caret == f.anchor {
            if let Some((_, b)) = self.chips().into_iter().find(|&(a, _)| a == f.caret) {
                self.edit(Kind::Other, |f| {
                    f.anchor = b;
                    f.insert("");
                });
                return;
            }
        }
        self.edit(Kind::Delete, |f| f.delete(word));
    }

    pub fn cut(&mut self) -> String {
        let taken = self.field.selected().to_string();
        self.edit(Kind::Other, |f| f.insert(""));
        taken
    }

    /// Everything replaced at once, as history recalled, kept for undo.
    pub fn set_text(&mut self, s: &str) {
        self.edit(Kind::Other, |f| {
            f.select_all();
            f.insert(s);
        });
    }

    pub fn undo(&mut self) -> bool {
        let Some(s) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.snap());
        self.restore(s);
        self.last = Kind::Other;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(s) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.snap());
        self.restore(s);
        self.last = Kind::Other;
        true
    }

    /// The caret moved without an edit: the next typing is a step of its
    /// own.
    pub fn moved(&mut self) {
        self.last = Kind::Other;
    }

    /// Byte ranges of the folded pastes still whole in the text.
    pub fn chips(&self) -> Vec<(usize, usize)> {
        chips_in(&self.field.text, self.pastes.len())
    }

    /// The text with each chip opened back into what was pasted: what is
    /// sent, and what history keeps.
    pub fn expanded(&self) -> String {
        let text = &self.field.text;
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        for (a, b) in self.chips() {
            out.push_str(&text[at..a]);
            match number_of(&text[a..b]).and_then(|n| self.pastes.get(n - 1)) {
                Some(p) => out.push_str(p),
                None => out.push_str(&text[a..b]),
            }
            at = b;
        }
        out.push_str(&text[at..]);
        out
    }

    /// Empties it after a send. Undo can bring the text back.
    pub fn clear(&mut self) {
        self.set_text("");
        self.pastes.clear();
    }

    /// The word the caret is in, from its start: `(start, word)`. A word
    /// runs back to whitespace or the start of the text.
    pub fn word_at_caret(&self) -> (usize, &str) {
        let f = &self.field;
        let head = &f.text[..f.caret];
        let start = head
            .char_indices()
            .rev()
            .find(|&(_, c)| c.is_whitespace())
            .map_or(0, |(i, c)| i + c.len_utf8());
        (start, &f.text[start..f.caret])
    }
}

/// How a paste is named when it folds, or None when it is small enough
/// to show.
fn fold(s: &str) -> Option<String> {
    let lines = s.trim_end_matches('\n').split('\n').count();
    if lines > FOLD_LINES {
        return Some(format!("{lines} lines"));
    }
    let chars = s.chars().count();
    (chars > FOLD_CHARS).then(|| format!("{chars} chars"))
}

fn number_of(token: &str) -> Option<usize> {
    let n = token.strip_suffix(']')?.rsplit_once(" #")?.1;
    n.parse().ok()
}

/// The `[Pasted ... #n]` tokens in `text` whose number is one of the `n`
/// pastes kept.
fn chips_in(text: &str, n: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    if n == 0 {
        return out;
    }
    let mut from = 0;
    while let Some(i) = text[from..].find("[Pasted ") {
        let a = from + i;
        let Some(j) = text[a..].find(']') else {
            break;
        };
        let b = a + j + 1;
        let token = &text[a..b];
        let ok = !token[1..].contains('[')
            && !token.contains('\n')
            && number_of(token).is_some_and(|k| (1..=n).contains(&k));
        if ok {
            out.push((a, b));
            from = b;
        } else {
            from = a + 1;
        }
    }
    out
}

/// The prompts sent before, walked from the newest with Up. What was being
/// written is kept while walking, and comes back past the newest.
#[derive(Debug, Clone, Default)]
pub struct History {
    pub entries: Vec<String>,
    /// The entry shown, None while writing.
    at: Option<usize>,
    draft: String,
}

impl History {
    pub fn new(entries: Vec<String>) -> History {
        History {
            entries,
            ..Default::default()
        }
    }

    /// A prompt sent. The same one twice in a row is kept once.
    pub fn push(&mut self, s: &str) {
        self.at = None;
        if s.trim().is_empty() || self.entries.last().is_some_and(|l| l == s) {
            return;
        }
        self.entries.push(s.to_string());
        if self.entries.len() > HISTORY_MAX {
            self.entries.remove(0);
        }
    }

    /// Up: the entry before, given what the composer holds now. None at
    /// the oldest.
    pub fn older(&mut self, now: &str) -> Option<String> {
        let next = match self.at {
            None => {
                self.draft = now.to_string();
                self.entries.len().checked_sub(1)?
            }
            Some(0) => return None,
            Some(i) => i - 1,
        };
        self.at = Some(next);
        Some(self.entries[next].clone())
    }

    /// Down: the entry after, or what was being written past the newest.
    /// None while writing.
    pub fn newer(&mut self) -> Option<String> {
        let i = self.at?;
        if i + 1 < self.entries.len() {
            self.at = Some(i + 1);
            Some(self.entries[i + 1].clone())
        } else {
            self.at = None;
            Some(std::mem::take(&mut self.draft))
        }
    }

    /// Whether Up and Down walk it now rather than move the caret.
    pub fn walking(&self) -> bool {
        self.at.is_some()
    }

    /// Typing leaves the walk where it is.
    pub fn stop(&mut self) {
        self.at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_undoes_a_word_at_a_time() {
        let mut e = Editor::new("");
        for c in "hello world".chars() {
            e.type_str(&c.to_string());
        }
        assert!(e.undo());
        assert_eq!(e.text(), "hello ");
        assert!(e.undo());
        assert_eq!(e.text(), "");
        assert!(!e.undo());
        assert!(e.redo());
        assert_eq!(e.text(), "hello ");
    }

    #[test]
    fn an_edit_after_undo_drops_the_redo() {
        let mut e = Editor::new("");
        e.insert("a");
        e.insert("b");
        e.undo();
        e.insert("c");
        assert!(!e.redo());
        assert_eq!(e.text(), "ac");
    }

    #[test]
    fn a_small_paste_goes_in_as_it_is() {
        let mut e = Editor::new("");
        e.paste("one\r\ntwo");
        assert_eq!(e.text(), "one\ntwo");
        assert!(e.chips().is_empty());
    }

    #[test]
    fn a_big_paste_folds_and_opens_on_send() {
        let mut e = Editor::new("look: ");
        let big: String = (0..120).map(|i| format!("line {i}\n")).collect();
        e.paste(&big);
        e.type_str(" thanks");
        assert_eq!(e.text(), "look: [Pasted 120 lines #1] thanks");
        assert_eq!(e.chips(), vec![(6, 27)]);
        assert_eq!(e.expanded(), format!("look: {big} thanks"));
    }

    #[test]
    fn a_long_line_folds_by_its_length() {
        let mut e = Editor::new("");
        e.paste(&"x".repeat(900));
        assert_eq!(e.text(), "[Pasted 900 chars #1]");
    }

    #[test]
    fn backspace_after_a_chip_takes_it_whole() {
        let mut e = Editor::new("a ");
        e.paste(&"y\n".repeat(20));
        e.backspace(false);
        assert_eq!(e.text(), "a ");
        assert!(e.undo());
        assert_eq!(e.chips().len(), 1);
    }

    #[test]
    fn delete_before_a_chip_takes_it_whole() {
        let mut e = Editor::new("");
        e.paste(&"y\n".repeat(20));
        e.type_str("!");
        e.field.set_caret(0, false);
        e.delete(false);
        assert_eq!(e.text(), "!");
    }

    #[test]
    fn a_chip_edited_by_hand_is_text_again() {
        let mut e = Editor::new("");
        e.paste(&"y\n".repeat(20));
        e.field.set_caret(3, false);
        e.type_str("[");
        assert!(e.chips().is_empty());
        assert!(e.expanded().starts_with("[Pa["));
    }

    #[test]
    fn a_token_typed_with_no_paste_behind_it_is_text() {
        let e = Editor::new("[Pasted 3 lines #1]");
        assert!(e.chips().is_empty());
        assert_eq!(e.expanded(), "[Pasted 3 lines #1]");
    }

    #[test]
    fn the_word_at_the_caret() {
        let mut e = Editor::new("see @src/ma");
        assert_eq!(e.word_at_caret(), (4, "@src/ma"));
        e.field.set_caret(3, false);
        assert_eq!(e.word_at_caret(), (0, "see"));
    }

    #[test]
    fn history_walks_back_and_returns_the_draft() {
        let mut h = History::new(vec!["one".into(), "two".into()]);
        assert_eq!(h.older("draft").as_deref(), Some("two"));
        assert_eq!(h.older("two").as_deref(), Some("one"));
        assert_eq!(h.older("one"), None);
        assert_eq!(h.newer().as_deref(), Some("two"));
        assert_eq!(h.newer().as_deref(), Some("draft"));
        assert_eq!(h.newer(), None);
        assert!(!h.walking());
    }

    #[test]
    fn history_keeps_a_repeat_once_and_skips_blanks() {
        let mut h = History::default();
        h.push("a");
        h.push("a");
        h.push("  ");
        h.push("b");
        assert_eq!(h.entries, vec!["a", "b"]);
    }

    #[test]
    fn empty_history_has_nothing_older() {
        let mut h = History::default();
        assert_eq!(h.older("x"), None);
        assert!(!h.walking());
    }
}
