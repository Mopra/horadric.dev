//! A markdown table in a reply, set as a data table: no box and no
//! stripes, hairlines between rows, a small label for a header, figures
//! right aligned and in step, short statuses as pills.
//!
//! A table too wide to set at a readable width slides sideways inside its
//! own frame rather than squeezing every column to a sliver. Where it has
//! slid to is kept here by the table's text, as code folds are, and the
//! transcript builds a reply again when it moves.

use std::cell::{Cell, RefCell};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use horadric_core::markdown::{Align, Inline};
use windows::core::Interface;
use windows::Win32::Graphics::DirectWrite::{
    IDWriteTextLayout, IDWriteTextLayout1, DWRITE_FONT_FEATURE,
    DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, DWRITE_FONT_WEIGHT_MEDIUM,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_LINE_METRICS, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_WORD, DWRITE_WORD_WRAPPING_EMERGENCY_BREAK,
};

use super::flow::{self, Kind, Para};
use super::rich::{first_line, range, Ctx, Item, Scene};
use super::transcript::Act;
use super::view::Hit;
use crate::folio_look as look;
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
/// The narrowest a table column gets when it cannot slide.
const CELL_LEAST: f32 = 64.0;
/// The narrowest a column is squeezed to before the table slides
/// instead: about a dozen characters, so a word or a figure still reads.
const READ_LEAST: f32 = 96.0;
/// A sliding table has room to spare, so its wide columns wrap at a
/// comfortable measure rather than running a line across many screens.
const WIDE_MOST: f32 = 280.0;
/// A cell taller than this many lines is cut to them, with an ellipsis:
/// one long note must not make its row a page tall. Copy has it whole.
const CELL_LINES: usize = 8;
/// How far a notch of the wheel slides a table.
const NOTCH_X: f32 = 72.0;
/// The scroll indicator under a sliding table: its room above, its height.
const BAR_GAP: f32 = 6.0;
const BAR_H: f32 = 3.0;
/// How far the indicator's grab band reaches past it above and below:
/// the bar stays thin, the place to grab it does not.
pub(super) const BAR_REACH: f32 = 6.0;
/// The shortest the indicator's thumb gets, so it stays easy to see.
const THUMB_LEAST: f32 = 28.0;
/// The fade at an edge the table runs on past, in strips of solid colour
/// since a scene has no gradients.
const FADE_W: f32 = 28.0;
const FADE_STEPS: usize = 7;
/// The header label: smaller than the body and a touch tracked, so it
/// reads as a label over the data rather than a row of it.
const HEAD_SIZE: f32 = 12.5;
const HEAD_TRACK: f32 = 0.4;
/// A status pill's text size, its height and its padding either side.
const PILL_SIZE: f32 = 12.5;
const PILL_H: f32 = 20.0;
const PILL_X: f32 = 8.0;
/// The hover Copy button's size.
const COPY_W: f32 = 76.0;
const COPY_H: f32 = 22.0;

/// Where a sliding table is: how far it has slid, how far it can, its
/// frame in its own scene, and whether it moved since the transcript last
/// built what shows.
#[derive(Debug, Clone, Copy, Default)]
struct Slide {
    at: f32,
    most: f32,
    /// How wide its columns are in all.
    content: f32,
    frame: Rect,
    moved: bool,
}

thread_local! {
    /// The tables wider than their view, by [`slide_key`].
    static SLIDES: RefCell<HashMap<String, Slide>> = RefCell::new(HashMap::new());
    /// Whether anything can slide a table yet. Until the transcript calls
    /// [`settle`], a table squeezes to fit as it always did, since one
    /// wider than the view with no wheel to reach it would hide columns.
    static LIVE: Cell<bool> = const { Cell::new(false) };
}

/// The key a table slides by, from its text: the same table twice slides
/// together, which reads as one thing anyway.
pub(super) fn slide_key(header: &[Inline], rows: &[Vec<Inline>]) -> String {
    let mut h = DefaultHasher::new();
    for cell in header.iter().chain(rows.iter().flatten()) {
        cell.text.hash(&mut h);
    }
    (header.len(), rows.len()).hash(&mut h);
    format!("table:{:016x}", h.finish())
}

/// Slides table `key` by `by` pixels, right for more than nought. False
/// when it did not move: it is not wider than its view, or already at
/// that end, so the caller can leave the input to the page.
pub(super) fn slide(key: &str, by: f32) -> bool {
    slide_with(key, |s| s.at + by)
}

/// Slides table `key` so its indicator's thumb starts `left` into the
/// track, as a drag of the thumb does. False when it did not move.
pub(super) fn slide_thumb(key: &str, left: f32) -> bool {
    slide_with(key, |s| thumb_at(left, s.most, s.frame.w, s.content))
}

/// Table `key`'s indicator thumb, its left and width within the track.
pub(super) fn thumb_of(key: &str) -> Option<(f32, f32)> {
    SLIDES.with(|m| {
        let m = m.borrow();
        let s = m.get(key)?;
        Some(thumb(s.at, s.most, s.frame.w, s.content))
    })
}

fn slide_with(key: &str, to: impl FnOnce(&Slide) -> f32) -> bool {
    SLIDES.with(|m| {
        let mut m = m.borrow_mut();
        let Some(s) = m.get_mut(key) else {
            return false;
        };
        let at = to(s).clamp(0.0, s.most);
        if (at - s.at).abs() < 0.01 {
            return false;
        }
        s.at = at;
        s.moved = true;
        true
    })
}

/// The wheel over table `key`, in Windows' units of 120 a notch, more
/// than nought sliding it right.
pub(super) fn wheel(key: &str, delta: i32) -> bool {
    slide(key, delta as f32 / 120.0 * NOTCH_X)
}

/// Where every table has slid, as one number: a reply is built again when
/// it changes.
pub(super) fn slide_print() -> u64 {
    SLIDES.with(|m| {
        // Xor, so the map's order does not matter.
        (m.borrow().iter())
            .filter(|(_, s)| s.at > 0.0)
            .map(|(k, s)| {
                let mut h = DefaultHasher::new();
                (k, s.at.to_bits()).hash(&mut h);
                h.finish()
            })
            .fold(0, |a, b| a ^ b)
    })
}

/// Whether a paragraph kept from the last build is a table that has slid
/// since, and so must be built again.
pub(super) fn stale(p: &Para) -> bool {
    let Kind::Table { header, rows, .. } = &p.kind else {
        return false;
    };
    let key = slide_key(header, rows);
    SLIDES.with(|m| m.borrow().get(&key).is_some_and(|s| s.moved))
}

/// Called once the transcript has built what shows: every table that
/// moved is drawn where it now is. It is also what turns sliding on.
pub(super) fn settle() {
    LIVE.with(|l| l.set(true));
    SLIDES.with(|m| m.borrow_mut().values_mut().for_each(|s| s.moved = false));
}

/// The key and frame of a table just built, in its own scene, when it is
/// wider than its view: what the wheel over that frame slides.
pub(super) fn slider(header: &[Inline], rows: &[Vec<Inline>]) -> Option<(String, Rect)> {
    let key = slide_key(header, rows);
    let frame = SLIDES.with(|m| m.borrow().get(&key).map(|s| s.frame))?;
    Some((key, frame))
}

/// Notes a table being built at width `w` with its columns `total_w`
/// wide: how far it can slide now, keeping where it was within that.
/// Returns how far it has slid. A table that fits is forgotten.
fn place(key: &str, total_w: f32, w: f32) -> f32 {
    SLIDES.with(|m| {
        let mut m = m.borrow_mut();
        let most = (total_w - w).max(0.0);
        if most <= 0.0 {
            m.remove(key);
            return 0.0;
        }
        let s = m.entry(key.to_string()).or_default();
        s.most = most;
        s.content = total_w;
        s.at = s.at.clamp(0.0, most);
        s.at
    })
}

/// The frame a sliding table was drawn in, once its height is known.
fn set_frame(key: &str, frame: Rect) {
    SLIDES.with(|m| {
        if let Some(s) = m.borrow_mut().get_mut(key) {
            s.frame = frame;
        }
    });
}

/// Column widths for a table whose columns want `natural` in a view `w`
/// wide, and whether it slides: columns that fit keep their width; when
/// they do not, the wide ones wrap, none below a readable width. Only
/// when even that is too wide, and the table can slide, does it run past
/// the view, each column at what it wants up to a comfortable measure.
fn plan(natural: &[f32], w: f32, can_slide: bool) -> (Vec<f32>, bool) {
    let cols = natural.len();
    let total: f32 = natural.iter().sum();
    if total <= w || cols == 0 {
        return (natural.to_vec(), false);
    }
    let floor: f32 = natural.iter().map(|n| n.min(READ_LEAST)).sum();
    if floor <= w {
        return (fit(&flow::table_widths(natural, READ_LEAST, w), READ_LEAST, w), false);
    }
    // One column has nothing beside it to make room for, so wrapping it
    // to the view beats sliding it.
    if !can_slide || cols < 2 {
        let least = CELL_LEAST.min(w / cols as f32);
        return (fit(&flow::table_widths(natural, least, w), least, w), false);
    }
    (natural.iter().map(|n| n.min(WIDE_MOST)).collect(), true)
}

/// The scroll indicator's thumb under a table `content` wide seen through
/// a view `view` wide, slid `at` of `most`: its left and width.
fn thumb(at: f32, most: f32, view: f32, content: f32) -> (f32, f32) {
    if content <= view || most <= 0.0 {
        return (0.0, view.max(0.0));
    }
    let tw = (view * view / content).clamp(THUMB_LEAST.min(view), view);
    let k = (at / most).clamp(0.0, 1.0);
    (k * (view - tw), tw)
}

/// Where a table slides to with its thumb dragged to `left`, the inverse
/// of [`thumb`]: the thumb travels less than the table does, so a pixel
/// of drag moves the table by the ratio of the two.
fn thumb_at(left: f32, most: f32, view: f32, content: f32) -> f32 {
    let (_, tw) = thumb(0.0, most, view, content);
    let travel = view - tw;
    if travel <= 0.0 {
        return 0.0;
    }
    (left / travel).clamp(0.0, 1.0) * most
}

/// The height a cell whose lines are `lines` tall is cut to, or None when
/// it is short enough to show whole.
fn capped(lines: &[f32], most: usize) -> Option<f32> {
    (lines.len() > most).then(|| lines[..most].iter().sum())
}

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
        let can_slide = LIVE.with(Cell::get);
        let (widths, wide) = plan(&natural, w, can_slide);
        let total_w: f32 = widths.iter().sum();
        let key = slide_key(header, rows);
        // A table that fits is set from the left; one that slides is drawn
        // moved by as far as it has slid, inside a frame the view wide.
        let at = if can_slide { place(&key, total_w, w) } else { 0.0 };
        let lefts: Vec<f32> = widths
            .iter()
            .scan(-at, |x, cw| {
                let left = *x;
                *x += cw;
                Some(left)
            })
            .collect();
        let rule_w = if wide { w } else { total_w };
        // Every status pill sits level with the first line of body text.
        let line = self
            .one_line(&self.fonts.body, "Ag")
            .and_then(|l| first_line(&l))
            .unwrap_or(PILL_H);

        // The hover Copy button covers nothing: beside the table when it
        // leaves the room, else in a slim margin kept above it.
        let top = copy_top(w, total_w);
        let first_item = s.items.len();
        let first_hit = s.hits.len();

        // The header: a dim label, then a rule a shade firmer than the
        // hairlines between rows, which is all that sets it apart.
        let mut y = top;
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
                (widths[c] - pl - pr).max(0.0),
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
            Rect::new(0.0, y - 1.0, rule_w, 1.0),
            0.0,
            self.pal.fg.with_alpha(0.16),
        );

        for (k, row) in rows.iter().enumerate() {
            let mut rh: f32 = 0.0;
            for (c, inl) in row.iter().enumerate().take(cols) {
                let (pl, pr) = pad(c);
                let x = lefts[c] + pl;
                let cw = (widths[c] - pl - pr).max(0.0);
                let t = inl.text.as_str();
                // A column squeezed narrower than a pill's own padding
                // shows its status as plain text.
                let pill = status(t).filter(|_| cw >= 2.0 * PILL_X);
                if let Some((tone, label)) = pill {
                    let at = (x, y + CELL_Y, cw, line);
                    rh = rh.max(self.pill(&mut s, tone, &label, at, aligns[c]));
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
                let drawn = s.items.len();
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
                        let (_, mut nh) = self.fit_cell(&u.layout);
                        if let Some(cut) = capped(&line_heights(&u.layout), CELL_LINES) {
                            self.cut(&u.layout, cut);
                            nh = cut;
                            // The lines past the cut are still in the
                            // layout, so the cell is clipped to it too.
                            let clip = Rect::new(x, y + CELL_Y, cw, cut);
                            s.items.insert(drawn, Item::Clip(clip));
                            s.items.push(Item::Unclip);
                        }
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
                    Rect::new(0.0, y - 1.0, rule_w, 1.0),
                    0.0,
                    self.pal.fg.with_alpha(0.07),
                );
            }
        }

        if wide {
            let frame = Rect::new(0.0, top, w, y - top);
            self.frame(&mut s, &key, frame, (first_item, first_hit), (at, total_w - w), total_w);
            set_frame(&key, frame);
            y += BAR_GAP + BAR_H + 2.0;
        }

        self.copy_button(&mut s, header, rows, w, top, hh);
        s.h = y + 4.0;
        s
    }

    /// A sliding table's frame: what was drawn from `first` on clipped to
    /// it, the links in it kept to what shows, a fade at each edge it runs
    /// on past, and a thin indicator of where it is under it.
    unsafe fn frame(
        &self,
        s: &mut Scene,
        key: &str,
        frame: Rect,
        (first_item, first_hit): (usize, usize),
        (at, most): (f32, f32),
        content: f32,
    ) {
        s.items.insert(first_item, Item::Clip(frame));
        let bg = self.pal.bg;
        let strip = FADE_W / FADE_STEPS as f32;
        for i in 0..FADE_STEPS {
            // Strongest at the edge, gone by the fade's inner end.
            let k = (i + 1) as f32 / FADE_STEPS as f32;
            let a = 0.92 * k * k;
            if at > 0.5 {
                let x = frame.x + FADE_W - (i + 1) as f32 * strip;
                s.fill(Rect::new(x, frame.y, strip, frame.h), 0.0, bg.with_alpha(a));
            }
            if at < most - 0.5 {
                let x = frame.right() - FADE_W + i as f32 * strip;
                s.fill(Rect::new(x, frame.y, strip, frame.h), 0.0, bg.with_alpha(a));
            }
        }
        s.items.push(Item::Unclip);
        // A link slid out of view must not take a click where it was.
        let kept: Vec<(Rect, Hit)> = (s.hits.drain(first_hit..))
            .filter_map(|(r, h)| Some((look::clip(&r, &frame)?, h)))
            .collect();
        s.hits.extend(kept);

        let bar_y = frame.bottom() + BAR_GAP;
        s.fill(
            Rect::new(frame.x, bar_y, frame.w, BAR_H),
            BAR_H / 2.0,
            self.pal.fg.with_alpha(0.05),
        );
        let (tx, tw) = thumb(at, most, frame.w, content);
        s.fill(
            Rect::new(frame.x + tx, bar_y, tw, BAR_H),
            BAR_H / 2.0,
            self.pal.fg.with_alpha(0.26),
        );
        // The thumb is dragged, and a press beside it jumps there.
        let band = Rect::new(frame.x, bar_y - BAR_REACH, frame.w, BAR_H + 2.0 * BAR_REACH);
        s.hit(band, Hit::Tr(Act::TableBar(key.to_string())));
    }

    /// A cell cut to its first lines `h` tall, an ellipsis ending the last
    /// one so it reads as cut rather than as all there is.
    unsafe fn cut(&self, l: &IDWriteTextLayout, h: f32) {
        let _ = l.SetMaxHeight(h + 0.5);
        let trimming = DWRITE_TRIMMING {
            granularity: DWRITE_TRIMMING_GRANULARITY_WORD,
            delimiter: 0,
            delimiterCount: 0,
        };
        if let Ok(sign) = self.gpu.dw.CreateEllipsisTrimmingSign(&self.fonts.body) {
            let _ = l.SetTrimming(&trimming, &sign);
        }
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
    /// line of body text, `line` tall, so it sits level with the cells
    /// beside it. Returns the height it takes, which is that line's.
    unsafe fn pill(
        &self,
        s: &mut Scene,
        tone: Tone,
        label: &str,
        (x, y, cw, line): (f32, f32, f32, f32),
        align: Align,
    ) -> f32 {
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
    /// spreadsheet as cells. It sits at the right, level with the header
    /// when the table leaves room beside it, else in the margin `top`
    /// kept above the table, so it never covers a label.
    unsafe fn copy_button(
        &self,
        s: &mut Scene,
        header: &[Inline],
        rows: &[Vec<Inline>],
        w: f32,
        top: f32,
        hh: f32,
    ) {
        let copy = copy_rect(w, top, hh);
        let head: Vec<&str> = header.iter().map(|i| i.text.as_str()).collect();
        let body: Vec<Vec<&str>> = rows
            .iter()
            .map(|r| r.iter().map(|i| i.text.as_str()).collect())
            .collect();
        let text = tsv(&head, &body);
        s.hover.extend(self.copy_face(copy, &text, self.pal.dim));
        s.hover_hits.push((copy, Hit::Tr(Act::Copy(text))));
    }
}

/// The margin kept above a table for its Copy button: none when the
/// button fits beside the table, right of its last column.
fn copy_top(w: f32, total_w: f32) -> f32 {
    if w - total_w >= COPY_W + 8.0 {
        0.0
    } else {
        COPY_H + 6.0
    }
}

/// Where the Copy button goes: at the right, never left of the scene or
/// above its top, level with the header when there is no margin for it.
fn copy_rect(w: f32, top: f32, hh: f32) -> Rect {
    let cw = COPY_W.min(w.max(0.0));
    let y = if top > 0.0 { 0.0 } else { ((hh - COPY_H) / 2.0).max(0.0) };
    Rect::new((w - cw).max(0.0), y, cw, COPY_H)
}

fn whole(text: &str) -> windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_RANGE {
    range(0..text.encode_utf16().count() as u32)
}

/// How tall each of a layout's lines is.
fn line_heights(l: &IDWriteTextLayout) -> Vec<f32> {
    let mut n = 0u32;
    unsafe {
        let _ = l.GetLineMetrics(None, &mut n);
        let mut lines = vec![DWRITE_LINE_METRICS::default(); n as usize];
        if n == 0 || l.GetLineMetrics(Some(&mut lines), &mut n).is_err() {
            return Vec::new();
        }
        lines.iter().map(|m| m.height).collect()
    }
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
    fn table_copy_button_covers_nothing() {
        // Room beside the table: level with the header, right of it.
        let top = copy_top(600.0, 300.0);
        assert_eq!(top, 0.0);
        let r = copy_rect(600.0, top, 17.0);
        assert!(r.x >= 300.0 + 8.0 && r.y >= 0.0);
        // A full width table: a margin above it holds the button.
        let top = copy_top(600.0, 600.0);
        let r = copy_rect(600.0, top, 17.0);
        assert!(r.y >= 0.0 && r.y + r.h <= top);
        // Narrower than the button: it stays inside the scene.
        let r = copy_rect(40.0, copy_top(40.0, 40.0), 17.0);
        assert!(r.x >= 0.0 && r.x + r.w <= 40.01);
    }

    #[test]
    fn table_fit_never_overflows() {
        let w = fit(&[500.0, 2000.0], 64.0, 300.0);
        assert!(w.iter().sum::<f32>() <= 300.01);
        assert!(w.iter().all(|x| *x >= 64.0 - 0.01));
        assert_eq!(fit(&[100.0, 50.0], 64.0, 300.0), vec![100.0, 50.0]);
    }

    #[test]
    fn table_plan_keeps_what_fits() {
        assert_eq!(plan(&[100.0, 50.0], 300.0, true), (vec![100.0, 50.0], false));
    }

    #[test]
    fn table_plan_wraps_wide_columns_to_a_readable_width() {
        // A long column gives up room; the short one keeps its own.
        let (w, wide) = plan(&[60.0, 900.0, 120.0], 500.0, true);
        assert!(!wide);
        assert!(w.iter().sum::<f32>() <= 500.01);
        assert_eq!(w[0], 60.0);
        assert!(w.iter().all(|x| *x >= 60.0 - 0.01));
        assert!(w[1] >= READ_LEAST);
    }

    #[test]
    fn table_plan_slides_when_columns_would_be_unreadable() {
        let natural = vec![150.0; 20];
        let (w, wide) = plan(&natural, 600.0, true);
        assert!(wide);
        assert!(w.iter().all(|x| *x >= READ_LEAST));
        assert!(w.iter().sum::<f32>() > 600.0);
        // A wide column wraps at a comfortable measure even then.
        let (w, _) = plan(&[2000.0, 150.0, 150.0, 150.0, 150.0], 400.0, true);
        assert_eq!(w[0], WIDE_MOST);
        // Short columns keep their own width.
        let (w, wide) = plan(&[40.0, 500.0, 500.0, 500.0], 200.0, true);
        assert!(wide && w[0] == 40.0);
    }

    #[test]
    fn table_plan_squeezes_when_it_cannot_slide() {
        let natural = vec![150.0; 20];
        let (w, wide) = plan(&natural, 600.0, false);
        assert!(!wide);
        assert!(w.iter().sum::<f32>() <= 600.01);
        // One column wraps to the view rather than sliding.
        let (w, wide) = plan(&[900.0], 60.0, true);
        assert!(!wide && w[0] <= 60.01);
    }

    #[test]
    fn table_thumb_tracks_the_slide() {
        let (x, w) = thumb(0.0, 600.0, 400.0, 1000.0);
        assert_eq!(x, 0.0);
        assert!((w - 160.0).abs() < 0.01);
        let (x, w) = thumb(600.0, 600.0, 400.0, 1000.0);
        assert!((x + w - 400.0).abs() < 0.01);
        // A very wide table still has a thumb worth seeing.
        let (_, w) = thumb(0.0, 9600.0, 400.0, 10000.0);
        assert_eq!(w, THUMB_LEAST);
        assert_eq!(thumb(0.0, 0.0, 400.0, 300.0), (0.0, 400.0));
    }

    #[test]
    fn table_thumb_drag_maps_to_the_slide() {
        // 1000 wide through 400: the thumb is 160 and travels 240 while
        // the table travels 600, so a pixel of thumb is 2.5 of table.
        assert_eq!(thumb_at(0.0, 600.0, 400.0, 1000.0), 0.0);
        assert!((thumb_at(100.0, 600.0, 400.0, 1000.0) - 250.0).abs() < 0.01);
        assert_eq!(thumb_at(240.0, 600.0, 400.0, 1000.0), 600.0);
        // Past either end it stays at the end.
        assert_eq!(thumb_at(-50.0, 600.0, 400.0, 1000.0), 0.0);
        assert_eq!(thumb_at(900.0, 600.0, 400.0, 1000.0), 600.0);
        // It is the inverse of where the thumb is drawn.
        for at in [0.0, 137.0, 420.0, 600.0] {
            let (x, _) = thumb(at, 600.0, 400.0, 1000.0);
            assert!((thumb_at(x, 600.0, 400.0, 1000.0) - at).abs() < 0.01);
        }
        // A table that fits has nowhere to go.
        assert_eq!(thumb_at(30.0, 0.0, 400.0, 300.0), 0.0);
    }

    #[test]
    fn table_tall_cells_are_cut() {
        assert_eq!(capped(&[20.0; 5], 8), None);
        assert_eq!(capped(&[20.0; 12], 8), Some(160.0));
    }

    #[test]
    fn table_slide_stays_in_bounds() {
        let key = "table:test";
        assert!(!slide(key, 50.0), "an unknown table does not slide");
        assert_eq!(place(key, 1000.0, 400.0), 0.0);
        assert!(slide(key, 250.0));
        assert!(slide(key, 9999.0));
        assert!(!slide(key, 10.0), "already at the right end");
        assert_eq!(place(key, 1000.0, 400.0), 600.0);
        // Wider view: it keeps its place within the new reach.
        assert_eq!(place(key, 1000.0, 700.0), 300.0);
        assert!(wheel(key, -120));
        assert!((place(key, 1000.0, 700.0) - (300.0 - NOTCH_X)).abs() < 0.01);
        let before = slide_print();
        assert_ne!(before, 0);
        settle();
        // Once it fits it is forgotten.
        assert_eq!(place(key, 500.0, 700.0), 0.0);
        assert!(!slide(key, 10.0));
        assert_eq!(slide_print(), 0);
    }
}
