//! An edit's diff in the transcript, drawn as a code host draws one: a
//! header with the file and its counts, old and new line numbers in a
//! gutter, rows tinted by what happened to them with the words that
//! changed tinted stronger, the code coloured by its language, long lines
//! wrapped under the code rather than cut off, and hunks set apart.

use std::ops::Range;

use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_BOLD, DWRITE_LINE_METRICS, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_WORD_WRAPPING_EMERGENCY_BREAK, IDWriteTextLayout,
};

use super::flow::{self, DiffRow};
use super::rich::{self, range, Ctx, Scene, F, MONO_LINE};
use super::transcript::*;
use super::view::Hit;
use crate::layout::Rect;
use crate::render;
use crate::theme::{self, Color};

/// The header bar's height.
const HEAD: f32 = 30.0;
/// Space above the first row and below the last.
const BODY_PAD: f32 = 4.0;
const RADIUS: f32 = 6.0;
/// The squares of the bar beside the counts, as on GitHub.
const STAT_SQUARES: usize = 5;

impl Ctx<'_> {
    /// An edit's file, as a link that opens it, and its diff inline. `id`
    /// is what "Show all" is kept under: the step's, or the ask's. With
    /// `review`, an edit ask open to comments: each row's numbers take a
    /// click, and the rows commented on are marked.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn diff(
        &self,
        s: &mut Scene,
        id: &str,
        e: &horadric_core::folio::FileEdit,
        x: f32,
        y: f32,
        w: f32,
        st: &State,
        review: Option<&super::comments::Review>,
    ) -> f32 {
        let rows = flow::diff_rows(e);
        let all = st.all.contains(id);
        let shown = if all {
            rows.len()
        } else {
            rows.len().min(DIFF_ROWS)
        };
        let view = &rows[..shown];
        let hunked = !e.hunks.is_empty();
        let add = theme::done();
        let del = theme::error();

        let cw = self
            .one_line(&self.fonts.rows, "0")
            .map_or(7.8, |l| render::text_size(&l).0);
        let digits = (rows.iter())
            .filter_map(|r| r.old.max(r.new))
            .max()
            .unwrap_or(1)
            .to_string()
            .len()
            .max(2) as f32;
        let gw = digits * cw + 14.0;
        let mw = cw + 10.0;
        let cx = x + 2.0 * gw + mw;
        let text_w = (w - (cx - x) - 10.0).max(40.0);

        // The code, one source line a row. Wrapping is set on the layout,
        // not the format, since shell output shares the format unwrapped.
        let text: Vec<&str> = view
            .iter()
            .map(|r| if r.mark == '~' { "" } else { r.text.as_str() })
            .collect();
        let joined = text.join("\n");
        let starts = utf16_starts(&text);
        let l = if view.is_empty() {
            None
        } else {
            self.layout(&self.fonts.rows, &joined, text_w)
        };
        if let Some(l) = &l {
            let _ = l.SetWordWrapping(DWRITE_WORD_WRAPPING_EMERGENCY_BREAK);
            self.colour_rows(l, view, &starts, &e.path);
        }
        let heights = l.as_ref().map_or_else(Vec::new, |l| row_heights(l));
        let spans: Vec<(f32, f32)> = if heights.len() == view.len() {
            heights
        } else {
            (0..view.len())
                .map(|k| (k as f32 * MONO_LINE, MONO_LINE))
                .collect()
        };
        let body_h = spans.last().map_or(0.0, |&(t, h)| t + h);
        let lead = lead_label(view, hunked);
        let lead_h = if lead.is_some() { MONO_LINE } else { 0.0 };
        let body_top = y + HEAD;
        let rows_y = body_top + BODY_PAD + lead_h;
        let plate_h = if view.is_empty() {
            HEAD
        } else {
            HEAD + 2.0 * BODY_PAD + lead_h + body_h
        };
        let plate = Rect::new(x, y, w, plate_h);

        s.fill(plate, RADIUS, Color::rgb(0).with_alpha(0.3));
        self.diff_head(s, e, plate, add, del);

        if !view.is_empty() {
            s.items.push(rich::Item::Clip(Rect::new(
                x,
                body_top,
                w,
                plate_h - HEAD,
            )));
            let row_rect = |k: usize| {
                let (t, h) = spans[k];
                Rect::new(x, rows_y + t, w, h)
            };
            let gutter = |r: Rect| Rect::new(x, r.y, 2.0 * gw + mw - 4.0, r.h);
            // The gutter a shade apart from the code, as editors set it.
            s.fill(
                Rect::new(x, body_top, 2.0 * gw + mw - 4.0, plate_h - HEAD),
                0.0,
                self.pal.fg.with_alpha(0.02),
            );
            if let Some(label) = &lead {
                let band = Rect::new(x, body_top + BODY_PAD, w, MONO_LINE);
                self.fold_band(s, band, label, cx);
            }
            for (k, r) in view.iter().enumerate() {
                let row = row_rect(k);
                match r.mark {
                    '+' | '-' => {
                        let ink = if r.mark == '+' { add } else { del };
                        s.fill(row, 0.0, ink.with_alpha(0.11));
                        s.fill(gutter(row), 0.0, ink.with_alpha(0.10));
                    }
                    '~' => {
                        let label = gap_label(view, k, hunked);
                        self.fold_band(s, row, &label, cx);
                    }
                    _ => {}
                }
            }
            if let Some(l) = &l {
                for (a, b) in pairs(view) {
                    let Some((ra, rb)) = word_spans(&view[a].text, &view[b].text) else {
                        continue;
                    };
                    for (k, changed, ink) in [(a, ra, del), (b, rb, add)] {
                        let row = &view[k].text;
                        for r in changed {
                            let at = starts[k] + row[..r.start].encode_utf16().count() as u32;
                            let n = row[r].encode_utf16().count() as u32;
                            for hit in render::range_rects(l, at, n) {
                                s.fill(
                                    Rect::new(cx + hit.x, rows_y + hit.y, hit.w, hit.h),
                                    2.0,
                                    ink.with_alpha(0.30),
                                );
                            }
                        }
                    }
                }
            }
            if let Some(review) = review {
                for (k, r) in view.iter().enumerate() {
                    if r.mark == '~' {
                        continue;
                    }
                    let row = row_rect(k);
                    let nums = Rect::new(x, row.y, 2.0 * gw, row.h);
                    if review.marks(k) {
                        s.fill(nums, 0.0, self.pal.accent.with_alpha(0.22));
                        s.fill(Rect::new(x, row.y, 2.0, row.h), 0.0, self.pal.accent);
                    }
                    s.hit(nums, Hit::Tr(Act::Line(id.to_string(), k)));
                }
            }
            s.fill(
                Rect::new(x + 2.0 * gw + mw - 4.0, body_top, 1.0, plate_h - HEAD),
                0.0,
                self.pal.fg.with_alpha(0.06),
            );

            let lines = |k: usize| (spans[k].1 / MONO_LINE).round().max(1.0) as usize;
            let column = |f: &dyn Fn(&DiffRow) -> String| -> String {
                let mut out: Vec<String> = Vec::new();
                for (k, r) in view.iter().enumerate() {
                    out.push(f(r));
                    out.extend(std::iter::repeat_n(String::new(), lines(k) - 1));
                }
                out.join("\n")
            };
            let num = |n: Option<u32>| n.map_or(String::new(), |n| n.to_string());
            let olds = column(&|r| num(r.old));
            let news = column(&|r| num(r.new));
            for (k, nums) in [olds, news].iter().enumerate() {
                if let Some(nl) = self.layout(&self.fonts.rows, nums, gw - 8.0) {
                    let _ = nl.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING);
                    s.text(&nl, x + k as f32 * gw, rows_y, self.pal.faint);
                }
            }
            let marks = column(&|r| match r.mark {
                '+' => "+".to_string(),
                '-' => "-".to_string(),
                '~' => "\u{22ef}".to_string(),
                _ => String::new(),
            });
            if let Some(ml) = self.layout(&self.fonts.rows, &marks, mw) {
                let mut at = 0u32;
                for (k, r) in view.iter().enumerate() {
                    let ink = match r.mark {
                        '+' => add,
                        '-' => del,
                        _ => self.pal.faint,
                    };
                    let n = if matches!(r.mark, '+' | '-' | '~') { 1 } else { 0 };
                    self.paint(&ml, at..at + n, ink);
                    at += n + lines(k) as u32;
                }
                s.text(&ml, x + 2.0 * gw + 2.0, rows_y, self.pal.faint);
            }
            if let Some(l) = &l {
                s.unit(l, cx, rows_y, self.pal.fg.with_alpha(0.88), &joined);
            }
            s.items.push(rich::Item::Unclip);
        }
        s.stroke(plate, RADIUS, self.pal.fg.with_alpha(0.07), 1.0);

        let mut y = plate.bottom() + 2.0;
        if rows.len() > DIFF_ROWS {
            let say = if all {
                "Show less".to_string()
            } else {
                format!("Show all {} rows", rows.len())
            };
            s.line(
                say,
                F::Small,
                Rect::new(x + 10.0, y, w - 10.0, 20.0),
                self.link_ink(),
            );
            s.hit(Rect::new(x, y, w, 20.0), Hit::Tr(Act::All(id.to_string())));
            y += 20.0;
        }
        y
    }

    /// The bar over a diff: the file, its folder dim and its name bright,
    /// a link that opens it; then the lines added and removed, in their
    /// colours, with GitHub's five squares beside them.
    unsafe fn diff_head(
        &self,
        s: &mut Scene,
        e: &horadric_core::folio::FileEdit,
        plate: Rect,
        add: Color,
        del: Color,
    ) {
        let (x, y, w) = (plate.x, plate.y, plate.w);
        // The header a shade lighter, its top corners the plate's.
        s.items.push(rich::Item::Clip(Rect::new(x, y, w, HEAD)));
        s.fill(plate, RADIUS, self.pal.fg.with_alpha(0.035));
        s.items.push(rich::Item::Unclip);
        if plate.h > HEAD {
            s.fill(
                Rect::new(x, y + HEAD - 1.0, w, 1.0),
                0.0,
                self.pal.fg.with_alpha(0.07),
            );
        }

        let (plus, minus) = if e.added + e.removed > 0 {
            (e.added as usize, e.removed as usize)
        } else {
            counts(&flow::diff_rows(e))
        };
        let mut right = x + w - 10.0;
        let sq = 7.0;
        let squares = stat_squares(plus, minus);
        let bar_w = STAT_SQUARES as f32 * (sq + 2.0) - 2.0;
        let sy = y + (HEAD - sq) / 2.0;
        for (k, c) in squares.iter().enumerate() {
            let ink = match c {
                '+' => add,
                '-' => del,
                _ => self.pal.fg.with_alpha(0.12),
            };
            let r = Rect::new(right - bar_w + k as f32 * (sq + 2.0), sy, sq, sq);
            s.fill(r, 1.5, ink);
        }
        right -= bar_w + 8.0;
        let count = format!("+{plus} \u{2212}{minus}");
        if let Some(cl) = self.one_line(&self.fonts.rows, &count) {
            let split = format!("+{plus}").encode_utf16().count() as u32;
            let end = count.encode_utf16().count() as u32;
            self.paint(&cl, 0..split, add);
            self.paint(&cl, split + 1..end, del);
            let (tw, th) = render::text_size(&cl);
            right -= tw;
            s.text(&cl, right, y + (HEAD - th) / 2.0, self.pal.fg);
        }
        if e.created {
            let label_w = 64.0;
            right -= 10.0;
            s.line(
                "new file",
                F::Right,
                Rect::new(right - label_w, y, label_w, HEAD),
                self.pal.faint,
            );
            right -= label_w;
        }

        let shown = flow::project_path(self.dir, &e.path).unwrap_or_else(|| e.path.clone());
        let px = x + 12.0;
        let room = (right - 12.0 - px).max(20.0);
        if let Some(pl) = self.one_line(&self.fonts.rows, &shown) {
            let _ = pl.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
            let folder = folder_len(&shown);
            self.paint(&pl, 0..folder, self.pal.fg.with_alpha(0.5));
            let _ = pl.SetFontWeight(
                DWRITE_FONT_WEIGHT_BOLD,
                range(folder..shown.encode_utf16().count() as u32),
            );
            let (tw, th) = render::text_size(&pl);
            let r = Rect::new(px, y, tw.min(room), HEAD);
            s.items.push(rich::Item::Clip(Rect::new(px, y, room, HEAD)));
            s.text(&pl, px, y + (HEAD - th) / 2.0, self.link_ink());
            s.items.push(rich::Item::Unclip);
            s.hit(r, Hit::Tr(Act::Path(e.path.clone(), None)));
        }
    }

    /// A band between hunks, or over the first: dim, with what it stands
    /// for written in it.
    fn fold_band(&self, s: &mut Scene, r: Rect, label: &str, cx: f32) {
        s.fill(r, 0.0, self.pal.accent.with_alpha(0.06));
        s.fill(
            Rect::new(r.x, r.y, r.w, 1.0),
            0.0,
            self.pal.fg.with_alpha(0.05),
        );
        s.fill(
            Rect::new(r.x, r.bottom() - 1.0, r.w, 1.0),
            0.0,
            self.pal.fg.with_alpha(0.05),
        );
        s.line(
            label.to_string(),
            F::Small,
            Rect::new(cx, r.y, (r.right() - cx - 10.0).max(10.0), r.h),
            self.pal.faint,
        );
    }

    /// Colours each row as its file's grammar does. The old side and the
    /// new are each read whole, as GitHub does, so a removed line does not
    /// throw the grammar off for the added lines after it.
    fn colour_rows(&self, l: &IDWriteTextLayout, rows: &[DiffRow], starts: &[u32], path: &str) {
        let (old, new, at) = sides(rows);
        let old = crate::highlight::highlight(path, &old);
        let new = crate::highlight::highlight(path, &new);
        if old.is_empty() && new.is_empty() {
            return;
        }
        for (k, r) in rows.iter().enumerate() {
            let Some((is_old, i)) = at[k] else { continue };
            let Some(spans) = (if is_old { old.get(i) } else { new.get(i) }) else {
                continue;
            };
            let (mut byte, mut pos) = (0usize, starts[k]);
            for sp in spans {
                let end = (byte + sp.len).min(r.text.len());
                let Some(piece) = r.text.get(byte..end) else { break };
                let n = piece.encode_utf16().count() as u32;
                let [cr, cg, cb] = sp.style.fg;
                let c = Color::rgb((cr as u32) << 16 | (cg as u32) << 8 | cb as u32);
                self.paint(l, pos..pos + n, c);
                if sp.style.bold {
                    unsafe {
                        let _ = l.SetFontWeight(DWRITE_FONT_WEIGHT_BOLD, range(pos..pos + n));
                    }
                }
                pos += n;
                byte = end;
            }
        }
    }
}

/// Each source row's top and height in a layout whose rows wrap: a long
/// row takes several lines.
fn row_heights(l: &IDWriteTextLayout) -> Vec<(f32, f32)> {
    let mut n = 0u32;
    let lines = unsafe {
        let _ = l.GetLineMetrics(None, &mut n);
        let mut lines = vec![DWRITE_LINE_METRICS::default(); n as usize];
        if l.GetLineMetrics(Some(&mut lines), &mut n).is_err() {
            return Vec::new();
        }
        lines
    };
    let metrics: Vec<(f32, bool)> = (lines.iter())
        .map(|m| (m.height, m.newlineLength > 0))
        .collect();
    super::code::source_rows(&metrics)
}

/// Where each row starts in the rows joined by newlines, in UTF-16 units,
/// which is what a layout counts in.
fn utf16_starts(rows: &[&str]) -> Vec<u32> {
    let mut at = 0u32;
    (rows.iter())
        .map(|r| {
            let s = at;
            at += r.encode_utf16().count() as u32 + 1;
            s
        })
        .collect()
}

/// How many UTF-16 units of a path are its folder, slash included.
fn folder_len(path: &str) -> u32 {
    path.rfind(['/', '\\'])
        .map_or(0, |i| path[..=i].encode_utf16().count() as u32)
}

/// Lines added and removed.
fn counts(rows: &[DiffRow]) -> (usize, usize) {
    let n = |m: char| rows.iter().filter(|r| r.mark == m).count();
    (n('+'), n('-'))
}

/// GitHub's five squares for a change: green for added, red for removed,
/// in proportion, grey for what a small change does not fill.
fn stat_squares(add: usize, del: usize) -> [char; STAT_SQUARES] {
    let mut out = [' '; STAT_SQUARES];
    let total = add + del;
    if total == 0 {
        return out;
    }
    let (g, r) = if total <= STAT_SQUARES {
        (add, del)
    } else {
        let g = ((add as f32 / total as f32) * STAT_SQUARES as f32).round() as usize;
        // A side with any lines shows at least one square.
        let g = g.clamp(usize::from(add > 0), STAT_SQUARES - usize::from(del > 0));
        (g, STAT_SQUARES - g)
    };
    for (k, c) in out.iter_mut().enumerate() {
        *c = if k < g {
            '+'
        } else if k < g + r {
            '-'
        } else {
            ' '
        };
    }
    out
}

/// The old file's lines and the new's among `rows`, and for each row which
/// side and line it is: removed rows are the old side's, added and kept
/// rows the new side's. Gaps are on neither.
#[allow(clippy::type_complexity)]
fn sides(rows: &[DiffRow]) -> (Vec<String>, Vec<String>, Vec<Option<(bool, usize)>>) {
    let (mut old, mut new, mut at) = (Vec::new(), Vec::new(), Vec::new());
    for r in rows {
        match r.mark {
            '-' => {
                at.push(Some((true, old.len())));
                old.push(r.text.clone());
            }
            '+' => {
                at.push(Some((false, new.len())));
                new.push(r.text.clone());
            }
            '~' => at.push(None),
            _ => {
                at.push(Some((false, new.len())));
                old.push(r.text.clone());
                new.push(r.text.clone());
            }
        }
    }
    (old, new, at)
}

/// The band over the first hunk, when the CLI's hunks say it starts past
/// the top of the file.
fn lead_label(rows: &[DiffRow], hunked: bool) -> Option<String> {
    let first = rows.first()?;
    let line = first.new.or(first.old)?;
    (hunked && line > 1).then(|| format!("@@ line {line}"))
}

/// What the gap at `k` stands for: the lines left out between two hunks
/// of the CLI's, or, between the swaps of one MultiEdit, which swap
/// follows, since where each sits in the file is not known.
fn gap_label(rows: &[DiffRow], k: usize, hunked: bool) -> String {
    if hunked {
        let before = rows[..k].iter().rev().find_map(|r| r.old);
        let after = rows[k + 1..].iter().find_map(|r| r.old);
        let at = rows[k + 1..].iter().find_map(|r| r.new.or(r.old));
        return match (before, after) {
            (Some(b), Some(a)) if a > b + 1 => {
                let n = a - b - 1;
                let s = if n == 1 { "" } else { "s" };
                match at {
                    Some(at) => format!("{n} unchanged line{s} \u{b7} @@ line {at}"),
                    None => format!("{n} unchanged line{s}"),
                }
            }
            _ => at.map_or_else(|| "\u{22ef}".to_string(), |at| format!("@@ line {at}")),
        };
    }
    let gaps = rows.iter().filter(|r| r.mark == '~').count();
    let nth = rows[..k].iter().filter(|r| r.mark == '~').count() + 2;
    format!("Edit {nth} of {}", gaps + 1)
}

/// The removed rows and the added rows that replace them, paired in order:
/// each run of removed rows with the run of added rows straight after it.
fn pairs(rows: &[DiffRow]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut k = 0;
    while k < rows.len() {
        if rows[k].mark != '-' {
            k += 1;
            continue;
        }
        let a = k;
        while k < rows.len() && rows[k].mark == '-' {
            k += 1;
        }
        let b = k;
        while k < rows.len() && rows[k].mark == '+' {
            k += 1;
        }
        out.extend((a..b).zip(b..k));
    }
    out
}

/// A line's tokens: a word, a run of white space, or any other single
/// character, as byte ranges.
fn tokens(s: &str) -> Vec<Range<usize>> {
    let kind = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut last = None;
    for (i, c) in s.char_indices() {
        let k = kind(c);
        match out.last_mut() {
            Some(r) if last == Some(k) && k != 2 => r.end = i + c.len_utf8(),
            _ => out.push(i..i + c.len_utf8()),
        }
        last = Some(k);
    }
    out
}

/// The tokens longer than this on either side are not diffed word by word,
/// since the table grows as the product of the two.
const MAX_TOKENS: usize = 400;

/// What changed between a removed line and the added line that replaces
/// it, as byte ranges in each: the tokens off their longest common run.
/// None when the lines share too little for the spans to help, as GitHub
/// leaves a rewritten line plain.
fn word_spans(old: &str, new: &str) -> Option<(Vec<Range<usize>>, Vec<Range<usize>>)> {
    let (a, b) = (tokens(old), tokens(new));
    if a.len() > MAX_TOKENS || b.len() > MAX_TOKENS || old == new {
        return None;
    }
    let (n, m) = (a.len(), b.len());
    let mut t = vec![0u16; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[at(i, j)] = if old[a[i].clone()] == new[b[j].clone()] {
                t[at(i + 1, j + 1)] + 1
            } else {
                t[at(i + 1, j)].max(t[at(i, j + 1)])
            };
        }
    }
    let (mut keep_a, mut keep_b) = (vec![false; n], vec![false; m]);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[a[i].clone()] == new[b[j].clone()] {
            keep_a[i] = true;
            keep_b[j] = true;
            i += 1;
            j += 1;
        } else if t[at(i + 1, j)] >= t[at(i, j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    let solid = |s: &str, r: &Range<usize>| s[r.clone()].trim().len();
    let shared: usize = (a.iter().zip(&keep_a))
        .filter(|(_, k)| **k)
        .map(|(r, _)| solid(old, r))
        .sum();
    let total = (a.iter().map(|r| solid(old, r)).sum::<usize>())
        .max(b.iter().map(|r| solid(new, r)).sum::<usize>());
    if total == 0 || shared * 10 < total * 4 {
        return None;
    }
    let changed = |toks: &[Range<usize>], keep: &[bool]| {
        let mut out: Vec<Range<usize>> = Vec::new();
        for (r, k) in toks.iter().zip(keep) {
            if *k {
                continue;
            }
            match out.last_mut() {
                Some(last) if last.end == r.start => last.end = r.end,
                _ => out.push(r.clone()),
            }
        }
        out
    };
    Some((changed(&a, &keep_a), changed(&b, &keep_b)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(mark: char, old: Option<u32>, new: Option<u32>, text: &str) -> DiffRow {
        DiffRow {
            old,
            new,
            mark,
            text: text.to_string(),
        }
    }

    #[test]
    fn a_changed_word_is_all_that_is_marked() {
        let (a, b) = word_spans("let x = foo(1);", "let x = bar(1);").unwrap();
        assert_eq!(a, vec![8..11]);
        assert_eq!(b, vec![8..11]);
    }

    #[test]
    fn an_insertion_marks_only_the_new_side() {
        let (a, b) = word_spans("call(a, b)", "call(a, b, c)").unwrap();
        assert!(a.is_empty());
        assert_eq!(&"call(a, b, c)"[b[0].clone()], ", c");
    }

    #[test]
    fn a_rewritten_line_is_left_plain() {
        assert!(word_spans("fn alpha() {}", "let beta = gamma;").is_none());
        assert!(word_spans("same", "same").is_none());
    }

    #[test]
    fn removed_runs_pair_with_the_added_runs_after_them() {
        let rows = [
            row(' ', Some(1), Some(1), "a"),
            row('-', Some(2), None, "b"),
            row('-', Some(3), None, "c"),
            row('+', None, Some(2), "B"),
            row(' ', Some(4), Some(3), "d"),
            row('+', None, Some(4), "e"),
        ];
        assert_eq!(pairs(&rows), vec![(1, 3)]);
    }

    #[test]
    fn a_gap_between_hunks_counts_the_lines_left_out() {
        let rows = [
            row(' ', Some(10), Some(10), "a"),
            row('~', None, None, ""),
            row(' ', Some(20), Some(21), "b"),
        ];
        assert_eq!(gap_label(&rows, 1, true), "9 unchanged lines \u{b7} @@ line 21");
        assert_eq!(gap_label(&rows, 1, false), "Edit 2 of 2");
    }

    #[test]
    fn the_first_hunk_says_where_it_starts() {
        let rows = [row(' ', Some(40), Some(42), "a")];
        assert_eq!(lead_label(&rows, true).as_deref(), Some("@@ line 42"));
        assert_eq!(lead_label(&rows, false), None);
    }

    #[test]
    fn each_side_reads_its_own_lines() {
        let rows = [
            row(' ', Some(1), Some(1), "k"),
            row('-', Some(2), None, "old"),
            row('+', None, Some(2), "new"),
            row('~', None, None, ""),
        ];
        let (old, new, at) = sides(&rows);
        assert_eq!(old, ["k", "old"]);
        assert_eq!(new, ["k", "new"]);
        assert_eq!(at, [Some((false, 0)), Some((true, 1)), Some((false, 1)), None]);
    }

    #[test]
    fn squares_split_in_proportion_and_keep_one_for_each_side() {
        assert_eq!(stat_squares(2, 1), ['+', '+', '-', ' ', ' ']);
        assert_eq!(stat_squares(100, 1), ['+', '+', '+', '+', '-']);
        assert_eq!(stat_squares(0, 0), [' '; 5]);
    }

    #[test]
    fn the_folder_is_everything_to_the_last_slash() {
        assert_eq!(folder_len("crates/ui/src/diff.rs"), 14);
        assert_eq!(folder_len("diff.rs"), 0);
    }
}
