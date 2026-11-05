//! The cards that wait on the human: a permission ask (allow once,
//! always, or deny with what to do instead), a question with its options,
//! and a plan to approve or send back. Each is built into a [`Scene`]
//! like any block; what is typed into their fields lives in [`State`].

use horadric_core::folio::{AskState, Block, FileEdit, SignInState};
use windows::Win32::Graphics::DirectWrite::DWRITE_WORD_WRAPPING_EMERGENCY_BREAK;

use super::comments::{self, Review};
use super::flow;
use super::rich::{Ctx, Scene, F};
use super::transcript::{
    change_field, deny_field, keep_field, note_field, other_field, Act, State,
};
use super::view::{Hit, CARET_W};
use crate::field::utf16_at;
use crate::folio_look as look;
use crate::layout::Rect;
use crate::render;
use crate::theme::{self, Color};

pub(super) const PAD: f32 = 14.0;
pub(super) const BUTTON_H: f32 = 30.0;
/// The most of an ask's input a card shows before "Show all".
const ASK_MAX_H: f32 = 220.0;

/// A button `w` wide ending at `right`, filled when it is the one to
/// press. Returns its left edge.
pub(super) fn button(
    cx: &Ctx,
    s: &mut Scene,
    right: f32,
    y: f32,
    label: &str,
    primary: bool,
    hit: Hit,
) -> f32 {
    let w = label.chars().count() as f32 * 7.4 + 30.0;
    let r = Rect::new(right - w, y, w, BUTTON_H);
    if primary {
        s.fill(r, 8.0, cx.pal.fg.with_alpha(0.92));
        s.line(label, F::Button, r, cx.pal.bg);
    } else {
        s.stroke(r, 8.0, cx.pal.fg.with_alpha(0.32), 1.0);
        s.line(label, F::Button, r, cx.pal.fg);
    }
    s.hit(r, hit);
    r.x
}

/// A text field with its caret when it has the keyboard. Returns its
/// bottom.
#[allow(clippy::too_many_arguments)]
pub(super) fn field(
    cx: &Ctx,
    s: &mut Scene,
    st: &State,
    id: &str,
    hint: &str,
    x: f32,
    y: f32,
    w: f32,
) -> f32 {
    let text = st.text(id);
    let focused = st.keys && st.focus.as_deref() == Some(id);
    let inner = w - 20.0;
    let shown = if text.is_empty() { hint } else { text };
    let Some(l) = cx.layout(&cx.fonts.body, shown, inner) else {
        return y;
    };
    let (_, th) = render::text_size(&l);
    let r = Rect::new(x, y, w, th.max(20.0) + 14.0);
    s.fill(r, 8.0, Color::rgb(0).with_alpha(0.25));
    s.stroke(r, 8.0, cx.pal.fg.with_alpha(0.18), 1.0);
    let ink = if text.is_empty() {
        cx.pal.faint
    } else {
        cx.pal.fg
    };
    s.text(&l, x + 10.0, y + 7.0, ink);
    if focused && st.caret_lit {
        let f = st.fields.get(id);
        let at = f.map_or(0, |f| utf16_at(&f.text, f.caret));
        let c = if text.is_empty() {
            Rect::new(0.0, 0.0, 1.0, th.max(18.0))
        } else {
            render::caret_at(&l, at)
        };
        s.fill(
            Rect::new(x + 10.0 + c.x, y + 7.0 + c.y, CARET_W, c.h),
            0.0,
            cx.pal.caret(),
        );
    }
    s.hit(r, Hit::Tr(Act::Field(id.to_string())));
    r.bottom()
}

pub(super) fn plate(cx: &Ctx, s: &mut Scene, w: f32, h: f32, waiting: bool) {
    let card = Rect::new(0.0, 0.0, w, h);
    s.items.insert(
        0,
        super::rich::Item::Fill(card, 12.0, cx.pal.bg.mix(cx.pal.fg, 0.05)),
    );
    let edge = if waiting {
        theme::waiting().with_alpha(0.75)
    } else {
        cx.pal.faint.with_alpha(0.3)
    };
    s.stroke(card, 12.0, edge, 1.2);
}

/// Allow, always, deny: what Claude wants to do, with the buttons.
pub(super) unsafe fn ask(cx: &Ctx, b: &Block, st: &State, w: f32) -> Scene {
    let mut s = Scene::default();
    let Block::Ask {
        request_id,
        tool,
        shown,
        input,
        suggestions,
        state,
        ..
    } = b
    else {
        return s;
    };
    let inner = w - 2.0 * PAD;
    s.line(
        look::ask_title(tool),
        F::Title,
        Rect::new(PAD, PAD, inner, 20.0),
        cx.pal.fg,
    );
    let mut y = PAD + 26.0;
    let waiting = *state == AskState::Waiting;
    // An edit shows what it changes, as its step will; anything else all
    // it acts on, every line, so nothing asked about is out of sight.
    let edit = FileEdit::from_input(tool, input).filter(|_| waiting);
    let changing = st.changing.contains(request_id);
    let denying = st.denying.contains(request_id);
    let none = Review::default();
    let review = st.reviews.get(request_id).unwrap_or(&none);
    // Comments are made only while nothing else is open on the card.
    let commenting = edit.is_some() && waiting && !changing && !denying;
    let mut rows = Vec::new();
    if let Some(e) = &edit {
        let open = commenting.then_some(review);
        y = cx.diff(&mut s, request_id, e, PAD, y, inner, st, open) + 10.0;
        rows = flow::diff_rows(e);
        if !changing {
            y = notes(cx, &mut s, request_id, &rows, review, commenting, y, inner);
        }
    } else if let Some(l) = cx.layout(&cx.gfx.mono, shown, inner) {
        let _ = l.SetWordWrapping(DWRITE_WORD_WRAPPING_EMERGENCY_BREAK);
        let (_, lh) = render::text_size(&l);
        let all = st.all.contains(request_id);
        let cut = lh > ASK_MAX_H && !all;
        let h = if cut { ASK_MAX_H } else { lh };
        let r = Rect::new(PAD - 2.0, y - 2.0, inner + 4.0, h + 6.0);
        s.fill(r, 6.0, Color::rgb(0).with_alpha(0.25));
        s.items.push(super::rich::Item::Clip(r));
        s.unit(&l, PAD, y + 1.0, cx.pal.fg.with_alpha(0.85), shown);
        s.items.push(super::rich::Item::Unclip);
        y = r.bottom() + 4.0;
        if lh > ASK_MAX_H {
            let lines = shown.lines().count();
            let say = if all {
                "Show less".to_string()
            } else {
                format!("Show all {lines} lines")
            };
            let link = Rect::new(PAD, y, inner, 20.0);
            s.line(say, F::Small, link, cx.pal.accent.mix(cx.pal.fg, 0.45));
            s.hit(link, Hit::Tr(Act::All(request_id.clone())));
            y += 20.0;
        }
        y += 8.0;
    }
    let right = w - PAD;
    let always = flow::always_label(suggestions);
    match state {
        AskState::Waiting if changing => {
            let id = change_field(request_id);
            let hint = "What it writes instead";
            y = field(cx, &mut s, st, &id, hint, PAD, y, inner) + 10.0;
            let say = "Ctrl+Enter to allow it as changed";
            s.line(say, F::Small, Rect::new(PAD, y, inner, 18.0), cx.pal.faint);
            let x = button(
                cx,
                &mut s,
                right,
                y,
                "Allow as changed",
                true,
                Hit::Tr(Act::Change(request_id.clone())),
            );
            button(
                cx,
                &mut s,
                x - 8.0,
                y,
                "Cancel",
                false,
                Hit::Tr(Act::Cancel(request_id.clone())),
            );
            y += BUTTON_H;
        }
        AskState::Waiting if denying => {
            let id = deny_field(request_id);
            let hint = match review.comments.len() {
                0 => "What should be done instead? (optional)",
                _ => "Anything to add to your comments? (optional)",
            };
            y = field(cx, &mut s, st, &id, hint, PAD, y, inner) + 10.0;
            let x = button(
                cx,
                &mut s,
                right,
                y,
                "Deny",
                true,
                Hit::Tr(Act::Deny(request_id.clone())),
            );
            button(
                cx,
                &mut s,
                x - 8.0,
                y,
                "Cancel",
                false,
                Hit::Tr(Act::Cancel(request_id.clone())),
            );
            y += BUTTON_H;
        }
        AskState::Waiting if commenting && review.draft.is_some() => {
            let (from, to) = review.span().unwrap_or_default();
            let say = format!(
                "Comment on {}",
                comments::shown_place(&rows, from, to).to_lowercase()
            );
            s.line(say, F::Small, Rect::new(PAD, y, inner, 18.0), cx.pal.dim);
            y += 20.0;
            let id = note_field(request_id);
            let hint = "What should change here?";
            y = field(cx, &mut s, st, &id, hint, PAD, y, inner) + 10.0;
            let say =
                "Enter adds it, Shift+Enter for a new line, Shift+click a number for more lines";
            s.line(
                say,
                F::Small,
                Rect::new(PAD, y + 34.0, inner, 18.0),
                cx.pal.faint,
            );
            let x = button(
                cx,
                &mut s,
                right,
                y,
                "Add comment",
                true,
                Hit::Tr(Act::Note(request_id.clone())),
            );
            button(
                cx,
                &mut s,
                x - 8.0,
                y,
                "Cancel",
                false,
                Hit::Tr(Act::Unnote(request_id.clone(), None)),
            );
            y += BUTTON_H + 22.0;
        }
        AskState::Waiting => {
            if let Some(label) = &always {
                let say = format!("Always allow: {label}");
                s.line(say, F::Small, Rect::new(PAD, y, inner, 18.0), cx.pal.faint);
                y += 24.0;
            }
            let mut x = right + 8.0;
            let notes = review.comments.len();
            if notes > 0 {
                let label = match notes {
                    1 => "Send 1 comment".to_string(),
                    n => format!("Send {n} comments"),
                };
                let send = Hit::Tr(Act::SendNotes(request_id.clone()));
                x = button(cx, &mut s, x - 8.0, y, &label, true, send);
            }
            let allow = Hit::Answer(request_id.clone(), true);
            x = button(cx, &mut s, x - 8.0, y, "Allow", notes == 0, allow);
            if always.is_some() {
                x = button(
                    cx,
                    &mut s,
                    x - 8.0,
                    y,
                    "Always allow",
                    false,
                    Hit::Tr(Act::Always(request_id.clone())),
                );
            }
            if horadric_core::folio::edit_text(tool, input).is_some() {
                x = button(
                    cx,
                    &mut s,
                    x - 8.0,
                    y,
                    "Change",
                    false,
                    Hit::Tr(Act::Change(request_id.clone())),
                );
            }
            button(
                cx,
                &mut s,
                x - 8.0,
                y,
                "Deny",
                false,
                Hit::Tr(Act::Deny(request_id.clone())),
            );
            y += BUTTON_H;
        }
        _ => {
            let (icon, ink) = settled_mark(cx, *state);
            let said = look::ask_said(*state);
            y = outcome(cx, &mut s, icon, ink, said, cx.pal.dim, y, inner);
        }
    }
    let h = y + PAD;
    plate(cx, &mut s, w, h, *state == AskState::Waiting);
    s.h = h;
    s
}

/// The comments on an edit ask's lines, each with where it is and a way
/// to drop it, or how to make one when there are none. Returns the bottom.
#[allow(clippy::too_many_arguments)]
unsafe fn notes(
    cx: &Ctx,
    s: &mut Scene,
    request_id: &str,
    rows: &[flow::DiffRow],
    review: &Review,
    commenting: bool,
    mut y: f32,
    inner: f32,
) -> f32 {
    if review.comments.is_empty() {
        if commenting && review.draft.is_none() && !rows.is_empty() {
            let say = "Click a line number to comment on it";
            s.line(say, F::Small, Rect::new(PAD, y, inner, 18.0), cx.pal.faint);
            y += 24.0;
        }
        return y;
    }
    for (k, c) in review.comments.iter().enumerate() {
        let top = y;
        let place = comments::shown_place(rows, c.from, c.to);
        let ink = cx.pal.accent.mix(cx.pal.fg, 0.45);
        s.line(
            place,
            F::Small,
            Rect::new(PAD + 10.0, y, inner - 90.0, 18.0),
            ink,
        );
        if commenting {
            let drop = Rect::new(PAD + inner - 64.0, y, 64.0, 18.0);
            s.line("Remove", F::Small, drop, cx.pal.faint);
            s.hit(drop, Hit::Tr(Act::Unnote(request_id.to_string(), Some(k))));
        }
        y += 20.0;
        if let Some(l) = cx.layout(&cx.fonts.body, &c.text, inner - 10.0) {
            let (_, th) = render::text_size(&l);
            s.text(&l, PAD + 10.0, y, cx.pal.fg.with_alpha(0.9));
            y += th;
        }
        s.fill(Rect::new(PAD, top, 2.0, y - top), 0.0, cx.pal.accent);
        y += 10.0;
    }
    y
}

/// The most of an option's preview its row shows: a preview is there to
/// compare options at a glance, and four long ones would bury the ask.
const PREVIEW_MAX_H: f32 = 9.0 * super::rich::MONO_LINE;

/// A small rounded label at (`x`, `y`), such as a question's header or
/// the badge of the option Claude recommends. Returns its right edge.
fn chip(cx: &Ctx, s: &mut Scene, x: f32, y: f32, text: &str, ink: Color, fill: Color) -> f32 {
    let tw = cx
        .one_line(&cx.gfx.small, text)
        .map_or(text.chars().count() as f32 * 7.0, |l| {
            render::text_size(&l).0
        });
    let r = Rect::new(x, y, tw + 16.0, 20.0);
    s.fill(r, 10.0, fill);
    s.line(text, F::Centre, r, ink);
    r.right()
}

/// A question's header chip, where it stands among several, and the
/// question itself: a title while it waits, a quiet line once answered.
/// Returns the bottom.
#[allow(clippy::too_many_arguments)]
unsafe fn heading(
    cx: &Ctx,
    s: &mut Scene,
    question: &horadric_core::folio::Question,
    q: usize,
    total: usize,
    waiting: bool,
    mut y: f32,
    inner: f32,
) -> f32 {
    let mut x = PAD;
    if !question.header.is_empty() {
        let (ink, fill) = if waiting {
            (
                cx.pal.accent.mix(cx.pal.fg, 0.55),
                cx.pal.accent.with_alpha(0.16),
            )
        } else {
            (cx.pal.dim, cx.pal.fg.with_alpha(0.06))
        };
        x = chip(cx, s, x, y, &question.header, ink, fill) + 8.0;
    }
    if total > 1 {
        let step = format!("{} of {total}", q + 1);
        s.line(step, F::Small, Rect::new(x, y, inner, 20.0), cx.pal.faint);
    }
    if x > PAD || total > 1 {
        y += 28.0;
    }
    let (font, ink) = if waiting {
        (&cx.fonts.h3, cx.pal.fg)
    } else {
        (&cx.fonts.body, cx.pal.dim)
    };
    if let Some(l) = cx.layout(font, &question.question, inner) {
        s.unit(&l, PAD, y, ink, &question.question);
        y += render::text_size(&l).1 + if waiting { 12.0 } else { 6.0 };
    }
    y
}

/// One option as a full-width row: a radio or a box to tick, the label
/// with a badge when Claude recommends it, the description under it, and
/// its preview when it has one. Returns the row.
unsafe fn option(
    cx: &Ctx,
    s: &mut Scene,
    opt: &horadric_core::folio::QuestionOption,
    multi: bool,
    on: bool,
    y: f32,
    inner: f32,
) -> Rect {
    let (label, rec) = look::recommended(&opt.label);
    let badge = "Recommended";
    let badge_w = match rec {
        true => cx
            .one_line(&cx.gfx.small, badge)
            .map_or(90.0, |l| render::text_size(&l).0 + 16.0),
        false => 0.0,
    };
    let x = PAD + 36.0;
    let text_w = inner - 48.0;
    let label_w = if rec { text_w - badge_w - 8.0 } else { text_w };
    let label_l = cx.layout(&cx.gfx.text, label, label_w);
    let desc_l = (!opt.description.is_empty())
        .then(|| cx.layout(&cx.fonts.small_wrap, &opt.description, text_w))
        .flatten();
    let preview_l = (!opt.preview.is_empty())
        .then(|| cx.layout(&cx.fonts.rows, &opt.preview, text_w - 16.0))
        .flatten();
    let lh = label_l.as_ref().map_or(18.0, |l| render::text_size(l).1);
    let dh = desc_l
        .as_ref()
        .map_or(0.0, |l| render::text_size(l).1 + 3.0);
    let ph = preview_l
        .as_ref()
        .map_or(0.0, |l| render::text_size(l).1.min(PREVIEW_MAX_H) + 22.0);
    let r = Rect::new(PAD, y, inner, lh + dh + ph + 20.0);
    if on {
        s.fill(r, 8.0, cx.pal.accent.with_alpha(0.14));
        s.stroke(r, 8.0, cx.pal.accent.with_alpha(0.65), 1.0);
    } else {
        s.fill(r, 8.0, cx.pal.fg.with_alpha(0.025));
        s.stroke(r, 8.0, cx.pal.fg.with_alpha(0.12), 1.0);
    }
    let icon = match (multi, on) {
        (true, true) => "\u{E73A}",
        (true, false) => "\u{E739}",
        (false, true) => "\u{ECCB}",
        (false, false) => "\u{ECCA}",
    };
    let icon_ink = if on {
        cx.pal.accent.mix(cx.pal.fg, 0.3)
    } else {
        cx.pal.faint
    };
    let first = label_l
        .as_ref()
        .and_then(|l| super::rich::first_line(l))
        .unwrap_or(20.0);
    s.line(
        icon,
        F::Icon,
        Rect::new(PAD + 12.0, y + 10.0, 16.0, first),
        icon_ink,
    );
    if let Some(l) = &label_l {
        s.unit(l, x, y + 10.0, cx.pal.fg, label);
    }
    if rec {
        let top = y + 10.0 + (first - 20.0) / 2.0;
        let (ink, fill) = (
            theme::done().mix(cx.pal.fg, 0.35),
            theme::done().with_alpha(0.14),
        );
        chip(cx, s, r.right() - 12.0 - badge_w, top, badge, ink, fill);
    }
    let mut at = y + 10.0 + lh + 3.0;
    if let Some(l) = &desc_l {
        s.unit(l, x, at, cx.pal.dim, &opt.description);
        at += render::text_size(l).1;
    }
    if let Some(l) = &preview_l {
        let shown = render::text_size(l).1.min(PREVIEW_MAX_H);
        let pr = Rect::new(x, at + 8.0, text_w, shown + 12.0);
        s.fill(pr, 6.0, Color::rgb(0).with_alpha(0.25));
        s.items.push(super::rich::Item::Clip(pr));
        s.unit(
            l,
            x + 8.0,
            pr.y + 6.0,
            cx.pal.fg.with_alpha(0.85),
            &opt.preview,
        );
        s.items.push(super::rich::Item::Unclip);
    }
    r
}

/// The mark before a settled ask's outcome: a tick for yes, a cross for
/// no, a dash for one that lapsed or was answered elsewhere.
fn settled_mark(cx: &Ctx, state: AskState) -> (&'static str, Color) {
    match state {
        AskState::Allowed => ("\u{E73E}", theme::done()),
        AskState::Denied => ("\u{E711}", cx.pal.faint),
        _ => ("\u{E738}", cx.pal.faint),
    }
}

/// How an ask ended, with a mark before it: what was chosen, or that it
/// was denied or lapsed. Returns the bottom.
#[allow(clippy::too_many_arguments)]
unsafe fn outcome(
    cx: &Ctx,
    s: &mut Scene,
    icon: &str,
    icon_ink: Color,
    said: &str,
    ink: Color,
    y: f32,
    inner: f32,
) -> f32 {
    let Some(l) = cx.layout(&cx.fonts.body, said, inner - 22.0) else {
        return y;
    };
    let first = super::rich::first_line(&l).unwrap_or(20.0);
    s.line(icon, F::Icon, Rect::new(PAD, y, 16.0, first), icon_ink);
    s.unit(&l, PAD + 22.0, y, ink, said);
    y + render::text_size(&l).1 + 4.0
}

/// What a question card counts as answered: per question, the labels
/// picked, or the human's own words when typed.
pub(super) fn picks_of(b: &Block, st: &State) -> Vec<Vec<String>> {
    let Block::Question {
        request_id,
        questions,
        ..
    } = b
    else {
        return Vec::new();
    };
    let picked = st.picks.get(request_id);
    (questions.iter().enumerate())
        .map(|(q, question)| {
            let mut out: Vec<String> = (picked.and_then(|p| p.get(q)).into_iter().flatten())
                .filter_map(|&o| question.options.get(o).map(|o| o.label.clone()))
                .collect();
            let other = st.text(&other_field(request_id, q)).trim();
            if !other.is_empty() {
                if !question.multi_select {
                    out.clear();
                }
                out.push(other.to_string());
            }
            out
        })
        .collect()
}

/// A question card: each question with its options as buttons (boxes to
/// tick when several may be picked), a field for an answer of one's own,
/// and Submit. A single question with one pick answers on the click.
/// Once settled each question shrinks to the line that answered it.
pub(super) unsafe fn question(cx: &Ctx, b: &Block, st: &State, w: f32) -> Scene {
    let mut s = Scene::default();
    let Block::Question {
        request_id,
        questions,
        answers,
        state,
        ..
    } = b
    else {
        return s;
    };
    let inner = w - 2.0 * PAD;
    let waiting = *state == AskState::Waiting;
    let total = questions.len();
    let picked = st.picks.get(request_id);
    let mut y = PAD;
    for (q, question) in questions.iter().enumerate() {
        if q > 0 {
            // Several questions read as steps of one ask, each its own
            // section, rather than one long list of options.
            let gap = if waiting { 18.0 } else { 10.0 };
            y += gap - 8.0;
            s.fill(
                Rect::new(PAD, y, inner, 1.0),
                0.0,
                cx.pal.fg.with_alpha(0.08),
            );
            y += gap;
        }
        y = heading(cx, &mut s, question, q, total, waiting, y, inner);
        if !waiting {
            let said = answers.get(q).map_or("", String::as_str);
            // A lapsed ask says so once below, not "No answer" per question.
            if said.is_empty() && *state != AskState::Allowed {
                continue;
            }
            let (icon, ink, said) = if said.is_empty() {
                ("\u{E738}", cx.pal.faint, "No answer".to_string())
            } else {
                ("\u{E73E}", theme::done(), look::answer_shown(said))
            };
            y = outcome(cx, &mut s, icon, ink, &said, cx.pal.fg, y, inner) + 4.0;
            continue;
        }
        let picks = picked.and_then(|p| p.get(q));
        for (o, opt) in question.options.iter().enumerate() {
            let on = picks.is_some_and(|p| p.contains(&o));
            let r = option(cx, &mut s, opt, question.multi_select, on, y, inner);
            s.hit(r, Hit::Tr(Act::Pick(request_id.clone(), q, o)));
            y = r.bottom() + 6.0;
        }
        let id = other_field(request_id, q);
        let hint = "Something else? Type your own answer";
        y = field(cx, &mut s, st, &id, hint, PAD, y, inner) + 14.0;
    }
    if waiting {
        let picks = picks_of(b, st);
        let ready = picks.iter().all(|p| !p.is_empty());
        let answered = picks.iter().filter(|p| !p.is_empty()).count();
        let multi = questions.first().is_some_and(|q| q.multi_select);
        let say = look::question_progress(answered, total, multi);
        let r = Rect::new(w - PAD - 96.0, y, 96.0, BUTTON_H);
        s.line(
            say,
            F::Small,
            Rect::new(PAD, y, inner - r.w - 12.0, BUTTON_H),
            cx.pal.faint,
        );
        if ready {
            s.fill(r, 8.0, cx.pal.fg.with_alpha(0.92));
            s.line("Submit", F::Button, r, cx.pal.bg);
            s.hit(r, Hit::Tr(Act::Submit(request_id.clone())));
        } else {
            s.stroke(r, 8.0, cx.pal.fg.with_alpha(0.15), 1.0);
            s.line("Submit", F::Button, r, cx.pal.faint);
        }
        y += BUTTON_H;
    } else if *state != AskState::Allowed {
        let (icon, ink) = settled_mark(cx, *state);
        // A question is not denied, it goes unanswered.
        let said = match state {
            AskState::Denied => "Declined",
            _ => look::ask_said(*state),
        };
        y = outcome(cx, &mut s, icon, ink, said, cx.pal.dim, y, inner);
    }
    let h = y + PAD;
    plate(cx, &mut s, w, h, waiting);
    s.h = h;
    s
}

/// A plan as a page: rendered, then approve, approve with edits accepted,
/// or keep planning with what to change.
pub(super) unsafe fn plan(cx: &Ctx, b: &Block, st: &State, w: f32) -> Scene {
    let mut s = Scene::default();
    let Block::Plan {
        request_id,
        plan,
        state,
        feedback,
        ..
    } = b
    else {
        return s;
    };
    let inner = w - 2.0 * PAD;
    s.line(
        "\u{E8A5}",
        F::Icon,
        Rect::new(PAD, PAD, 16.0, 20.0),
        cx.pal.accent.mix(cx.pal.fg, 0.4),
    );
    s.line(
        "The plan",
        F::Title,
        Rect::new(PAD + 24.0, PAD, inner, 20.0),
        cx.pal.fg,
    );
    let mut y = PAD + 32.0;
    for p in flow::paras(plan) {
        let ps = cx.para(&p, inner);
        y += p.gap;
        s.append(&ps, PAD, y);
        y += ps.h;
    }
    y += 16.0;
    let right = w - PAD;
    match state {
        AskState::Waiting if st.keeping.contains(request_id) => {
            let id = keep_field(request_id);
            y = field(cx, &mut s, st, &id, "What should change?", PAD, y, inner) + 10.0;
            let x = button(
                cx,
                &mut s,
                right,
                y,
                "Send back",
                true,
                Hit::Tr(Act::Keep(request_id.clone())),
            );
            button(
                cx,
                &mut s,
                x - 8.0,
                y,
                "Cancel",
                false,
                Hit::Tr(Act::Cancel(request_id.clone())),
            );
            y += BUTTON_H;
        }
        AskState::Waiting => {
            let x = button(
                cx,
                &mut s,
                right,
                y,
                "Approve",
                true,
                Hit::Tr(Act::Approve(request_id.clone(), None)),
            );
            let x = button(
                cx,
                &mut s,
                x - 8.0,
                y,
                "Approve, accept edits",
                false,
                Hit::Tr(Act::Approve(request_id.clone(), Some("acceptEdits"))),
            );
            button(
                cx,
                &mut s,
                x - 8.0,
                y,
                "Keep planning",
                false,
                Hit::Tr(Act::Keep(request_id.clone())),
            );
            y += BUTTON_H;
        }
        settled => {
            let said = match settled {
                AskState::Allowed => "Approved".to_string(),
                AskState::Denied if feedback.trim().is_empty() => "Sent back".to_string(),
                AskState::Denied => format!("Sent back: {}", feedback.trim()),
                _ => look::ask_said(*settled).to_string(),
            };
            let (icon, ink) = settled_mark(cx, *settled);
            y = outcome(cx, &mut s, icon, ink, &said, cx.pal.dim, y, inner);
        }
    }
    let h = y + PAD;
    plate(cx, &mut s, w, h, *state == AskState::Waiting);
    s.h = h;
    s
}

/// The sign-in card's code field, by its block.
pub(super) fn code_field(block: usize) -> String {
    format!("signin:{block}")
}

/// Logging in to Claude: why it is needed, then the sign-in page and a
/// field for the code it shows, then how it went.
pub(super) unsafe fn sign_in(cx: &Ctx, i: usize, b: &Block, st: &State, w: f32) -> Scene {
    let mut s = Scene::default();
    let Block::SignIn(card) = b else {
        return s;
    };
    let inner = w - 2.0 * PAD;
    let right = w - PAD;
    let title = match &card.state {
        SignInState::Needed => "Claude is not logged in",
        SignInState::Done(_) => "Logged in",
        _ => "Log in to Claude",
    };
    s.line(title, F::Title, Rect::new(PAD, PAD, inner, 20.0), cx.pal.fg);
    let mut y = PAD + 26.0;
    let say = |s: &mut Scene, y: &mut f32, text: &str, ink: Color| {
        if let Some(l) = cx.layout(&cx.fonts.body, text, inner) {
            s.unit(&l, PAD, *y, ink, text);
            *y += render::text_size(&l).1 + 8.0;
        }
    };
    if !card.why.is_empty() && card.state == SignInState::Needed {
        say(&mut s, &mut y, &card.why, cx.pal.dim);
    }
    let text = match &card.state {
        SignInState::Needed => String::new(),
        SignInState::Starting => "Starting the sign-in\u{2026}".to_string(),
        SignInState::Waiting if card.bad_code => {
            "That code was not whole. Copy all of it from the page and paste it again.".to_string()
        }
        SignInState::Waiting => {
            "The sign-in page opened in your browser. Finish there and this goes on by itself."
                .to_string()
        }
        SignInState::Checking => "Checking the code\u{2026}".to_string(),
        SignInState::Done(Some(email)) => {
            format!("Logged in as {email}. The session starts again with it.")
        }
        SignInState::Done(None) => "The session starts again with the new login.".to_string(),
        SignInState::Failed(why) => format!("The sign-in failed: {why}"),
        SignInState::Cancelled => "The sign-in was closed.".to_string(),
    };
    if !text.is_empty() {
        let ink = match card.state {
            SignInState::Failed(_) => theme::waiting(),
            _ => cx.pal.fg.with_alpha(0.85),
        };
        say(&mut s, &mut y, &text, ink);
    }
    let waiting = matches!(card.state, SignInState::Waiting | SignInState::Checking);
    if waiting && card.asks_code {
        let id = code_field(i);
        let hint = "Or paste the code the page shows, then Enter";
        y = field(cx, &mut s, st, &id, hint, PAD, y, inner) + 10.0;
    }
    match &card.state {
        SignInState::Needed | SignInState::Failed(_) | SignInState::Cancelled => {
            let label = match card.state {
                SignInState::Needed => "Log in",
                _ => "Try again",
            };
            button(cx, &mut s, right, y, label, true, Hit::Tr(Act::SignIn(i)));
            y += BUTTON_H;
        }
        SignInState::Starting | SignInState::Waiting | SignInState::Checking => {
            let mut x = right;
            if card.url.is_some() {
                x = button(
                    cx,
                    &mut s,
                    x,
                    y,
                    "Open sign-in page",
                    true,
                    Hit::Tr(Act::SignInPage(i)),
                ) - 8.0;
            }
            if waiting && card.asks_code && !st.text(&code_field(i)).trim().is_empty() {
                x = button(
                    cx,
                    &mut s,
                    x,
                    y,
                    "Send code",
                    false,
                    Hit::Tr(Act::SignInCode(i)),
                ) - 8.0;
            }
            button(
                cx,
                &mut s,
                x,
                y,
                "Cancel",
                false,
                Hit::Tr(Act::SignInCancel(i)),
            );
            y += BUTTON_H;
        }
        SignInState::Done(_) => {}
    }
    let h = y + PAD;
    let asking = matches!(card.state, SignInState::Needed | SignInState::Waiting);
    plate(cx, &mut s, w, h, asking);
    s.h = h;
    s
}
