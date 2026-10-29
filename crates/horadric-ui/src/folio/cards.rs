//! The cards that wait on the human: a permission ask (allow once,
//! always, or deny with what to do instead), a question with its options,
//! and a plan to approve or send back. Each is built into a [`Scene`]
//! like any block; what is typed into their fields lives in [`State`].

use horadric_core::folio::{AskState, Block};

use super::flow;
use super::rich::{Ctx, Scene, F};
use super::transcript::{deny_field, keep_field, other_field, Act, State};
use super::view::Hit;
use crate::field::utf16_at;
use crate::folio_look as look;
use crate::layout::Rect;
use crate::render;
use crate::theme::{self, Color};

const PAD: f32 = 14.0;
const BUTTON_H: f32 = 30.0;
/// The most of an ask's input a card shows before it is cut off.
const ASK_MAX_H: f32 = 140.0;

/// A button `w` wide ending at `right`, filled when it is the one to
/// press. Returns its left edge.
fn button(
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
fn field(cx: &Ctx, s: &mut Scene, st: &State, id: &str, hint: &str, x: f32, y: f32, w: f32) -> f32 {
    let text = st.text(id);
    let focused = st.focus.as_deref() == Some(id);
    let inner = w - 20.0;
    let shown = if text.is_empty() { hint } else { text };
    let Some(l) = cx.layout(&cx.fonts.body, shown, inner) else {
        return y;
    };
    let (_, th) = render::text_size(&l);
    let r = Rect::new(x, y, w, th.max(20.0) + 14.0);
    s.fill(r, 8.0, Color::rgb(0).with_alpha(0.25));
    let edge = if focused {
        cx.pal.accent.with_alpha(0.75)
    } else {
        cx.pal.fg.with_alpha(0.18)
    };
    s.stroke(r, 8.0, edge, 1.0);
    let ink = if text.is_empty() {
        cx.pal.faint
    } else {
        cx.pal.fg
    };
    s.text(&l, x + 10.0, y + 7.0, ink);
    if focused {
        let f = st.fields.get(id);
        let at = f.map_or(0, |f| utf16_at(&f.text, f.caret));
        let c = if text.is_empty() {
            Rect::new(0.0, 0.0, 1.0, th.max(18.0))
        } else {
            render::caret_at(&l, at)
        };
        s.fill(
            Rect::new(x + 10.0 + c.x, y + 7.0 + c.y, 1.3, c.h),
            0.0,
            cx.pal.fg,
        );
    }
    s.hit(r, Hit::Tr(Act::Field(id.to_string())));
    r.bottom()
}

fn plate(cx: &Ctx, s: &mut Scene, w: f32, h: f32, waiting: bool) {
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
    if let Some(l) = cx.layout(&cx.gfx.mono, shown, inner) {
        let (_, lh) = render::text_size(&l);
        let r = Rect::new(PAD - 2.0, y - 2.0, inner + 4.0, lh.min(ASK_MAX_H) + 6.0);
        s.fill(r, 6.0, Color::rgb(0).with_alpha(0.25));
        s.items.push(super::rich::Item::Clip(r));
        s.unit(&l, PAD, y + 1.0, cx.pal.fg.with_alpha(0.85), shown);
        s.items.push(super::rich::Item::Unclip);
        y = r.bottom() + 12.0;
    }
    let right = w - PAD;
    let always = flow::always_label(suggestions);
    match state {
        AskState::Waiting if st.denying.contains(request_id) => {
            let id = deny_field(request_id);
            let hint = "What should Claude do instead? (optional)";
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
        AskState::Waiting => {
            if let Some(label) = &always {
                let say = format!("Always allow: {label}");
                s.line(say, F::Small, Rect::new(PAD, y, inner, 18.0), cx.pal.faint);
                y += 24.0;
            }
            let mut x = button(
                cx,
                &mut s,
                right,
                y,
                "Allow",
                true,
                Hit::Answer(request_id.clone(), true),
            );
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
            let said = look::ask_said(*state);
            s.line(said, F::Line, Rect::new(PAD, y, inner, 22.0), cx.pal.dim);
            y += 22.0;
        }
    }
    let h = y + PAD;
    plate(cx, &mut s, w, h, *state == AskState::Waiting);
    s.h = h;
    s
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
    let title = if questions.len() == 1 {
        "Claude asks"
    } else {
        "Claude asks a few things"
    };
    s.line(title, F::Title, Rect::new(PAD, PAD, inner, 20.0), cx.pal.fg);
    let mut y = PAD + 28.0;
    let picked = st.picks.get(request_id);
    for (q, question) in questions.iter().enumerate() {
        if !question.header.is_empty() {
            s.line(
                question.header.to_uppercase(),
                F::Small,
                Rect::new(PAD, y, inner, 16.0),
                cx.pal.accent.mix(cx.pal.fg, 0.5),
            );
            y += 18.0;
        }
        if let Some(l) = cx.layout(&cx.fonts.body, &question.question, inner) {
            s.unit(&l, PAD, y, cx.pal.fg, &question.question);
            y += render::text_size(&l).1 + 8.0;
        }
        if !waiting {
            let said = answers.get(q).map_or("", String::as_str);
            let said = if said.is_empty() { "No answer" } else { said };
            if let Some(l) = cx.layout(&cx.fonts.body, said, inner - 22.0) {
                s.line(
                    "\u{E73E}",
                    F::Icon,
                    Rect::new(PAD, y, 16.0, 20.0),
                    theme::done(),
                );
                s.unit(&l, PAD + 22.0, y, cx.pal.dim, said);
                y += render::text_size(&l).1 + 12.0;
            }
            continue;
        }
        for (o, opt) in question.options.iter().enumerate() {
            let on = picked
                .and_then(|p| p.get(q))
                .is_some_and(|p| p.contains(&o));
            let text_w = inner - 44.0;
            let label_l = cx.layout(&cx.gfx.text, &opt.label, text_w);
            let desc_l = (!opt.description.is_empty())
                .then(|| cx.layout(&cx.fonts.small_wrap, &opt.description, text_w))
                .flatten();
            let lh = label_l.as_ref().map_or(18.0, |l| render::text_size(l).1);
            let dh = desc_l
                .as_ref()
                .map_or(0.0, |l| render::text_size(l).1 + 2.0);
            let r = Rect::new(PAD, y, inner, lh + dh + 16.0);
            if on {
                s.fill(r, 8.0, cx.pal.accent.with_alpha(0.18));
                s.stroke(r, 8.0, cx.pal.accent.with_alpha(0.7), 1.0);
            } else {
                s.stroke(r, 8.0, cx.pal.fg.with_alpha(0.16), 1.0);
            }
            let icon = match (question.multi_select, on) {
                (true, true) => "\u{E73A}",
                (true, false) => "\u{E739}",
                (false, true) => "\u{ECCB}",
                (false, false) => "\u{ECCA}",
            };
            s.line(
                icon,
                F::Icon,
                Rect::new(r.x + 10.0, y + 8.0, 16.0, lh),
                cx.pal.dim,
            );
            if let Some(l) = &label_l {
                s.unit(l, r.x + 34.0, y + 8.0, cx.pal.fg, &opt.label);
            }
            if let Some(l) = &desc_l {
                s.unit(
                    l,
                    r.x + 34.0,
                    y + 8.0 + lh + 2.0,
                    cx.pal.dim,
                    &opt.description,
                );
            }
            s.hit(r, Hit::Tr(Act::Pick(request_id.clone(), q, o)));
            y = r.bottom() + 6.0;
        }
        let id = other_field(request_id, q);
        y = field(cx, &mut s, st, &id, "Or say something else", PAD, y, inner) + 14.0;
    }
    if waiting {
        let ready = picks_of(b, st).iter().all(|p| !p.is_empty());
        let r = Rect::new(w - PAD - 96.0, y, 96.0, BUTTON_H);
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
        s.line(
            look::ask_said(*state),
            F::Line,
            Rect::new(PAD, y, inner, 22.0),
            cx.pal.dim,
        );
        y += 22.0;
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
        "Claude's plan",
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
        AskState::Allowed => {
            s.line(
                "Approved",
                F::Line,
                Rect::new(PAD, y, inner, 22.0),
                cx.pal.dim,
            );
            y += 22.0;
        }
        AskState::Denied => {
            let said = if feedback.trim().is_empty() {
                "Sent back".to_string()
            } else {
                format!("Sent back: {}", feedback.trim())
            };
            if let Some(l) = cx.layout(&cx.fonts.body, &said, inner) {
                s.unit(&l, PAD, y, cx.pal.dim, &said);
                y += render::text_size(&l).1;
            }
        }
        AskState::NotNeeded => {
            s.line(
                look::ask_said(*state),
                F::Line,
                Rect::new(PAD, y, inner, 22.0),
                cx.pal.dim,
            );
            y += 22.0;
        }
    }
    let h = y + PAD;
    plate(cx, &mut s, w, h, *state == AskState::Waiting);
    s.h = h;
    s
}
