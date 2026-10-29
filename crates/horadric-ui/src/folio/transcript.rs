//! The conversation's blocks: how tall each stands, and drawing them.

use horadric_core::folio::{AskState, Block, Ending, StepState};
use windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE;
use windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE;
use windows::Win32::Graphics::DirectWrite::{
    IDWriteTextFormat, IDWriteTextLayout, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER,
};
use windows_numerics::Vector2;

use super::view::{d2d, text_format, Gfx, Hit, Laid, Palette, View, FONT};
use super::Chat;
use crate::folio_look::{self as look, RunState};
use crate::layout::Rect;
use crate::render::{self, color};
use crate::theme::{self, Color};

const TOP: f32 = 22.0;
const GAP: f32 = 14.0;
const LINE_H: f32 = 22.0;
const STEP_H: f32 = 20.0;
const FOOTER_H: f32 = 18.0;
const BUBBLE_X: f32 = 14.0;
const BUBBLE_Y: f32 = 9.0;
const BUBBLE_SHARE: f32 = 0.82;
const CARD_PAD: f32 = 14.0;
const BUTTON_W: f32 = 84.0;
const BUTTON_H: f32 = 30.0;
/// The most of an ask's input a card shows before it is cut off.
const ASK_MAX_H: f32 = 140.0;
/// What a raw line shows of itself.
const RAW_MAX: usize = 400;
impl View {
    /// The block's text laid out at `width`, from the cache when it has not
    /// changed.
    pub(super) fn laid(
        &self,
        gfx: &Gfx,
        i: usize,
        tag: u8,
        text: &str,
        format: &IDWriteTextFormat,
        width: f32,
    ) -> Option<(IDWriteTextLayout, f32, f32)> {
        let mut laid = self.laid.borrow_mut();
        if laid.len() <= i {
            laid.resize_with(i + 1, || None);
        }
        if let Some(l) = &laid[i] {
            if l.tag == tag && l.len == text.len() && l.width == width {
                return Some((l.layout.clone(), l.w, l.h));
            }
        }
        let layout = render::wrapped(&self.shared.gpu, format, text, width.max(1.0)).ok()?;
        let (w, h) = render::text_size(&layout);
        laid[i] = Some(Laid {
            tag,
            len: text.len(),
            width,
            layout: layout.clone(),
            w,
            h,
        });
        let _ = gfx;
        Some((layout, w, h))
    }

    /// How tall block `i` stands at column width `cw`.
    pub(super) fn height(&self, gfx: &Gfx, i: usize, b: &Block, open: bool, cw: f32) -> f32 {
        let laid = |tag, text: &str, f: &IDWriteTextFormat, w| {
            self.laid(gfx, i, tag, text, f, w).map_or(0.0, |l| l.2)
        };
        match b {
            Block::Prompt(t) => {
                laid(0, t, &gfx.text, cw * BUBBLE_SHARE - 2.0 * BUBBLE_X) + 2.0 * BUBBLE_Y
            }
            Block::Reply(t) if t.trim().is_empty() => 0.0,
            Block::Reply(t) => laid(1, t, &gfx.text, cw),
            Block::Thinking(t) if open => LINE_H + 4.0 + laid(2, t, &gfx.italic, cw - 22.0),
            Block::Thinking(_) => LINE_H,
            Block::Tools(steps) if open => LINE_H + steps.len() as f32 * STEP_H,
            Block::Tools(_) => LINE_H,
            Block::Ask { shown, .. } => {
                let shown = laid(3, shown, &gfx.mono, cw - 2.0 * CARD_PAD).min(ASK_MAX_H);
                CARD_PAD + 20.0 + 6.0 + shown + 12.0 + BUTTON_H + CARD_PAD
            }
            Block::Footer { .. } => FOOTER_H,
            Block::Raw(t) => laid(4, &raw_text(t), &gfx.italic, cw),
            _ => 0.0,
        }
    }

    /// The conversation in `area`, above the composer: measured, scrolled
    /// to follow the end or kept in bounds, then drawn block by block.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn transcript(
        &self,
        gfx: &Gfx,
        pal: &Palette,
        chat: &mut Chat,
        cx: f32,
        cw: f32,
        area: &Rect,
        t: f32,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let rt = &gfx.rt;

        // Measure, then follow the end or keep the scroll in bounds.
        let heights: Vec<f32> = (chat.folio.blocks.iter().enumerate())
            .map(|(i, b)| self.height(gfx, i, b, chat.open.contains(&i), cw))
            .collect();
        let tail = if chat.folio.busy { 28.0 } else { 0.0 }
            + if chat.ended || chat.error.is_some() {
                LINE_H + GAP
            } else {
                0.0
            };
        let content = TOP
            + (chat.folio.blocks.iter().zip(&heights))
                .map(|(b, &bh)| if bh > 0.0 { bh + gap_after(b) } else { 0.0 })
                .sum::<f32>()
            + tail
            + 8.0;
        chat.content_h = content;
        chat.view_h = area.h;
        chat.scroll = if chat.follow {
            (content - area.h).max(0.0)
        } else {
            look::clamp_scroll(chat.scroll, content, area.h)
        };

        rt.PushAxisAlignedClip(&d2d(area), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        if chat.folio.blocks.is_empty() && chat.error.is_none() && !chat.ended {
            self.empty(gfx, pal, chat, area);
        }
        let mut y = TOP - chat.scroll;
        for (i, (b, &bh)) in chat.folio.blocks.iter().zip(&heights).enumerate() {
            if bh <= 0.0 {
                continue;
            }
            if y + bh >= 0.0 && y <= area.bottom() {
                let open = chat.open.contains(&i);
                let mut mine = Vec::new();
                self.block(gfx, pal, i, b, open, cx, cw, y, bh, &mut mine);
                // Drawing is clipped to the area; what is hidden must not
                // take clicks either.
                hits.extend(
                    (mine.into_iter()).filter_map(|(r, hit)| Some((look::clip(&r, area)?, hit))),
                );
            }
            y += bh + gap_after(b);
        }
        if chat.folio.busy {
            for k in 0..3 {
                let pulse = ((t * 4.0 - k as f32 * 0.7).sin() * 0.5 + 0.5).powf(1.5);
                let x = cx + 6.0 + k as f32 * 12.0;
                gfx.brush.SetColor(&color(
                    pal.accent.mix(pal.fg, 0.3).with_alpha(0.25 + 0.75 * pulse),
                ));
                rt.FillEllipse(
                    &D2D1_ELLIPSE {
                        point: Vector2 { X: x, Y: y + 10.0 },
                        radiusX: 3.2,
                        radiusY: 3.2,
                    },
                    &gfx.brush,
                );
            }
            y += 28.0;
        }
        if let Some(e) = &chat.error {
            gfx.text_in(e, &gfx.line, &Rect::new(cx, y, cw, LINE_H), theme::error());
        } else if chat.ended {
            let r = Rect::new(cx, y, cw, LINE_H);
            gfx.text_in(
                "Claude ended. Close the Folio to start again.",
                &gfx.line,
                &r,
                pal.dim,
            );
        }
        rt.PopAxisAlignedClip();
    }

    /// What an empty Folio says, in the middle of the glass.
    pub(super) unsafe fn empty(&self, gfx: &Gfx, pal: &Palette, chat: &Chat, area: &Rect) {
        let mid = area.h / 2.0;
        let x = area.w / 2.0;
        gfx.rune(3, x, mid - 70.0, 34.0, pal.accent.with_alpha(0.55), 2.0);
        let name = chat
            .dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let title = format!("Ask Claude about {name}");
        let r = Rect::new(area.x + 20.0, mid - 22.0, area.w - 40.0, 30.0);
        gfx.text_in(&title, &gfx.big, &r, pal.fg.with_alpha(0.85));
        let r = Rect::new(area.x + 20.0, mid + 10.0, area.w - 40.0, 20.0);
        let note = "Experimental. A headless Claude Code session, drawn by Horadric.";
        let mut centred = gfx.button.clone();
        if let Ok(f) = text_format(
            &self.shared.gpu,
            FONT,
            12.5,
            DWRITE_FONT_WEIGHT_NORMAL,
            false,
            true,
        ) {
            let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            centred = f;
        }
        gfx.text_in(note, &centred, &r, pal.faint);
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn block(
        &self,
        gfx: &Gfx,
        pal: &Palette,
        i: usize,
        b: &Block,
        open: bool,
        cx: f32,
        cw: f32,
        y: f32,
        bh: f32,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        match b {
            Block::Prompt(t) => {
                let max = cw * BUBBLE_SHARE - 2.0 * BUBBLE_X;
                let Some((l, tw, _)) = self.laid(gfx, i, 0, t, &gfx.text, max) else {
                    return;
                };
                let bw = tw.min(max) + 2.0 * BUBBLE_X;
                let r = Rect::new(cx + cw - bw, y, bw, bh);
                gfx.fill_round(&r, 14.0, pal.bg.mix(pal.accent, 0.2).mix(pal.fg, 0.04));
                gfx.stroke_round(&r, 14.0, pal.accent.with_alpha(0.28), 1.0);
                gfx.layout_at(&l, r.x + BUBBLE_X, y + BUBBLE_Y, pal.fg);
            }
            Block::Reply(t) => {
                if let Some((l, _, _)) = self.laid(gfx, i, 1, t, &gfx.text, cw) {
                    gfx.layout_at(&l, cx, y, pal.fg);
                }
            }
            Block::Thinking(t) => {
                let r = Rect::new(cx, y, cw, LINE_H);
                self.fold_line(
                    gfx,
                    pal,
                    ('\u{E82F}', pal.faint),
                    &look::thinking_line(t),
                    open,
                    &r,
                    pal.faint,
                );
                hits.push((r, Hit::Fold(i)));
                if open {
                    if let Some((l, _, _)) = self.laid(gfx, i, 2, t, &gfx.italic, cw - 22.0) {
                        gfx.fill(
                            &Rect::new(cx + 7.0, y + LINE_H + 2.0, 1.5, bh - LINE_H - 2.0),
                            pal.faint.with_alpha(0.25),
                        );
                        gfx.layout_at(&l, cx + 22.0, y + LINE_H + 4.0, pal.dim);
                    }
                }
            }
            Block::Tools(steps) => {
                let r = Rect::new(cx, y, cw, LINE_H);
                let state = look::run_state(steps);
                let mut text = look::run_summary(steps);
                // A failure marks the run's icon. The line stays quiet,
                // since a failed step is often Claude finding its way.
                let mark = match state {
                    RunState::Running => {
                        text.push_str(" \u{2026}");
                        pal.dim
                    }
                    RunState::Stopped => {
                        text.push_str(" \u{b7} stopped");
                        pal.faint
                    }
                    RunState::Done(0) => pal.dim,
                    RunState::Done(n) => {
                        text.push_str(&format!(" \u{b7} {n} failed"));
                        theme::error().with_alpha(0.9)
                    }
                };
                let icon = ('\u{E90F}', mark);
                self.fold_line(gfx, pal, icon, &text, open, &r, pal.dim);
                hits.push((r, Hit::Fold(i)));
                if open {
                    for (k, s) in steps.iter().enumerate() {
                        let sy = y + LINE_H + k as f32 * STEP_H;
                        let mark = match s.state {
                            StepState::Running => pal.faint,
                            StepState::Stopped => pal.faint.with_alpha(0.2),
                            StepState::Done => theme::done().with_alpha(0.8),
                            StepState::Failed => theme::error(),
                        };
                        gfx.fill_round(
                            &Rect::new(cx + 6.0, sy + STEP_H / 2.0 - 2.5, 5.0, 5.0),
                            2.5,
                            mark,
                        );
                        let line = if s.summary.is_empty() {
                            s.tool.clone()
                        } else {
                            format!("{}  {}", s.tool, s.summary)
                        };
                        gfx.text_in(
                            &line,
                            &gfx.small,
                            &Rect::new(cx + 22.0, sy, cw - 22.0, STEP_H),
                            pal.dim,
                        );
                    }
                }
            }
            Block::Ask {
                request_id,
                tool,
                shown,
                state,
                ..
            } => {
                let card = Rect::new(cx, y, cw, bh);
                gfx.fill_round(&card, 12.0, pal.bg.mix(pal.fg, 0.05));
                let edge = match state {
                    AskState::Waiting => theme::waiting().with_alpha(0.75),
                    _ => pal.faint.with_alpha(0.3),
                };
                gfx.stroke_round(&card, 12.0, edge, 1.2);
                let inner = cw - 2.0 * CARD_PAD;
                let title = Rect::new(cx + CARD_PAD, y + CARD_PAD, inner, 20.0);
                gfx.text_in(&look::ask_title(tool), &gfx.title, &title, pal.fg);
                let top = title.bottom() + 6.0;
                if let Some((l, _, lh)) = self.laid(gfx, i, 3, shown, &gfx.mono, inner) {
                    let shown = Rect::new(
                        cx + CARD_PAD - 2.0,
                        top - 2.0,
                        inner + 4.0,
                        lh.min(ASK_MAX_H) + 4.0,
                    );
                    gfx.fill_round(&shown, 6.0, Color::rgb(0).with_alpha(0.25));
                    gfx.rt
                        .PushAxisAlignedClip(&d2d(&shown), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                    gfx.layout_at(&l, cx + CARD_PAD, top, pal.fg.with_alpha(0.85));
                    gfx.rt.PopAxisAlignedClip();
                }
                let by = card.bottom() - CARD_PAD - BUTTON_H;
                let right = card.right() - CARD_PAD;
                match state {
                    AskState::Waiting => {
                        let allow = Rect::new(right - BUTTON_W, by, BUTTON_W, BUTTON_H);
                        let deny = Rect::new(allow.x - 8.0 - BUTTON_W, by, BUTTON_W, BUTTON_H);
                        gfx.fill_round(&allow, 8.0, pal.fg.with_alpha(0.92));
                        gfx.text_in("Allow", &gfx.button, &allow, pal.bg);
                        gfx.stroke_round(&deny, 8.0, pal.fg.with_alpha(0.35), 1.0);
                        gfx.text_in("Deny", &gfx.button, &deny, pal.fg);
                        hits.push((allow, Hit::Answer(request_id.clone(), true)));
                        hits.push((deny, Hit::Answer(request_id.clone(), false)));
                    }
                    _ => {
                        let said = look::ask_said(*state);
                        let r = Rect::new(cx + CARD_PAD, by, inner, BUTTON_H);
                        gfx.text_in(said, &gfx.line, &r, pal.dim);
                    }
                }
            }
            Block::Footer {
                duration_ms,
                cost_usd,
                ending,
                ..
            } => {
                let ink = match ending {
                    Ending::Failed => theme::error().with_alpha(0.8),
                    Ending::Done | Ending::Stopped => pal.faint,
                };
                let line = look::footer_line(*duration_ms, *cost_usd, *ending);
                gfx.text_in(&line, &gfx.small, &Rect::new(cx, y, cw, FOOTER_H), ink);
                // A short rule under the turn, in the project's colour.
                gfx.fill(
                    &Rect::new(cx, y + FOOTER_H + 8.0, 28.0, 1.0),
                    pal.accent.with_alpha(0.35),
                );
            }
            Block::Raw(t) => {
                if let Some((l, _, _)) = self.laid(gfx, i, 4, &raw_text(t), &gfx.italic, cw) {
                    gfx.layout_at(&l, cx, y, pal.faint);
                }
            }
            _ => {}
        }
    }

    /// A folded block's one line: its icon, its text, and a chevron saying
    /// a click opens it.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn fold_line(
        &self,
        gfx: &Gfx,
        pal: &Palette,
        (icon, mark): (char, Color),
        text: &str,
        open: bool,
        r: &Rect,
        ink: Color,
    ) {
        let i = Rect::new(r.x, r.y, 16.0, r.h);
        gfx.text_in(&icon.to_string(), &gfx.icon, &i, mark);
        let tr = Rect::new(r.x + 22.0, r.y, r.w - 44.0, r.h);
        gfx.text_in(text, &gfx.line, &tr, ink);
        let chevron = if open { '\u{E70D}' } else { '\u{E76C}' };
        let c = Rect::new(r.right() - 16.0, r.y, 16.0, r.h);
        gfx.text_in(
            &chevron.to_string(),
            &gfx.icon,
            &c,
            pal.faint.with_alpha(0.3),
        );
    }
}

/// The room left under a block before the next.
fn gap_after(b: &Block) -> f32 {
    match b {
        Block::Footer { .. } => GAP + 16.0,
        Block::Thinking(_) | Block::Tools(_) => GAP / 2.0,
        _ => GAP,
    }
}

fn raw_text(t: &str) -> String {
    if t.chars().count() <= RAW_MAX {
        return t.to_string();
    }
    let mut s: String = t.chars().take(RAW_MAX).collect();
    s.push('\u{2026}');
    s
}
