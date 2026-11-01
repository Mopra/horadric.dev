//! Words dictation should know: a personal dictionary, as Wispr Flow has,
//! applied to what Whisper wrote. A prompt asks Whisper nicely and did not
//! fix "pains" or "horridrick"; a list of what it hears and what was
//! meant fixes them every time.
//!
//! The list is a text file the human edits:
//!
//! ```text
//! horridrick => Horadric
//! Folio
//! [horadric.dev]
//! pains => panes
//! ```
//!
//! A line with `=>` (or `->`) puts the words on the right where those on
//! the left were heard, as whole words whatever their case. A line of
//! words alone spells them that way, whatever case Whisper gave them. A
//! `[folder]` line makes the lines under it count only in the project of
//! that folder's name, where "pains" may well be "panes" and elsewhere not.

/// Written into a new list, so the file explains itself when opened.
pub const HEADER: &str = "\
# Words dictation should know, one a line:
#   pains => panes     what it wrote => what you meant
#   Horadric           spelled so, whatever case it heard
# Lines under [folder] count only in the project of that folder's name.
# Lines starting with # are ignored. Saved, it applies from the next dictation.

";

/// One fix: the words heard, lower case, and what is written instead.
#[derive(Debug, Clone, PartialEq)]
struct Fix {
    heard: Vec<char>,
    meant: String,
}

/// The fixes that count in one project, longest first, so "horrid rick"
/// is tried before "horrid".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Words {
    fixes: Vec<Fix>,
}

fn lower(s: &str) -> Vec<char> {
    s.chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect()
}

/// An apostrophe ends a word here, so "Folio" fixes "folio's" too.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Words {
    /// The fixes in `list` that count in the project whose folder is named
    /// `project`: those above any `[folder]` line, and those under its own.
    pub fn parse(list: &str, project: Option<&str>) -> Words {
        let mut fixes = Vec::new();
        let mut counts = true;
        for line in list.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                counts = project.is_some_and(|p| p.eq_ignore_ascii_case(name.trim()));
                continue;
            }
            if !counts {
                continue;
            }
            let (heard, meant) = match line.split_once("=>").or_else(|| line.split_once("->")) {
                Some((h, m)) => (h.trim(), m.trim()),
                None => (line, line),
            };
            let heard = lower(&heard.split_whitespace().collect::<Vec<_>>().join(" "));
            if heard.is_empty() {
                continue;
            }
            fixes.push(Fix {
                heard,
                meant: meant.to_string(),
            });
        }
        fixes.sort_by_key(|f| std::cmp::Reverse(f.heard.len()));
        Words { fixes }
    }

    /// The list in the file at `path` for `project`, none when there is
    /// no file yet.
    pub fn load(path: &std::path::Path, project: Option<&str>) -> Words {
        std::fs::read_to_string(path)
            .map(|list| Words::parse(&list, project))
            .unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.fixes.is_empty()
    }

    /// `text` with every fix made. A fix at the start of a sentence, heard
    /// with a capital, keeps it: "Pains" becomes "Panes".
    pub fn apply(&self, text: &str) -> String {
        if self.fixes.is_empty() {
            return text.to_string();
        }
        let chars: Vec<char> = text.chars().collect();
        let low: Vec<char> = chars
            .iter()
            .map(|c| c.to_lowercase().next().unwrap_or(*c))
            .collect();
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        while i < chars.len() {
            let starts_word = is_word(chars[i]) && (i == 0 || !is_word(chars[i - 1]));
            let fix = starts_word
                .then(|| {
                    self.fixes.iter().find(|f| {
                        let end = i + f.heard.len();
                        low.get(i..end).is_some_and(|s| s == f.heard.as_slice())
                            && chars.get(end).is_none_or(|c| !is_word(*c))
                    })
                })
                .flatten();
            match fix {
                Some(f) => {
                    let mut meant = f.meant.chars();
                    if chars[i].is_uppercase() {
                        if let Some(first) = meant.next() {
                            out.extend(first.to_uppercase());
                        }
                    }
                    out.extend(meant);
                    i += f.heard.len();
                }
                None => {
                    out.push(chars[i]);
                    i += 1;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = "\
# a comment
horridrick => Horadric
horrid rick -> Horadric
Folio
[horadric.dev]
pains => panes
[elsewhere]
tiles => files
";

    #[test]
    fn a_fix_is_made_on_whole_words_whatever_their_case() {
        let w = Words::parse(LIST, Some("horadric.dev"));
        assert_eq!(
            w.apply("Pains in horridrick, and the folio's spains."),
            "Panes in Horadric, and the Folio's spains."
        );
        assert_eq!(w.apply("I said horrid rick."), "I said Horadric.");
    }

    #[test]
    fn project_lines_count_only_in_their_project() {
        let here = Words::parse(LIST, Some("horadric.dev"));
        assert_eq!(here.apply("the tiles"), "the tiles");
        let there = Words::parse(LIST, Some("Elsewhere"));
        assert_eq!(there.apply("pains and tiles"), "pains and files");
        let none = Words::parse(LIST, None);
        assert_eq!(none.apply("pains, horridrick"), "pains, Horadric");
    }

    #[test]
    fn the_header_alone_fixes_nothing() {
        assert!(Words::parse(HEADER, Some("x")).is_empty());
        assert_eq!(Words::default().apply("as is"), "as is");
    }
}
