//! A fenced code block in a reply.

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use windows::Win32::Graphics::DirectWrite::{
    IDWriteTextLayout, DWRITE_LINE_METRICS, DWRITE_TEXT_ALIGNMENT_TRAILING,
    DWRITE_WORD_WRAPPING_NO_WRAP,
};

use super::rich::{colour_code, Ctx, Item, Scene, F};
use super::transcript::Act;
use super::view::Hit;
use crate::layout::Rect;
use crate::render;
use crate::theme::{self, Color};

/// A code block's padding, header and corner radius.
const CODE_PAD: f32 = 12.0;
const CODE_HEAD: f32 = 30.0;
const RADIUS: f32 = 8.0;
/// Room between the header and the first line, and under the last.
const BODY_TOP: f32 = 8.0;
const BODY_BOTTOM: f32 = 10.0;
/// The row under a folded block that shows the rest.
const FOOT: f32 = 28.0;

/// Blocks longer than this fold to [`FOLD_TO`] lines until asked.
const FOLD_OVER: usize = 30;
const FOLD_TO: usize = 15;
/// A snippet of fewer lines has nothing to count, so no numbers.
const NUMBER_FROM: usize = 4;
/// Columns a tab advances to, as editors show code; DirectWrite's own
/// default is four ems, nearly twice as wide.
const TAB: f32 = 4.0;
/// The header's Copy and Fold buttons.
const COPY_W: f32 = 72.0;
const FOLD_W: f32 = 60.0;

thread_local! {
    /// The blocks shown whole, by [`fold_key`]. The transcript sets it
    /// from its state before building, since a block's scene is built
    /// from its text alone.
    static OPEN: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// The fold keys among a transcript's opened things, for [`OPEN`].
pub(super) fn set_open(all: &HashSet<String>) {
    OPEN.with(|o| {
        let mut o = o.borrow_mut();
        o.clear();
        o.extend(all.iter().filter(|k| k.starts_with("code:")).cloned());
    });
}

/// What the opened code blocks are, as one number: a reply is built
/// again when it changes.
pub(super) fn open_print(all: &HashSet<String>) -> u64 {
    // Xor, so the set's order does not matter.
    (all.iter().filter(|k| k.starts_with("code:")))
        .map(|k| {
            let mut h = DefaultHasher::new();
            k.hash(&mut h);
            h.finish()
        })
        .fold(0, |a, b| a ^ b)
}

/// The key a block is opened by, from its text: the same code twice
/// opens together, which reads as one thing anyway.
pub(super) fn fold_key(text: &str) -> String {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    format!("code:{:016x}", h.finish())
}

/// Whether the block or section with fold key `key` has been toggled.
pub(super) fn is_open(key: &str) -> bool {
    OPEN.with(|o| o.borrow().contains(key))
}

/// What a block's header calls its language. Formulas and diagrams are
/// shown as their source, not drawn, so the header says what they are.
pub(super) fn label(lang: &str) -> &str {
    match lang {
        "math" | "latex" | "tex" | "katex" => "math",
        "mermaid" | "dot" | "graphviz" | "plantuml" => "diagram",
        l => l,
    }
}

/// A fence's info string: the language and a file name when it names
/// one, as ```rust title="src/x.rs" or ```rust:src/x.rs.
#[derive(Debug, PartialEq)]
pub(super) struct Fence {
    pub(super) lang: String,
    pub(super) file: Option<String>,
}

impl Fence {
    pub(super) fn parse(info: &str) -> Fence {
        let info = info.trim();
        let first = info.split_whitespace().next().unwrap_or("");
        let (lang, mut file) = match first.split_once(':') {
            Some((l, f)) if !f.is_empty() => (l, Some(f.to_string())),
            _ => (first, None),
        };
        for key in ["title=", "file=", "filename=", "name="] {
            if file.is_some() {
                break;
            }
            if let Some(at) = info.find(key) {
                let rest = &info[at + key.len()..];
                let v = match rest.chars().next() {
                    Some(q @ ('"' | '\'')) => rest[1..].split(q).next().unwrap_or(""),
                    _ => rest.split_whitespace().next().unwrap_or(""),
                };
                if !v.is_empty() {
                    file = Some(v.to_string());
                }
            }
        }
        let lang = lang.trim_start_matches('{').trim_start_matches('.');
        Fence {
            lang: lang.to_ascii_lowercase(),
            file,
        }
    }
}

/// How a block is drawn beyond its colours.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Style {
    /// Numbered once it is long enough.
    Plain,
    /// Rows tinted by their mark.
    Diff,
    /// A prompt before each command; `true` for PowerShell's.
    Shell(bool),
}

impl Style {
    pub(super) fn of(lang: &str, text: &str) -> Style {
        match lang {
            "diff" | "patch" | "udiff" => Style::Diff,
            "bash" | "sh" | "shell" | "zsh" | "fish" | "console" | "shell-session"
            | "sh-session" | "terminal" => Style::Shell(false),
            "powershell" | "ps1" | "ps" | "pwsh" | "posh" | "cmd" | "bat" | "batch" => {
                Style::Shell(true)
            }
            "" if looks_like_diff(text) => Style::Diff,
            _ => Style::Plain,
        }
    }
}

/// Whether unlabelled text reads as a diff: most lines marked, and a
/// line added or a hunk, so a plain list of `- ` items is not one.
pub(super) fn looks_like_diff(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let marked = (lines.iter())
        .filter(|l| l.starts_with('+') || l.starts_with('-') || l.starts_with("@@"))
        .count();
    let telling = lines
        .iter()
        .any(|l| l.starts_with('+') || l.starts_with("@@"));
    marked >= 2 && telling && 2 * marked >= lines.len()
}

/// What a diff line is, by its first characters.
fn diff_mark(line: &str) -> Option<char> {
    if line.starts_with("+++") || line.starts_with("---") {
        Some('f')
    } else if line.starts_with("@@") {
        Some('@')
    } else if line.starts_with('+') {
        Some('+')
    } else if line.starts_with('-') {
        Some('-')
    } else {
        None
    }
}

/// A shell block's line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Row {
    /// A command starts here: the prompt goes before it.
    Command,
    /// Part of a command, or a blank or comment line between them.
    More,
    /// What a command printed, shown dim and never copied.
    Output,
}

/// A shell block as shown: its text with any typed prompts taken off,
/// each line's part, and what Copy copies.
#[derive(Debug, PartialEq)]
pub(super) struct ShellView {
    pub(super) shown: String,
    pub(super) rows: Vec<Row>,
    pub(super) copy: String,
}

/// A line's command when it starts with a prompt: `$ `, or PowerShell's
/// `PS> ` or `PS C:\dir> `.
fn after_prompt(line: &str) -> Option<&str> {
    if line == "$" {
        return Some("");
    }
    if let Some(rest) = line.strip_prefix("$ ") {
        return Some(rest);
    }
    if line.starts_with("PS>") || line.starts_with("PS ") {
        let at = line.find('>')?;
        return Some(line[at + 1..].strip_prefix(' ').unwrap_or(&line[at + 1..]));
    }
    None
}

/// Whether the line after this one carries on its command.
fn continues(line: &str) -> bool {
    let t = line.trim_end();
    t.ends_with('\\')
        || t.ends_with('`')
        || t.ends_with('|')
        || t.ends_with("&&")
        || t.ends_with("||")
}

pub(super) fn shell_view(text: &str) -> ShellView {
    let lines: Vec<&str> = text.split('\n').collect();
    let typed = lines.iter().any(|l| after_prompt(l).is_some());
    let mut shown = Vec::with_capacity(lines.len());
    let mut rows = Vec::with_capacity(lines.len());
    let mut copy = Vec::new();
    let mut carry = false;
    for l in &lines {
        if typed {
            // A session as it was typed: only the prompted lines and what
            // carries them on are commands, the rest is their output.
            if let Some(cmd) = after_prompt(l) {
                shown.push(cmd);
                rows.push(Row::Command);
                copy.push(cmd);
                carry = continues(cmd);
            } else if carry {
                shown.push(l);
                rows.push(Row::More);
                copy.push(l);
                carry = continues(l);
            } else {
                shown.push(l);
                rows.push(Row::Output);
            }
        } else {
            // A script: every line is copied, a prompt marks where each
            // command at the top level starts.
            let t = l.trim();
            let starts = !carry
                && !t.is_empty()
                && !t.starts_with('#')
                && !l.starts_with(char::is_whitespace);
            shown.push(l);
            rows.push(if starts { Row::Command } else { Row::More });
            if !t.is_empty() && !t.starts_with('#') {
                carry = continues(l);
            }
        }
    }
    ShellView {
        shown: shown.join("\n"),
        rows,
        copy: if typed {
            copy.join("\n")
        } else {
            text.to_string()
        },
    }
}

/// Each source line's top and height, from the laid out lines' heights
/// and whether each ends one: a long line wraps onto several.
pub(super) fn source_rows(lines: &[(f32, bool)]) -> Vec<(f32, f32)> {
    let mut out = Vec::new();
    let (mut y, mut top) = (0.0, 0.0);
    for (k, &(h, ends)) in lines.iter().enumerate() {
        y += h;
        if ends || k + 1 == lines.len() {
            out.push((top, y - top));
            top = y;
        }
    }
    out
}

/// The gutter as one text, a line per laid out line of the code: each
/// source line's mark on its first row, the rows it wraps onto blank.
/// One layout for the gutter, not one per line, keeps a long block cheap.
pub(super) fn gutter_lines(marks: &[Option<String>], lines: &[(f32, bool)]) -> String {
    let mut out = String::new();
    let mut row = 0;
    let mut first = true;
    for (k, &(_, ends)) in lines.iter().enumerate() {
        if k > 0 {
            out.push('\n');
        }
        if first {
            if let Some(Some(m)) = marks.get(row) {
                out.push_str(m);
            }
        }
        first = ends;
        if ends {
            row += 1;
        }
    }
    out
}

/// What the footer of a folded block says.
pub(super) fn fold_say(total: usize, whole: bool) -> String {
    match (whole, total.saturating_sub(FOLD_TO)) {
        (true, _) => "Show less".to_string(),
        (false, 1) => "Show 1 more line".to_string(),
        (false, n) => format!("Show {n} more lines"),
    }
}

fn line_metrics(l: &IDWriteTextLayout) -> Vec<(f32, bool)> {
    let mut n = 0u32;
    unsafe {
        let _ = l.GetLineMetrics(None, &mut n);
        let mut lines = vec![DWRITE_LINE_METRICS::default(); n as usize];
        if l.GetLineMetrics(Some(&mut lines), &mut n).is_err() {
            return Vec::new();
        }
        lines
            .iter()
            .map(|m| (m.height, m.newlineLength > 0))
            .collect()
    }
}

/// Where each line starts in a layout's text, in UTF-16 units, with its
/// length.
fn spans(lines: &[&str]) -> Vec<(u32, u32)> {
    let mut at = 0u32;
    (lines.iter())
        .map(|l| {
            let n = l.encode_utf16().count() as u32;
            let s = (at, n);
            at += n + 1;
            s
        })
        .collect()
}

impl Ctx<'_> {
    /// A fenced code block: a header with its language, file and Copy,
    /// the code coloured as the file viewer colours it, numbered when it
    /// is long and folded when it is very long. Diffs have their rows
    /// tinted, shell blocks a prompt before each command.
    ///
    /// Long lines wrap rather than clip: the Folio has no sideways
    /// scroll, and a clipped line hides exactly the part that differs.
    /// The gutter marks only a wrapped line's first row.
    pub(super) unsafe fn code(&self, info: &str, text: &str, w: f32) -> Scene {
        let mut s = Scene::default();
        // A lone carriage return would start a laid out line of its own
        // and push the gutter and tints off their lines.
        let crlf;
        let text = if text.contains('\r') {
            crlf = text.replace("\r\n", "\n").replace('\r', "\n");
            crlf.as_str()
        } else {
            text
        };
        let fence = Fence::parse(info);
        let style = Style::of(&fence.lang, text);
        let shell = matches!(style, Style::Shell(_)).then(|| shell_view(text));
        let body = shell.as_ref().map_or(text, |v| v.shown.as_str());
        let all: Vec<&str> = body.split('\n').collect();
        let key = fold_key(text);
        let foldable = all.len() > FOLD_OVER;
        let whole = !foldable || is_open(&key);
        let lines = if whole { &all[..] } else { &all[..FOLD_TO] };
        let shown = lines.join("\n");

        let cw = self
            .one_line(&self.fonts.code, "0")
            .map_or(7.6, |l| render::text_size(&l).0);
        let gutter = match style {
            Style::Plain if all.len() >= NUMBER_FROM => {
                (all.len().to_string().len() as f32 * cw + 18.0).max(32.0)
            }
            Style::Shell(_) => 2.0 * cw + 10.0,
            _ => 0.0,
        };
        let x0 = CODE_PAD + gutter;
        let Some(l) = self.layout(&self.fonts.code, &shown, w - x0 - CODE_PAD) else {
            return s;
        };
        let _ = l.SetIncrementalTabStop(TAB * cw);
        let at = spans(lines);
        match style {
            Style::Diff => self.colour_diff(&l, lines, &at),
            _ => colour_code(self, &l, &fence.lang, &shown),
        }
        if let Some(v) = &shell {
            for (row, &(a, n)) in v.rows.iter().zip(&at) {
                if *row == Row::Output {
                    self.paint(&l, a..a + n, self.pal.dim);
                }
            }
        }
        let visual = line_metrics(&l);
        let rows = source_rows(&visual);
        let (_, th) = render::text_size(&l);
        let top = CODE_HEAD + BODY_TOP;
        let foot = if foldable { FOOT } else { 0.0 };
        let h = top + th + BODY_BOTTOM + foot;

        let plate = Rect::new(0.0, 0.0, w, h);
        s.fill(plate, RADIUS, Color::rgb(0).with_alpha(0.32));
        // The header a shade lighter, its top corners the plate's.
        s.items.push(Item::Clip(Rect::new(0.0, 0.0, w, CODE_HEAD)));
        s.fill(plate, RADIUS, self.pal.fg.with_alpha(0.035));
        s.items.push(Item::Unclip);
        s.stroke(plate, RADIUS, self.pal.fg.with_alpha(0.07), 1.0);
        s.fill(
            Rect::new(0.0, CODE_HEAD - 1.0, w, 1.0),
            0.0,
            self.pal.fg.with_alpha(0.06),
        );
        // Folding again from the header, so a long block opened need not
        // be scrolled to its end to close.
        let fold = (foldable && whole).then_some(&key);
        self.code_head(&mut s, &fence, style, text, shell.as_ref(), fold, w);

        if style == Style::Diff {
            for (line, &(y, rh)) in lines.iter().zip(&rows) {
                let tint = match diff_mark(line) {
                    Some('+') => theme::done().with_alpha(0.14),
                    Some('-') => theme::error().with_alpha(0.16),
                    _ => continue,
                };
                s.fill(Rect::new(1.0, top + y, w - 2.0, rh), 0.0, tint);
            }
        }
        let marks: Vec<Option<String>> = match (&shell, style) {
            (Some(v), Style::Shell(ps)) => {
                let glyph = if ps { ">" } else { "$" };
                (v.rows.iter().take(rows.len()))
                    .map(|r| (*r == Row::Command).then(|| glyph.to_string()))
                    .collect()
            }
            (_, Style::Plain) if gutter > 0.0 => {
                (1..=rows.len()).map(|k| Some(k.to_string())).collect()
            }
            _ => Vec::new(),
        };
        if !marks.is_empty() {
            self.gutter(&mut s, &marks, &visual, &rows, x0 - 10.0, top);
        }
        s.unit(&l, x0, top, self.pal.fg.with_alpha(0.9), &shown);

        if foldable {
            let fy = h - FOOT;
            s.fill(
                Rect::new(0.0, fy, w, 1.0),
                0.0,
                self.pal.fg.with_alpha(0.06),
            );
            let icon = if whole { "\u{E70E}" } else { "\u{E70D}" };
            let say = fold_say(all.len(), whole);
            let ink = self.link_ink();
            s.line(icon, F::Icon, Rect::new(CODE_PAD, fy, 16.0, FOOT), ink);
            s.line(
                say,
                F::Small,
                Rect::new(CODE_PAD + 22.0, fy, w / 2.0, FOOT),
                ink,
            );
            s.hit(Rect::new(0.0, fy, w, FOOT), Hit::Tr(Act::All(key)));
        }
        s.h = h + 6.0;
        s
    }

    /// The header: the file when the fence names one, the language, Copy.
    unsafe fn code_head(
        &self,
        s: &mut Scene,
        fence: &Fence,
        style: Style,
        text: &str,
        shell: Option<&ShellView>,
        fold: Option<&String>,
        w: f32,
    ) {
        let lang = match (fence.lang.as_str(), style) {
            ("", Style::Diff) => "diff",
            ("", _) => "text",
            (l, _) => label(l),
        };
        let buttons = COPY_W + if fold.is_some() { FOLD_W } else { 0.0 };
        let room = (w - 2.0 * CODE_PAD - buttons).max(0.0);
        let mut x = CODE_PAD;
        // Too narrow a header for a readable name leaves it to the language.
        if let Some(file) = fence.file.as_ref().filter(|_| room >= 40.0) {
            let fw = self
                .one_line(&self.gfx.small, file)
                .map_or(room / 2.0, |l| render::text_size(&l).0)
                .min(room * 0.7);
            let r = Rect::new(x, 0.0, fw + 2.0, CODE_HEAD);
            s.line(file.clone(), F::Small, r, self.pal.fg.with_alpha(0.78));
            s.hit(r, Hit::Tr(Act::Path(file.clone(), None)));
            x += fw + 12.0;
        }
        s.line(
            lang,
            F::Small,
            Rect::new(x, 0.0, (CODE_PAD + room - x).max(0.0), CODE_HEAD),
            self.pal.faint,
        );
        let copy = Rect::new(w - COPY_W - 4.0, 4.0, COPY_W, CODE_HEAD - 8.0);
        let what = shell.map_or(text, |v| v.copy.as_str());
        s.items.extend(self.copy_face(copy, what, self.pal.dim));
        s.hit(copy, Hit::Tr(Act::Copy(what.to_string())));
        if let Some(key) = fold {
            let r = Rect::new(copy.x - FOLD_W, 4.0, FOLD_W, CODE_HEAD - 8.0);
            let ink = self.pal.dim;
            let icon = Rect::new(r.x + 6.0, r.y, 16.0, r.h);
            s.line("\u{E70E}", F::Icon, icon, ink);
            s.line(
                "Fold",
                F::Small,
                Rect::new(r.x + 26.0, r.y, r.w - 28.0, r.h),
                ink,
            );
            s.hit(r, Hit::Tr(Act::All(key.clone())));
        }
    }

    /// Line numbers or prompts, right aligned to `right` on each line's
    /// first row, drawn but never selected or copied. One layout for the
    /// lot when its rows match the code's, which they do unless a font
    /// fallback made some line of the code taller; then one per mark.
    unsafe fn gutter(
        &self,
        s: &mut Scene,
        marks: &[Option<String>],
        visual: &[(f32, bool)],
        rows: &[(f32, f32)],
        right: f32,
        top: f32,
    ) {
        let ink = self.pal.faint.with_alpha(0.75);
        let all = gutter_lines(marks, visual);
        if let Some(g) = self.layout(&self.fonts.code, &all, right) {
            let _ = g.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            let _ = g.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING);
            let theirs = line_metrics(&g);
            let fits = theirs.len() == visual.len()
                && (theirs.iter().zip(visual)).all(|(a, b)| (a.0 - b.0).abs() < 0.5);
            if fits {
                s.text(&g, 0.0, top, ink);
                return;
            }
        }
        for (m, &(y, _)) in marks.iter().zip(rows) {
            let Some(m) = m else { continue };
            if let Some(l) = self.one_line(&self.fonts.code, m) {
                let (tw, _) = render::text_size(&l);
                s.text(&l, right - tw, top + y, ink);
            }
        }
    }

    /// A diff's marks coloured as the edit view colours them, its hunk
    /// and file lines dim.
    fn colour_diff(&self, l: &IDWriteTextLayout, lines: &[&str], at: &[(u32, u32)]) {
        for (line, &(a, n)) in lines.iter().zip(at) {
            match diff_mark(line) {
                Some('+') => self.paint(l, a..a + 1, theme::done()),
                Some('-') => self.paint(l, a..a + 1, theme::error()),
                Some('@') => self.paint(l, a..a + n, self.link_ink().with_alpha(0.8)),
                Some(_) => self.paint(l, a..a + n, self.pal.dim),
                None => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fence_names_a_file_either_way() {
        let f = Fence::parse("rust title=\"src/x.rs\"");
        assert_eq!(f.lang, "rust");
        assert_eq!(f.file.as_deref(), Some("src/x.rs"));
        let f = Fence::parse("rust:src/x.rs");
        assert_eq!(f.lang, "rust");
        assert_eq!(f.file.as_deref(), Some("src/x.rs"));
        let f = Fence::parse("Python");
        assert_eq!(
            f,
            Fence {
                lang: "python".into(),
                file: None
            }
        );
        assert_eq!(Fence::parse("").lang, "");
    }

    #[test]
    fn formulas_and_diagrams_say_what_they_are() {
        assert_eq!(label("math"), "math");
        assert_eq!(label("mermaid"), "diagram");
        assert_eq!(label("rust"), "rust");
    }

    #[test]
    fn diff_is_told_from_a_list() {
        assert!(looks_like_diff("@@ -1 +1 @@\n-a\n+b\n c"));
        assert!(!looks_like_diff("- one\n- two\n- three"));
        assert!(!looks_like_diff("fn main() {}\n+1"));
        assert_eq!(Style::of("", "-a\n+b"), Style::Diff);
        assert_eq!(Style::of("yaml", "-a\n+b"), Style::Plain);
        assert_eq!(Style::of("ps1", "ls"), Style::Shell(true));
    }

    #[test]
    fn typed_session_copies_only_commands() {
        let v = shell_view("$ cargo build \\\n  --release\n   Compiling x\n$ ls\nREADME.md");
        assert_eq!(
            v.shown,
            "cargo build \\\n  --release\n   Compiling x\nls\nREADME.md"
        );
        assert_eq!(
            v.rows,
            [
                Row::Command,
                Row::More,
                Row::Output,
                Row::Command,
                Row::Output
            ]
        );
        assert_eq!(v.copy, "cargo build \\\n  --release\nls");
    }

    #[test]
    fn powershell_prompt_comes_off() {
        let v = shell_view("PS C:\\x> Get-Item .\nout");
        assert_eq!(v.shown, "Get-Item .\nout");
        assert_eq!(v.copy, "Get-Item .");
    }

    #[test]
    fn script_marks_where_commands_start() {
        let text = "# build\ncargo build \\\n  --release\n\nls | wc -l";
        let v = shell_view(text);
        assert_eq!(v.shown, text);
        assert_eq!(v.copy, text);
        assert_eq!(
            v.rows,
            [Row::More, Row::Command, Row::More, Row::More, Row::Command]
        );
    }

    #[test]
    fn wrapped_lines_make_one_row() {
        let rows = source_rows(&[(10.0, false), (10.0, true), (10.0, true), (10.0, false)]);
        assert_eq!(rows, [(0.0, 20.0), (20.0, 10.0), (30.0, 10.0)]);
        assert!(source_rows(&[]).is_empty());
    }

    #[test]
    fn gutter_marks_only_a_wrapped_lines_first_row() {
        let marks = [Some("1".to_string()), None, Some("3".to_string())];
        // Line one wraps onto a second row, line three onto a third.
        let lines = [
            (10.0, false),
            (10.0, true),
            (10.0, true),
            (10.0, false),
            (10.0, false),
        ];
        assert_eq!(gutter_lines(&marks, &lines), "1\n\n\n3\n");
        assert_eq!(gutter_lines(&marks, &[]), "");
    }

    #[test]
    fn fold_footer_counts_what_is_hidden() {
        assert_eq!(fold_say(FOLD_TO + 1, false), "Show 1 more line");
        assert_eq!(
            fold_say(500, false),
            format!("Show {} more lines", 500 - FOLD_TO)
        );
        assert_eq!(fold_say(500, true), "Show less");
    }

    #[test]
    fn open_print_ignores_other_keys_and_order() {
        let a: HashSet<String> = ["code:1", "step", "code:2"].map(String::from).into();
        let b: HashSet<String> = ["code:2", "code:1"].map(String::from).into();
        assert_eq!(open_print(&a), open_print(&b));
        assert_ne!(open_print(&a), open_print(&HashSet::new()));
    }
}
