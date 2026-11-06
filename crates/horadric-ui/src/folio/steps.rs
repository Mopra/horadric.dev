//! A run of tool steps in the transcript: each step's line, and opened, its
//! command, output and images.


use horadric_core::folio::{Step, StepKind, StepState};

use super::flow::self;
use super::rich::{self, Ctx, Scene, F};
use super::view::Hit;
use crate::folio_look::{self as look, RunState};
use crate::layout::Rect;
use crate::render;
use crate::theme::{self, Color};
use super::transcript::*;

impl Ctx<'_> {
    pub(super) unsafe fn tools(&self, i: usize, steps: &[Step], open: bool, st: &State, cw: f32) -> Scene {
        let mut s = Scene::default();
        let r = Rect::new(0.0, 0.0, cw, LINE_H);
        let state = look::run_state(steps);
        let mut text = look::run_summary(steps);
        // A failure marks the run's icon. The line stays quiet, since a
        // failed step is often Claude finding its way.
        let mark = match state {
            RunState::Running => {
                text.push_str(" \u{2026}");
                self.pal.dim
            }
            RunState::Stopped => {
                text.push_str(" \u{b7} stopped");
                self.pal.faint
            }
            RunState::Done(0) => self.pal.dim,
            RunState::Done(n) => {
                text.push_str(&format!(" \u{b7} {n} failed"));
                theme::error().with_alpha(0.9)
            }
        };
        self.fold_row(&mut s, ('\u{E90F}', mark), &text, open, r, self.pal.dim);
        s.hit(r, Hit::Fold(i));
        let mut y = LINE_H;
        if open {
            for step in steps {
                y = self.step(&mut s, step, 0.0, y, cw, st);
            }
        } else if let Some((id, lane)) = (steps.iter())
            .filter_map(|s| Some((&s.id, s.lane.as_ref().filter(|l| at_work(s, l))?)))
            .next()
        {
            // A subagent at work says what it is doing even folded.
            let line = flow::lane_line(lane, true);
            let r = Rect::new(NEST, y, cw - NEST, 18.0);
            s.line(line, F::Small, r, self.pal.faint);
            s.hit(r, Hit::Tr(Act::Agent(i, id.clone())));
            y += 18.0;
        }
        s.h = y;
        s
    }

    /// One step's line at `x`, `y`, and its detail when open. Returns
    /// where the next goes.
    pub(super) unsafe fn step(&self, s: &mut Scene, step: &Step, x: f32, y: f32, cw: f32, st: &State) -> f32 {
        let w = cw - x;
        let row = Rect::new(x, y, w, STEP_H);
        let dot = match step.state {
            StepState::Running => self.pal.accent.mix(self.pal.fg, 0.4),
            StepState::Stopped => self.pal.faint.with_alpha(0.3),
            StepState::Done => theme::done().with_alpha(0.8),
            StepState::Failed => theme::error(),
        };
        s.fill(
            Rect::new(x + 6.0, y + STEP_H / 2.0 - 2.5, 5.0, 5.0),
            2.5,
            dot,
        );
        let detail = has_detail(step);
        let open = detail && step_open(step, st);
        let mut right = w - 4.0;
        if detail {
            let chevron = if open { '\u{E70D}' } else { '\u{E76C}' };
            s.line(
                chevron.to_string(),
                F::Icon,
                Rect::new(x + right - 14.0, y, 14.0, STEP_H),
                self.pal.faint.with_alpha(0.4),
            );
            right -= 22.0;
        }
        if let Some(e) = &step.edit {
            // Only what changed: a new file has nothing removed.
            let counts = [
                (e.removed, '-', theme::error().with_alpha(0.85)),
                (e.added, '+', theme::done().with_alpha(0.9)),
            ];
            for (n, sign, ink) in counts {
                if n > 0 {
                    let r = Rect::new(x + right - 40.0, y, 40.0, STEP_H);
                    s.line(format!("{sign}{n}"), F::Right, r, ink);
                    right -= 44.0;
                }
            }
        }
        if step.state == StepState::Running {
            s.line(
                "\u{2026}",
                F::Right,
                Rect::new(x + right - 20.0, y, 20.0, STEP_H),
                self.pal.faint,
            );
            right -= 24.0;
        }
        let mut label = if step.summary.is_empty() {
            step.tool.clone()
        } else {
            format!("{}  {}", step.tool, step.summary)
        };
        if let Some(at) = step.began.filter(|_| st.verbose) {
            label = format!("{}  {label}", clock(at));
        }
        s.line(
            label,
            F::Small,
            Rect::new(x + 20.0, y, (right - 20.0).max(0.0), STEP_H),
            self.pal.dim,
        );
        if detail {
            s.hit(row, Hit::Tr(Act::Step(step.id.clone())));
        }
        let mut y = y + STEP_H;
        if !open {
            if let Some(lane) = step.lane.as_ref().filter(|l| at_work(step, l)) {
                let line = flow::lane_line(lane, true);
                s.line(
                    line,
                    F::Small,
                    Rect::new(x + NEST, y - 3.0, w - NEST, 16.0),
                    self.pal.faint,
                );
                y += 14.0;
            }
            return y;
        }
        let dx = x + NEST;
        let dw = cw - dx;
        if let Some(lane) = &step.lane {
            y = self.lane(s, step, lane, dx, y, cw, st);
        } else if let Some(e) = &step.edit {
            y = self.diff(s, &step.id, e, dx, y, dw, st, None);
        } else {
            y = self.output(s, step, dx, y, dw, st);
        }
        if !step.images.is_empty() {
            y = self.step_images(s, step, dx, y + 6.0, dw);
        }
        y + 6.0
    }

    /// The images a step's result carried, as thumbnails in rows.
    pub(super) unsafe fn step_images(&self, s: &mut Scene, step: &Step, x: f32, y: f32, w: f32) -> f32 {
        let (mut cx, mut cy, mut row_h) = (x, y, 0.0f32);
        for (k, img) in step.images.iter().enumerate() {
            let got = (self.thumbs.borrow_mut())
                .entry(format!("{}:{k}", step.id))
                .or_insert_with(|| rich::thumbnail(self.gfx, &img.base64))
                .clone();
            let (tw, th) = match &got {
                Some((_, bw, bh)) => {
                    let f = (THUMB_W / *bw as f32).min(THUMB_H / *bh as f32).min(0.5);
                    (*bw as f32 * f, *bh as f32 * f)
                }
                None => (THUMB_H, THUMB_H * 0.75),
            };
            if cx + tw > x + w && cx > x {
                cx = x;
                cy += row_h + 8.0;
                row_h = 0.0;
            }
            let r = Rect::new(cx, cy, tw, th);
            match got {
                Some((b, _, _)) => s.items.push(rich::Item::Image(b, r)),
                None => {
                    s.fill(r, 8.0, self.pal.fg.with_alpha(0.06));
                    s.line("Image", F::Centre, r, self.pal.faint);
                }
            }
            s.stroke(r, 6.0, self.pal.fg.with_alpha(0.15), 1.0);
            s.hit(r, Hit::Tr(Act::StepImage(step.id.clone(), k)));
            cx += tw + 8.0;
            row_h = row_h.max(th);
        }
        cy + row_h
    }

    /// A map step's colour, by the sort of work it was.
    pub(super) fn kind_ink(&self, kind: StepKind, state: StepState) -> Color {
        if state == StepState::Failed {
            return theme::error().with_alpha(0.8);
        }
        let ink = match kind {
            StepKind::Read => self.pal.accent.mix(self.pal.fg, 0.55).with_alpha(0.55),
            StepKind::Edit => theme::done().with_alpha(0.85),
            StepKind::Shell => self.pal.fg.with_alpha(0.55),
            StepKind::Search => self.pal.faint.with_alpha(0.7),
            StepKind::Agent => self.pal.accent.with_alpha(0.85),
            StepKind::Web => self.pal.accent.mix(theme::done(), 0.5).with_alpha(0.7),
            StepKind::Other => self.pal.dim.with_alpha(0.6),
        };
        if state == StepState::Stopped {
            ink.with_alpha(0.3)
        } else {
            ink
        }
    }

    /// A shell step's command and the end of its output, or any step's
    /// output, in mono on a dark plate.
    pub(super) unsafe fn output(&self, s: &mut Scene, step: &Step, x: f32, y: f32, w: f32, st: &State) -> f32 {
        let all = st.all.contains(&step.id);
        let command = step.input["command"]
            .as_str()
            .filter(|_| is_shell(&step.tool));
        let (tail, hidden) = if all {
            (step.output.trim_end().to_string(), 0)
        } else {
            flow::last_lines(&step.output, OUTPUT_LINES)
        };
        let mut text = String::new();
        let mut cmd_len = 0;
        if let Some(c) = command {
            text = format!("$ {}", c.trim_end().replace('\n', "\n  "));
            cmd_len = text.encode_utf16().count() as u32;
            if !tail.is_empty() {
                text.push('\n');
            }
        }
        text.push_str(&tail);
        if text.is_empty() {
            return y;
        }
        let Some(l) = self.layout(&self.fonts.rows, &text, w - 20.0) else {
            return y;
        };
        let out_ink = if step.state == StepState::Failed {
            theme::error().mix(self.pal.fg, 0.35)
        } else {
            self.pal.fg.with_alpha(0.62)
        };
        self.paint(&l, cmd_len..text.encode_utf16().count() as u32, out_ink);
        let (_, th) = render::text_size(&l);
        let mut top = y + 2.0;
        if hidden > 0 {
            let r = Rect::new(x, top, w, 20.0);
            s.line(
                format!("Show all, {hidden} more lines above"),
                F::Small,
                Rect::new(x + 10.0, top, w - 10.0, 20.0),
                self.link_ink(),
            );
            s.hit(r, Hit::Tr(Act::All(step.id.clone())));
            top += 20.0;
        } else if all {
            let r = Rect::new(x, top, w, 20.0);
            s.line(
                "Show the end only",
                F::Small,
                Rect::new(x + 10.0, top, w - 10.0, 20.0),
                self.link_ink(),
            );
            s.hit(r, Hit::Tr(Act::All(step.id.clone())));
            top += 20.0;
        }
        let plate = Rect::new(x, top, w, th + 16.0);
        s.fill(plate, 6.0, Color::rgb(0).with_alpha(0.3));
        s.items.push(rich::Item::Clip(plate));
        s.unit(&l, x + 10.0, top + 8.0, self.pal.fg.with_alpha(0.9), &text);
        s.items.push(rich::Item::Unclip);
        plate.bottom()
    }
}
