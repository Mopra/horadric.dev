//! A markdown table in a reply, set as a data table: no box and no
//! stripes, hairlines between rows, a small label for a header, figures
//! right aligned and in step, short statuses as pills.

use horadric_core::markdown::{Align, Inline};
use windows::core::Interface;
use windows::Win32::Graphics::DirectWrite::{
    IDWriteTextLayout, IDWriteTextLayout1, DWRITE_FONT_FEATURE,
    DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, DWRITE_FONT_WEIGHT_MEDIUM,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_WORD_WRAPPING_EMERGENCY_BREAK,
};

use super::flow;
use super::rich::{first_line, range, Ctx, Scene, F};
use super::transcript::Act;
use super::view::Hit;
use crate::layout::Rect;
use crate::render;
use crate::theme;

/// The room between two columns, half on each side. The outer edges get
/// none, so the first column lines up with the prose around the table.
const CELL_X: f32 = 12.0;
/// A body row's padding above and below its text.
const CELL_Y: f32 = 7.0;
/// The header label's room under it, above its rule.
const HEAD_BELOW: f32 = 7.0;
/// The narrowest a table column gets.
const CELL_LEAST: f32 = 64.0;
/// The header label: smaller than the body and a touch tracked, so it
/// reads as a label over the data rather than a row of it.
const HEAD_SIZE: f32 = 12.5;
const HEAD_TRACK: f32 = 0.4;
/// A status pill's text size, its height and its padding either side.
const PILL_SIZE: f32 = 12.5;
const PILL_H: f32 = 20.0;
const PILL_X: f32 = 8.0;
/// The hover Copy button's size.
const COPY_W: f32 = 64.0;
const COPY_H: f32 = 22.0;

impl Ctx<'_> {
    /// A table: columns as wide as their widest cell, the wide ones
    /// wrapping when the table does not fit.
    pub(super) unsafe fn table(
        &self,
        header: &[Inline],
        align: &[Align],
        rows: &[Vec<Inline>],
        w: f32,
    ) -> Scene {
        let mut s = Scene::default();
        let cols = header.len();
        if cols == 0 {
            return s;
        }
        fn cell(row: &[Inline], c: usize) -> &str {
            row.get(c).map_or("", |i| i.text.as_str())
        }
        let numeric: Vec<bool> = (0..cols)
            .map(|c| numeric_column(rows.iter().map(|r| cell(r, c))))
            .collect();
        // Figures read down a column by place value, so a column of them
        // sits right unless the markdown asked for something else.
        let aligns: Vec<Align> = (0..cols)
            .map(|c| match align.get(c).copied().unwrap_or(Align::None) {
                Align::None if numeric[c] => Align::Right,
                a => a,
            })
            .collect();
        let pad = |c: usize| {
            let l = if c == 0 { 0.0 } else { CELL_X / 2.0 };
            let r = if c + 1 == cols { 0.0 } else { CELL_X / 2.0 };
            (l, r)
        };
        // The first column names the row, so it is set a little stronger,
        // unless it is figures, which have no name to give.
        let lead = cols > 1 && !numeric[0];

        let mut natural = vec![0.0f32; cols];
        for (c, h) in header.iter().enumerate() {
            if let Some(l) = self.one_line(&self.fonts.small_wrap, &h.text) {
                self.head_style(&l, &h.text);
                natural[c] = natural[c].max(render::text_size(&l).0);
            }
        }
        for row in rows {
            for (c, inl) in row.iter().enumerate().take(cols) {
                let t = inl.text.as_str();
                let width = if let Some((_, label)) = status(t) {
                    self.pill_layout(&label)
                        .map_or(0.0, |l| render::text_size(&l).0 + 2.0 * PILL_X)
                } else if let Some(l) = self.one_line(&self.fonts.body, t) {
                    if numeric[c] {
                        self.tabular(&l, t);
                    }
                    if lead && c == 0 {
                        let _ = l.SetFontWeight(DWRITE_FONT_WEIGHT_MEDIUM, whole(t));
                    }
                    render::text_size(&l).0
                } else {
                    0.0
                };
                natural[c] = natural[c].max(width);
            }
        }
        for (c, n) in natural.iter_mut().enumerate() {
            let (l, r) = pad(c);
            *n += l + r + 2.0;
        }
        let least = CELL_LEAST.min(w / cols as f32);
        let widths = fit(&flow::table_widths(&natural, least, w), least, w);
        let total_w: f32 = widths.iter().sum();
        let lefts: Vec<f32> = widths
            .iter()
            .scan(0.0, |x, cw| {
                let at = *x;
                *x += cw;
                Some(at)
            })
            .collect();

        // The header: a dim label, then a rule a shade firmer than the
        // hairlines between rows, which is all that sets it apart.
        let mut y = 0.0;
        let mut hh: f32 = 0.0;
        for (c, h) in header.iter().enumerate() {
            let (pl, pr) = pad(c);
            let mark = s.units.len();
            let (th, _) = self.inline(
                &mut s,
                h,
                &self.fonts.small_wrap,
                lefts[c] + pl,
                y,
                widths[c] - pl - pr,
                self.pal.dim,
                aligns[c],
            );
            let th = match s.units.get_mut(mark) {
                Some(u) => {
                    self.head_style(&u.layout, &u.text);
                    let (_, nh) = self.fit_cell(&u.layout);
                    u.h = nh;
                    nh
                }
                None => th,
            };
            hh = hh.max(th);
        }
        y += hh + HEAD_BELOW;
        s.fill(
            Rect::new(0.0, y - 1.0, total_w, 1.0),
            0.0,
            self.pal.fg.with_alpha(0.16),
        );

        for (k, row) in rows.iter().enumerate() {
            let mut rh: f32 = 0.0;
            for (c, inl) in row.iter().enumerate().take(cols) {
                let (pl, pr) = pad(c);
                let x = lefts[c] + pl;
                let cw = widths[c] - pl - pr;
                let t = inl.text.as_str();
                if let Some((tone, label)) = status(t) {
                    rh = rh.max(self.pill(&mut s, tone, &label, x, y + CELL_Y, cw, aligns[c]));
                    continue;
                }
                let ink = if blank(t) {
                    self.pal.faint
                } else if lead && c == 0 {
                    self.pal.fg.with_alpha(0.96)
                } else {
                    self.pal.fg.with_alpha(0.84)
                };
                let mark = s.units.len();
                let (h, _) = self.inline(
                    &mut s,
                    inl,
                    &self.fonts.body,
                    x,
                    y + CELL_Y,
                    cw,
                    ink,
                    aligns[c],
                );
                let h = match s.units.get_mut(mark) {
                    Some(u) => {
                        if numeric[c] {
                            self.tabular(&u.layout, &u.text);
                        }
                        if lead && c == 0 {
                            let _ = u
                                .layout
                                .SetFontWeight(DWRITE_FONT_WEIGHT_MEDIUM, whole(&u.text));
                        }
                        let (_, nh) = self.fit_cell(&u.layout);
                        u.h = nh;
                        nh
                    }
                    None => h,
                };
                rh = rh.max(h);
            }
            y += rh + 2.0 * CELL_Y;
            // No rule under the last row: with no box around it, the
            // table ends where its text does.
            if k + 1 < rows.len() {
                s.fill(
                    Rect::new(0.0, y - 1.0, total_w, 1.0),
                    0.0,
                    self.pal.fg.with_alpha(0.07),
                );
            }
        }

        self.copy_button(&mut s, header, rows, w, total_w, hh);
        s.h = y + 4.0;
        s
    }

    /// The header's label look on a layout already made from `text`.
    unsafe fn head_style(&self, l: &IDWriteTextLayout, text: &str) {
        let r = whole(text);
        let _ = l.SetFontSize(HEAD_SIZE, r);
        let _ = l.SetFontWeight(DWRITE_FONT_WEIGHT_SEMI_BOLD, r);
        if let Ok(l1) = l.cast::<IDWriteTextLayout1>() {
            let _ = l1.SetCharacterSpacing(0.0, HEAD_TRACK, 0.0, r);
        }
    }

    /// Every digit the same width, so figures line up down a column.
    unsafe fn tabular(&self, l: &IDWriteTextLayout, text: &str) {
        if let Ok(t) = self.gpu.dw.CreateTypography() {
            let _ = t.AddFontFeature(DWRITE_FONT_FEATURE {
                nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                parameter: 1,
            });
            let _ = l.SetTypography(&t, whole(text));
        }
    }

    /// A cell's size once a word too long for its column is broken
    /// rather than left to run into the next one.
    unsafe fn fit_cell(&self, l: &IDWriteTextLayout) -> (f32, f32) {
        let fits = l
            .DetermineMinWidth()
            .is_ok_and(|m| m <= l.GetMaxWidth() + 0.5);
        if !fits {
            let _ = l.SetWordWrapping(DWRITE_WORD_WRAPPING_EMERGENCY_BREAK);
        }
        render::text_size(l)
    }

    unsafe fn pill_layout(&self, label: &str) -> Option<IDWriteTextLayout> {
        let l = self.one_line(&self.fonts.small_wrap, label)?;
        let r = whole(label);
        let _ = l.SetFontSize(PILL_SIZE, r);
        let _ = l.SetFontWeight(DWRITE_FONT_WEIGHT_MEDIUM, r);
        Some(l)
    }

    /// A status as a small pill tinted by its tone, centred on the first
    /// line of body text so it sits level with the cells beside it.
    /// Returns the height it takes, which is that line's.
    #[allow(clippy::too_many_arguments)]
    unsafe fn pill(
        &self,
        s: &mut Scene,
        tone: Tone,
        label: &str,
        x: f32,
        y: f32,
        cw: f32,
        align: Align,
    ) -> f32 {
        let line = self
            .one_line(&self.fonts.body, "Ag")
            .and_then(|l| first_line(&l))
            .unwrap_or(PILL_H);
        let Some(l) = self.pill_layout(label) else {
            return line;
        };
        let (tw, th) = render::text_size(&l);
        let pw = (tw + 2.0 * PILL_X).min(cw);
        let px = match align {
            Align::Right => x + cw - pw,
            Align::Center => x + (cw - pw) / 2.0,
            _ => x,
        };
        let py = y + (line - PILL_H) / 2.0;
        let (fill, ink) = match tone {
            Tone::Good => (
                theme::done().with_alpha(0.14),
                theme::done().mix(self.pal.fg, 0.3),
            ),
            Tone::Bad => (
                theme::error().with_alpha(0.15),
                theme::error().mix(self.pal.fg, 0.3),
            ),
            Tone::Warn => (
                theme::waiting().with_alpha(0.14),
                theme::waiting().mix(self.pal.fg, 0.3),
            ),
            Tone::Neutral => (self.pal.fg.with_alpha(0.08), self.pal.dim),
        };
        s.fill(Rect::new(px, py, pw, PILL_H), PILL_H / 2.0, fill);
        s.unit(&l, px + PILL_X, py + (PILL_H - th) / 2.0, ink, label);
        line
    }

    /// Copy on hover, the table as tab separated text so it pastes into a
    /// spreadsheet as cells. It sits at the right of the header line;
    /// over a table that reaches that far it gets a plate of the page's
    /// own colour, so the header under it does not show through.
    unsafe fn copy_button(
        &self,
        s: &mut Scene,
        header: &[Inline],
        rows: &[Vec<Inline>],
        w: f32,
        total_w: f32,
        hh: f32,
    ) {
        use super::rich::Item;
        let copy = Rect::new(w - COPY_W, (hh - COPY_H) / 2.0, COPY_W, COPY_H);
        if total_w > copy.x - 8.0 {
            s.hover.push(Item::Fill(copy, 5.0, self.pal.bg));
            s.hover
                .push(Item::Fill(copy, 5.0, self.pal.fg.with_alpha(0.06)));
        }
        s.hover.push(Item::Line(
            "\u{E8C8}".into(),
            F::Icon,
            Rect::new(copy.x + 6.0, copy.y, 16.0, copy.h),
            self.pal.dim,
        ));
        s.hover.push(Item::Line(
            "Copy".into(),
            F::Small,
            Rect::new(copy.x + 26.0, copy.y, COPY_W - 26.0, copy.h),
            self.pal.dim,
        ));
        let head: Vec<&str> = header.iter().map(|i| i.text.as_str()).collect();
        let body: Vec<Vec<&str>> = rows
            .iter()
            .map(|r| r.iter().map(|i| i.text.as_str()).collect())
            .collect();
        s.hover_hits
            .push((copy, Hit::Tr(Act::Copy(tsv(&head, &body)))));
    }
}

fn whole(text: &str) -> windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_RANGE {
    range(0..text.encode_utf16().count() as u32)
}

/// Column widths that never add up to more than `w`. The flow's widths
/// can, when a column it keeps whole is itself wide; then every column
/// above `least` gives up room in proportion to what it has over it.
fn fit(widths: &[f32], least: f32, w: f32) -> Vec<f32> {
    let total: f32 = widths.iter().sum();
    if total <= w {
        return widths.to_vec();
    }
    let floor: f32 = widths.iter().map(|x| x.min(least)).sum();
    let over = total - floor;
    let room = (w - floor).max(0.0);
    let k = if over > 0.0 { room / over } else { 0.0 };
    widths
        .iter()
        .map(|x| {
            let base = x.min(least);
            base + (x - base) * k
        })
        .collect()
}

/// How a status cell is tinted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Good,
    Bad,
    Warn,
    Neutral,
}

/// A cell that holds nothing worth reading: blank, a dash, n/a.
fn blank(s: &str) -> bool {
    matches!(
        s.trim().to_lowercase().as_str(),
        "" | "-" | "\u{2013}" | "\u{2014}" | "n/a" | "na" | "?" | "..." | "\u{2026}"
    )
}

/// The symbols agents mark a status with, as the plain mark a pill shows.
/// Colour emoji would not take the pill's tint, and a plain mark does.
fn mark(c: char) -> Option<(Tone, &'static str)> {
    Some(match c {
        '\u{2705}' | '\u{2714}' | '\u{2713}' | '\u{2611}' => (Tone::Good, "\u{2713}"),
        '\u{274C}' | '\u{2716}' | '\u{2717}' | '\u{2718}' | '\u{274E}' => (Tone::Bad, "\u{2717}"),
        '\u{26A0}' => (Tone::Warn, "!"),
        '\u{23F3}' | '\u{1F7E1}' => (Tone::Warn, "\u{2022}"),
        '\u{1F7E2}' => (Tone::Good, "\u{2022}"),
        '\u{1F534}' => (Tone::Bad, "\u{2022}"),
        _ => return None,
    })
}

fn word_tone(w: &str) -> Option<Tone> {
    Some(match w {
        "yes" | "y" | "true" | "done" | "ok" | "okay" | "pass" | "passed" | "passing"
        | "success" | "succeeded" | "complete" | "completed" | "enabled" | "on" | "fixed"
        | "merged" | "shipped" | "supported" | "ready" | "green" | "works" | "working" => {
            Tone::Good
        }
        "no" | "n" | "false" | "fail" | "failed" | "failing" | "error" | "errored" | "broken"
        | "disabled" | "off" | "missing" | "blocked" | "unsupported" | "red" | "crash"
        | "crashed" | "rejected" => Tone::Bad,
        "partial" | "partly" | "warning" | "warn" | "pending" | "wip" | "in progress" | "flaky"
        | "degraded" | "slow" | "running" | "in review" | "yellow" => Tone::Warn,
        "todo" | "to do" | "tbd" | "skipped" | "skip" | "planned" | "later" | "draft"
        | "unknown" | "not started" => Tone::Neutral,
        _ => return None,
    })
}

/// A short status cell's tone and the label its pill shows: a known word,
/// a check or cross mark, or a mark and then a word ("✅ done"). Anything
/// longer is prose and stays text.
fn status(s: &str) -> Option<(Tone, String)> {
    let t = s.trim();
    if t.is_empty() || t.chars().count() > 24 {
        return None;
    }
    let mut chars = t.chars();
    let first = chars.next()?;
    if let Some((tone, sign)) = mark(first) {
        // A variation selector often follows an emoji and is not a word.
        let rest = chars.as_str().trim_start_matches('\u{FE0F}').trim();
        if rest.is_empty() {
            return Some((tone, sign.to_string()));
        }
        if rest.split_whitespace().count() > 3 {
            return None;
        }
        return Some((tone, format!("{sign} {rest}")));
    }
    let lower = t.to_lowercase();
    let word = lower.trim_end_matches(['.', '!']);
    word_tone(word).map(|tone| (tone, t.trim_end_matches(['.', '!']).to_string()))
}

/// Whether a cell holds a figure: a number with an optional sign,
/// currency and unit (`-12.5%`, `$1,200`, `12 KB`, `340ms`, `1h 30m`,
/// `1:23:45`). Dates, versions and words with a number in them are not.
fn numeric(s: &str) -> bool {
    let s = s.trim();
    let s = s.trim_start_matches([
        '+', '-', '\u{2212}', '\u{00B1}', '~', '\u{2248}', '<', '>', '\u{2264}', '\u{2265}',
    ]);
    let s = s.trim_start_matches(['$', '\u{20AC}', '\u{00A3}', '\u{00A5}', '\u{20B9}']);
    let mut rest = s.trim_start();
    if rest.is_empty() {
        return false;
    }
    let mut segments = 0;
    while !rest.is_empty() {
        let Some((after, unit)) = figure(rest) else {
            return false;
        };
        segments += 1;
        rest = after.trim_start();
        // Only a figure with a unit can be followed by another, as in
        // `1h 30m`; `12 34` is two numbers, not one.
        if !rest.is_empty() && !unit {
            return false;
        }
    }
    segments > 0
}

/// One number and the unit after it, if any. Returns what follows and
/// whether there was a unit.
fn figure(s: &str) -> Option<(&str, bool)> {
    let mut digits = 0;
    let mut dots = 0;
    let mut end = 0;
    for (i, c) in s.char_indices() {
        match c {
            '0'..='9' => digits += 1,
            '.' => dots += 1,
            ',' | '_' | '\'' | ':' => {}
            _ => break,
        }
        end = i + c.len_utf8();
    }
    let num = &s[..end];
    if digits == 0 || dots > 1 || num.ends_with([',', ':', '\'']) {
        return None;
    }
    let after = &s[end..];
    let trimmed = after.trim_start();
    let unit_len = trimmed
        .char_indices()
        .take_while(|(_, c)| {
            c.is_alphabetic()
                || matches!(
                    c,
                    '%' | '\u{2030}'
                        | '\u{00B0}'
                        | '\u{00B5}'
                        | '\u{00D7}'
                        | '/'
                        | '$'
                        | '\u{20AC}'
                        | '\u{00A3}'
                )
        })
        .last()
        .map_or(0, |(i, c)| i + c.len_utf8());
    if unit_len == 0 {
        // Nothing but space can follow a bare number.
        return Some((after, false)).filter(|_| trimmed.is_empty() || after.len() != trimmed.len());
    }
    let unit = &trimmed[..unit_len];
    if unit.chars().count() > 8 {
        return None;
    }
    Some((&trimmed[unit_len..], true))
}

/// A column is figures when most of its filled cells are. Blanks and
/// dashes say nothing either way.
fn numeric_column<'a>(cells: impl Iterator<Item = &'a str>) -> bool {
    let (mut filled, mut figures) = (0, 0);
    for c in cells.filter(|c| !blank(c)) {
        filled += 1;
        figures += numeric(c) as usize;
    }
    filled > 0 && figures * 4 >= filled * 3
}

/// The table as tab separated values, one line a row, the way a
/// spreadsheet takes a paste. Tabs and line breaks inside a cell would
/// split it, so they become spaces.
fn tsv(header: &[&str], rows: &[Vec<&str>]) -> String {
    let cols = header.len();
    let clean = |c: &str| c.replace(['\t', '\r', '\n'], " ").trim().to_string();
    let line = |r: &[&str]| {
        (0..cols)
            .map(|c| clean(r.get(c).copied().unwrap_or("")))
            .collect::<Vec<_>>()
            .join("\t")
    };
    std::iter::once(line(header))
        .chain(rows.iter().map(|r| line(r)))
        .collect::<Vec<_>>()
        .join("\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_figures_are_numeric() {
        for s in [
            "12",
            "1,234",
            "-3.5",
            "+7",
            "42%",
            "$1,200",
            "\u{20AC}9.99",
            "12 KB",
            "1.5GB",
            "340ms",
            "2 s",
            "1h 30m",
            "1:23:45",
            "3x",
            "~200",
            "12 files",
            "0.5 \u{00B5}s",
        ] {
            assert!(numeric(s), "{s} is a figure");
        }
    }

    #[test]
    fn table_words_and_dates_are_not_numeric() {
        for s in [
            "",
            "abc",
            "Step 3",
            "v1.2",
            "1.2.3",
            "2026-11-06",
            "12 34",
            "3 big apples",
            "1,",
            "n/a",
            "rust 1.80",
        ] {
            assert!(!numeric(s), "{s} is not a figure");
        }
    }

    #[test]
    fn table_column_is_numeric_by_majority() {
        assert!(numeric_column(["1", "2", "-", "3"].into_iter()));
        assert!(numeric_column(["1", "2", "3", "x"].into_iter()));
        assert!(!numeric_column(["1", "a", "b"].into_iter()));
        assert!(!numeric_column(["-", ""].into_iter()));
    }

    #[test]
    fn table_status_cells_become_pills() {
        assert_eq!(status("Yes"), Some((Tone::Good, "Yes".into())));
        assert_eq!(status("failed."), Some((Tone::Bad, "failed".into())));
        assert_eq!(status("\u{2705}"), Some((Tone::Good, "\u{2713}".into())));
        assert_eq!(
            status("\u{274C} timeout"),
            Some((Tone::Bad, "\u{2717} timeout".into()))
        );
        assert_eq!(status("\u{26A0}\u{FE0F}"), Some((Tone::Warn, "!".into())));
        assert_eq!(status("todo"), Some((Tone::Neutral, "todo".into())));
        assert_eq!(status("yes, but only on Windows"), None);
        assert_eq!(status("nothing"), None);
    }

    #[test]
    fn table_tsv_pastes_as_cells() {
        let t = tsv(&["a", "b"], &[vec!["1", "x\ty"], vec!["2"]]);
        assert_eq!(t, "a\tb\r\n1\tx y\r\n2\t");
    }

    #[test]
    fn table_fit_never_overflows() {
        let w = fit(&[500.0, 2000.0], 64.0, 300.0);
        assert!(w.iter().sum::<f32>() <= 300.01);
        assert!(w.iter().all(|x| *x >= 64.0 - 0.01));
        assert_eq!(fit(&[100.0, 50.0], 64.0, 300.0), vec![100.0, 50.0]);
    }
}
