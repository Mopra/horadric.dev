//! Colours for the file viewer, from the TextMate grammars `syntect`
//! bundles, which are the kind VS Code colours with too. The theme is VS
//! Code's Dark+, written out here as scope rules rather than loaded from a
//! file, so there is nothing to ship.

use std::str::FromStr;
use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color, FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};
use syntect::parsing::{SyntaxReference, SyntaxSet};

use crate::viewer::{Span, Style, TEXT};

/// Extensions the bundled grammars lack, and the nearest one they have.
const ALIASES: [(&str, &str); 12] = [
    ("ts", "js"),
    ("tsx", "js"),
    ("mts", "js"),
    ("cts", "js"),
    ("jsx", "js"),
    ("mjs", "js"),
    ("cjs", "js"),
    ("jsonc", "json"),
    ("json5", "json"),
    ("scss", "css"),
    ("less", "css"),
    ("vue", "html"),
];

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(dark_plus)
}

/// The grammar for a file, by its extension, its whole name (`Makefile`)
/// or its first line (`#!/bin/sh`). None when nothing fits.
fn syntax_for(name: &str, first: &str) -> Option<&'static SyntaxReference> {
    let set = syntaxes();
    let ext = name
        .rsplit_once('.')
        .map_or("", |(_, e)| e)
        .to_ascii_lowercase();
    let ext = ALIASES
        .iter()
        .find(|(from, _)| *from == ext)
        .map_or(ext.as_str(), |(_, to)| to);
    set.find_syntax_by_extension(ext)
        .or_else(|| set.find_syntax_by_extension(name))
        .or_else(|| set.find_syntax_by_first_line(first))
        .filter(|s| s.name != "Plain Text")
}

/// Styles for every line of a file called `name`, or none when no grammar
/// fits it. Lines are as [`crate::viewer::lines`] made them.
pub fn highlight(name: &str, lines: &[String]) -> Vec<Vec<Span>> {
    let Some(syntax) = syntax_for(name, lines.first().map_or("", String::as_str)) else {
        return Vec::new();
    };
    let set = syntaxes();
    let mut h = HighlightLines::new(syntax, theme());
    let mut out = Vec::with_capacity(lines.len());
    let mut buf = String::new();
    for line in lines {
        buf.clear();
        buf.push_str(line);
        buf.push('\n');
        let Ok(pieces) = h.highlight_line(&buf, set) else {
            break;
        };
        let mut left = line.len();
        let spans = pieces
            .into_iter()
            .filter_map(|(style, text)| {
                let len = text.len().min(left);
                left -= len;
                (len > 0).then(|| Span {
                    len,
                    style: Style {
                        fg: [style.foreground.r, style.foreground.g, style.foreground.b],
                        bold: style.font_style.contains(FontStyle::BOLD),
                        italic: style.font_style.contains(FontStyle::ITALIC),
                    },
                })
            })
            .collect();
        out.push(spans);
    }
    out
}

/// VS Code's Dark+ theme, for the scope names the Sublime grammars use.
fn dark_plus() -> Theme {
    let rules: [(&str, u32, FontStyle); 29] = [
        ("comment", 0x6A9955, FontStyle::empty()),
        ("string", 0xCE9178, FontStyle::empty()),
        ("string.regexp", 0xD16969, FontStyle::empty()),
        ("constant.character.escape", 0xD7BA7D, FontStyle::empty()),
        ("constant.numeric", 0xB5CEA8, FontStyle::empty()),
        ("constant.language", 0x569CD6, FontStyle::empty()),
        (
            "constant.other, variable.other.constant, entity.name.constant",
            0x4FC1FF,
            FontStyle::empty(),
        ),
        ("keyword", 0x569CD6, FontStyle::empty()),
        (
            "keyword.control, keyword.other.use, keyword.other.import",
            0xC586C0,
            FontStyle::empty(),
        ),
        ("keyword.operator", 0xD4D4D4, FontStyle::empty()),
        (
            "keyword.operator.new, keyword.operator.expression, keyword.operator.word",
            0x569CD6,
            FontStyle::empty(),
        ),
        ("storage", 0x569CD6, FontStyle::empty()),
        (
            "entity.name.function, support.function, support.macro, entity.name.macro",
            0xDCDCAA,
            FontStyle::empty(),
        ),
        (
            "entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, \
             entity.name.trait, entity.name.interface, entity.name.impl, \
             entity.name.namespace, entity.name.module, entity.other.inherited-class, \
             support.type, support.class",
            0x4EC9B0,
            FontStyle::empty(),
        ),
        (
            "variable, variable.parameter, variable.other, support.variable, \
             entity.name.variable, meta.definition.variable.name",
            0x9CDCFE,
            FontStyle::empty(),
        ),
        ("variable.language", 0x569CD6, FontStyle::empty()),
        (
            "variable.function, meta.function-call variable.function",
            0xDCDCAA,
            FontStyle::empty(),
        ),
        ("entity.name.tag", 0x569CD6, FontStyle::empty()),
        ("entity.other.attribute-name", 0x9CDCFE, FontStyle::empty()),
        ("punctuation.definition.tag", 0x808080, FontStyle::empty()),
        (
            "support.type.property-name, meta.mapping.key string, \
             meta.structure.dictionary.key string",
            0x9CDCFE,
            FontStyle::empty(),
        ),
        (
            "markup.heading, entity.name.section",
            0x569CD6,
            FontStyle::BOLD,
        ),
        ("markup.bold", 0xD4D4D4, FontStyle::BOLD),
        ("markup.italic", 0xD4D4D4, FontStyle::ITALIC),
        (
            "markup.raw, markup.inline.raw, markup.raw.inline",
            0xCE9178,
            FontStyle::empty(),
        ),
        ("markup.quote", 0x6A9955, FontStyle::empty()),
        ("markup.inserted", 0xB5CEA8, FontStyle::empty()),
        (
            "markup.deleted, markup.changed, meta.diff.header, meta.diff.range",
            0xCE9178,
            FontStyle::empty(),
        ),
        ("invalid", 0xF44747, FontStyle::empty()),
    ];
    let color = |hex: u32| Color {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
        a: 0xFF,
    };
    let [r, g, b] = TEXT.fg;
    Theme {
        settings: ThemeSettings {
            foreground: Some(Color { r, g, b, a: 0xFF }),
            ..ThemeSettings::default()
        },
        scopes: rules
            .into_iter()
            .map(|(scope, hex, font)| ThemeItem {
                scope: ScopeSelectors::from_str(scope).expect("a valid scope selector"),
                style: StyleModifier {
                    foreground: Some(color(hex)),
                    background: None,
                    font_style: Some(font),
                },
            })
            .collect(),
        ..Theme::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// The colour of the first span that starts at `word` in the line.
    fn colour_of(line: &str, spans: &[Span], word: &str) -> [u8; 3] {
        let at = line.find(word).unwrap();
        let mut start = 0;
        for span in spans {
            if start <= at && at < start + span.len {
                return span.style.fg;
            }
            start += span.len;
        }
        panic!("{word} has no span");
    }

    #[test]
    fn rust_looks_like_dark_plus() {
        let l = s(&["// note", "fn main() { let x = \"hi\"; return 42; }"]);
        let h = highlight("main.rs", &l);
        assert_eq!(h.len(), 2);
        assert_eq!(colour_of(&l[0], &h[0], "note"), [0x6A, 0x99, 0x55]);
        assert_eq!(colour_of(&l[1], &h[1], "fn"), [0x56, 0x9C, 0xD6]);
        assert_eq!(colour_of(&l[1], &h[1], "main"), [0xDC, 0xDC, 0xAA]);
        assert_eq!(colour_of(&l[1], &h[1], "\"hi\""), [0xCE, 0x91, 0x78]);
        assert_eq!(colour_of(&l[1], &h[1], "return"), [0xC5, 0x86, 0xC0]);
        assert_eq!(colour_of(&l[1], &h[1], "42"), [0xB5, 0xCE, 0xA8]);
    }

    #[test]
    fn spans_cover_the_line_and_not_its_newline() {
        let l = s(&["let a = 1;", ""]);
        let h = highlight("a.js", &l);
        assert_eq!(h[0].iter().map(|s| s.len).sum::<usize>(), l[0].len());
        assert!(h[1].is_empty());
    }

    #[test]
    fn typescript_borrows_javascript() {
        let l = s(&["const x = 'a';"]);
        let h = highlight("app.tsx", &l);
        assert_eq!(colour_of(&l[0], &h[0], "'a'"), [0xCE, 0x91, 0x78]);
    }

    #[test]
    fn a_shebang_names_the_language() {
        let l = s(&["#!/bin/bash", "echo hi"]);
        assert_eq!(highlight("run", &l).len(), 2);
    }

    #[test]
    fn unknown_files_get_no_colours() {
        assert!(highlight("notes.xyz", &s(&["just text"])).is_empty());
        assert!(highlight("readme.txt", &s(&["just text"])).is_empty());
    }
}
