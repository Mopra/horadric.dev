//! The conversation's blocks: each built once into a [`Scene`] and kept
//! until it changes, measured from those, and only the ones in view
//! drawn. Over them the selection, what a find matched and the hover;
//! above them the todo list and the find bar.
//!
//! A reply is markdown ([`flow`], [`rich`]); a run of tool steps folds to
//! one line and opens to a line a step, each opening to its command and
//! output, its diff or its subagent's lane; asks, questions and plans are
//! cards ([`cards`]).

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{Hash, Hasher};

use horadric_core::folio::{Block, Ending, Lane, Step, StepState, TodoStatus};
use windows::core::Interface;
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_ELLIPSE,
};
use windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT_TRAILING;
use windows_numerics::Vector2;

use super::cards;
use super::flow::{self, Pos, Pos2};
use super::rich::{self, Ctx, Fonts, Scene, F, MONO_LINE};
use super::view::{d2d, text_format, Gfx, Hit, Palette, View, FONT};
use super::Chat;
use crate::field::Field;
use crate::folio_look::{self as look, RunState};
use crate::layout::Rect;
use crate::render::{self, color};
use crate::theme::{self, Color};

const TOP: f32 = 18.0;
const GAP: f32 = 18.0;
const LINE_H: f32 = 26.0;
const STEP_H: f32 = 26.0;
const FOOTER_H: f32 = 18.0;
const BUBBLE_X: f32 = 16.0;
const BUBBLE_Y: f32 = 11.0;
const BUBBLE_SHARE: f32 = 0.82;
/// How far a step's detail and a lane sit in from their line.
const NEST: f32 = 22.0;
/// Shell output shows its last lines until "show all".
const OUTPUT_LINES: usize = 14;
/// A diff shows its first rows until "show all".
const DIFF_ROWS: usize = 40;
/// A prompt's thumbnail fits in this, in DIPs.
const THUMB_W: f32 = 180.0;
const THUMB_H: f32 = 135.0;
/// What a raw line shows of itself.
const RAW_MAX: usize = 400;
/// The find bar's width.
const FIND_W: f32 = 320.0;
/// About how many blocks a transcript shows, and lays out, at a time; the
/// rest wait behind "Show earlier messages".
pub(super) const SHOWN: usize = 300;
/// Room for that line at the top.
const EARLIER_H: f32 = 30.0;

/// What a click on a part of the transcript does, beyond folding a block
/// and answering an ask once.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Act {
    Copy(String),
    /// A web address, to the project's browser.
    Link(String),
    /// A file named in a reply, to the file viewer.
    Path(String, Option<u32>),
    /// A step opened or closed, by its tool use id.
    Step(String),
    /// All of a step's output or diff, or back to its end.
    All(String),
    /// Ask: allow, and add its suggested rules.
    Always(String),
    /// Ask: open the reason field, or deny with what it holds.
    Deny(String),
    /// Ask or plan: close the field without answering.
    Cancel(String),
    /// Question: option `.2` of question `.1` picked or unpicked.
    Pick(String, usize, usize),
    Submit(String),
    /// Plan: approved, into a mode when given.
    Approve(String, Option<&'static str>),
    /// Plan: open the feedback field, or send it back with what it holds.
    Keep(String),
    /// A text field takes the keyboard.
    Field(String),
    Todos,
    /// More of the conversation above what shows.
    Earlier,
    FindPrev,
    FindNext,
    FindClose,
}

/// A text field on a card or the find bar's, by its id.
pub(super) fn deny_field(id: &str) -> String {
    format!("deny:{id}")
}
pub(super) fn keep_field(id: &str) -> String {
    format!("keep:{id}")
}
pub(super) fn other_field(id: &str, q: usize) -> String {
    format!("other{q}:{id}")
}
pub(super) const FIND_FIELD: &str = "find";

/// The find bar: what is looked for and where it was found.
pub(super) struct Find {
    pub(super) field: Field,
    pub(super) hits: Vec<Pos2>,
    /// Which hit is the current one.
    pub(super) at: usize,
    /// The query and the text's size the hits are for, so they are found
    /// again only when either changes.
    pub(super) for_: (String, usize),
}

/// What the human has done to the transcript: steps opened, fields typed
/// in, picks, the selection, the find.
#[derive(Default)]
pub(super) struct State {
    /// Steps opened or closed by a click, against their default: a failed
    /// one opens by itself.
    pub(super) flipped: HashSet<String>,
    /// Steps showing all their output or diff.
    pub(super) all: HashSet<String>,
    pub(super) fields: HashMap<String, Field>,
    /// The field with the keyboard, the composer's when None.
    pub(super) focus: Option<String>,
    /// Asks whose deny reason is open, plans whose feedback is.
    pub(super) denying: HashSet<String>,
    pub(super) keeping: HashSet<String>,
    /// Per question card, per question, the options picked.
    pub(super) picks: HashMap<String, Vec<BTreeSet<usize>>>,
    pub(super) todos_open: bool,
    pub(super) sel: Option<(Pos, Pos)>,
    pub(super) dragging: bool,
    pub(super) mouse: Option<(f32, f32)>,
    pub(super) find: Option<Find>,
    /// The first block shown, None until the first frame picks one.
    pub(super) from: Option<usize>,
    /// From the last frame: each block's top in the content, the prompts'
    /// blocks, and the text in view with where it was drawn.
    pub(super) tops: Vec<f32>,
    pub(super) prompts: Vec<usize>,
    pub(super) shown: Vec<(Rect, usize, u32)>,
    pub(super) area: Rect,
}

impl State {
    pub(super) fn field(&mut self, id: &str) -> &mut Field {
        self.fields
            .entry(id.to_string())
            .or_insert_with(|| Field::new("", true, 4000))
    }

    pub(super) fn text(&self, id: &str) -> &str {
        self.fields.get(id).map_or("", |f| f.text.as_str())
    }
}

/// A block built, and what it was built from.
struct Built {
    stamp: u64,
    scene: Scene,
    /// A reply's paragraphs, each with its scene, so a streaming reply
    /// lays out only the paragraph that changed.
    paras: Vec<(flow::Para, Scene)>,
}

/// Decoded thumbnails by block and image, None where one would not decode.
type Thumbs = HashMap<(usize, usize), Option<(ID2D1Bitmap, u32, u32)>>;

/// What is kept between frames for drawing.
#[derive(Default)]
pub(super) struct Cache {
    /// The render target, width and colours the scenes were built for.
    made_for: (usize, u32, [u32; 2]),
    fonts: Option<Fonts>,
    brushes: RefCell<HashMap<u32, ID2D1SolidColorBrush>>,
    blocks: Vec<Option<Built>>,
    /// Decoded thumbnails by block and image.
    thumbs: Thumbs,
}

impl Cache {
    pub(super) fn unit(&self, block: usize, unit: u32) -> Option<&rich::Unit> {
        let b = self.blocks.get(block)?.as_ref()?;
        b.scene.units.get(unit as usize)
    }

    /// Every piece of text in the transcript, in order.
    pub(super) fn texts(&self) -> Vec<flow::Text<'_>> {
        (self.blocks.iter().enumerate())
            .filter_map(|(i, b)| Some((i, b.as_ref()?)))
            .flat_map(|(i, b)| {
                (b.scene.units.iter().enumerate()).map(move |(k, u)| flow::Text {
                    block: i,
                    unit: k as u32,
                    text: &u.text,
                })
            })
            .collect()
    }
}

/// The transcript's state and cache, kept with its chat.
#[derive(Default)]
pub(super) struct Ui {
    pub(super) state: State,
    pub(super) cache: Cache,
}

fn key_of(c: Color) -> u32 {
    let k = |v: f32| (v.clamp(0.0, 1.0) * 255.0) as u32;
    k(c.r) << 24 | k(c.g) << 16 | k(c.b) << 8 | k(c.a)
}

fn hash_str(h: &mut DefaultHasher, s: &str) {
    s.hash(h);
}

fn hash_step(h: &mut DefaultHasher, s: &Step, st: &State) {
    hash_str(h, &s.id);
    hash_str(h, &s.summary);
    (s.state as u8).hash(h);
    s.output.len().hash(h);
    hash_str(h, s.input["command"].as_str().unwrap_or_default());
    if let Some(e) = &s.edit {
        (e.added, e.removed, e.hunks.len(), e.changes.len()).hash(h);
    }
    st.flipped.contains(&s.id).hash(h);
    st.all.contains(&s.id).hash(h);
    if let Some(l) = &s.lane {
        hash_lane(h, l, st);
    }
}

fn hash_lane(h: &mut DefaultHasher, l: &Lane, st: &State) {
    hash_str(h, &l.agent_type);
    l.text.len().hash(h);
    hash_str(h, &l.progress);
    (l.tokens, l.tool_uses).hash(h);
    for s in &l.steps {
        hash_step(h, s, st);
    }
}

fn hash_field(h: &mut DefaultHasher, st: &State, id: &str) {
    if let Some(f) = st.fields.get(id) {
        hash_str(h, &f.text);
        (f.caret, f.anchor).hash(h);
    }
    (st.focus.as_deref() == Some(id)).hash(h);
}

/// What block `b` is built from: when this changes, it is built again.
fn stamp(b: &Block, open: bool, st: &State) -> u64 {
    let mut h = DefaultHasher::new();
    open.hash(&mut h);
    std::mem::discriminant(b).hash(&mut h);
    match b {
        // These only grow while they stream, and a reply settled after
        // an interrupt is a different length.
        Block::Prompt(t) | Block::Reply(t) | Block::Thinking(t) | Block::Raw(t) => {
            t.len().hash(&mut h);
            let mut end = t.len().saturating_sub(64);
            while !t.is_char_boundary(end) {
                end += 1;
            }
            hash_str(&mut h, &t[end..]);
        }
        Block::Images(v) => v.len().hash(&mut h),
        Block::Tools(steps) => {
            for s in steps {
                hash_step(&mut h, s, st);
            }
        }
        Block::Ask {
            request_id, state, ..
        } => {
            (*state as u8).hash(&mut h);
            st.denying.contains(request_id).hash(&mut h);
            st.all.contains(request_id).hash(&mut h);
            hash_field(&mut h, st, &deny_field(request_id));
        }
        Block::Question {
            request_id,
            state,
            answers,
            questions,
            ..
        } => {
            (*state as u8).hash(&mut h);
            answers.hash(&mut h);
            st.picks.get(request_id).hash(&mut h);
            for q in 0..questions.len() {
                hash_field(&mut h, st, &other_field(request_id, q));
            }
        }
        Block::Plan {
            request_id,
            state,
            plan,
            feedback,
            ..
        } => {
            (*state as u8).hash(&mut h);
            plan.len().hash(&mut h);
            hash_str(&mut h, feedback);
            st.keeping.contains(request_id).hash(&mut h);
            hash_field(&mut h, st, &keep_field(request_id));
        }
        Block::Footer {
            duration_ms,
            ending,
            ..
        } => (*duration_ms, *ending as u8).hash(&mut h),
        Block::Notice(n) => format!("{n:?}").hash(&mut h),
    }
    h.finish()
}

/// The room left under a block before the next.
fn gap_after(b: &Block) -> f32 {
    match b {
        Block::Footer { .. } => GAP + 16.0,
        Block::Thinking(_) | Block::Tools(_) | Block::Notice(_) | Block::Raw(_) => GAP / 2.0,
        Block::Prompt(_) => GAP / 2.0,
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

/// Whether a step opens to more than its line.
fn has_detail(s: &Step) -> bool {
    s.edit.is_some()
        || s.lane.is_some()
        || !s.output.trim().is_empty()
        || is_shell(&s.tool) && s.input["command"].is_string()
}

fn is_shell(tool: &str) -> bool {
    matches!(tool, "Bash" | "PowerShell" | "BashOutput")
}

/// Whether a step shows its detail: a failed one does until closed.
fn step_open(s: &Step, st: &State) -> bool {
    (s.state == StepState::Failed) != st.flipped.contains(&s.id)
}

impl Ctx<'_> {
    /// A folded block's one line: its icon, its text, and a chevron saying
    /// a click opens it.
    fn fold_row(
        &self,
        s: &mut Scene,
        (icon, mark): (char, Color),
        text: &str,
        open: bool,
        r: Rect,
        ink: Color,
    ) {
        s.line(
            icon.to_string(),
            F::Icon,
            Rect::new(r.x, r.y, 16.0, r.h),
            mark,
        );
        s.line(
            text,
            F::Line,
            Rect::new(r.x + 22.0, r.y, r.w - 44.0, r.h),
            ink,
        );
        let chevron = if open { '\u{E70D}' } else { '\u{E76C}' };
        s.line(
            chevron.to_string(),
            F::Icon,
            Rect::new(r.right() - 16.0, r.y, 16.0, r.h),
            self.pal.faint.with_alpha(0.4),
        );
    }

    unsafe fn prompt(&self, t: &str, cw: f32) -> Scene {
        let mut s = Scene::default();
        let max = cw * BUBBLE_SHARE - 2.0 * BUBBLE_X;
        let Some(l) = self.layout(&self.fonts.body, t, max) else {
            return s;
        };
        let (tw, th) = render::text_size(&l);
        let bw = tw.min(max) + 2.0 * BUBBLE_X;
        let r = Rect::new(cw - bw, 0.0, bw, th + 2.0 * BUBBLE_Y);
        s.fill(
            r,
            14.0,
            self.pal.bg.mix(self.pal.accent, 0.2).mix(self.pal.fg, 0.04),
        );
        s.stroke(r, 14.0, self.pal.accent.with_alpha(0.28), 1.0);
        s.unit(&l, r.x + BUBBLE_X, BUBBLE_Y, self.pal.fg, t);
        s.h = r.h;
        s
    }

    unsafe fn images(
        &self,
        i: usize,
        images: &[horadric_core::folio::PromptImage],
        thumbs: &mut Thumbs,
        cw: f32,
    ) -> Scene {
        let mut s = Scene::default();
        let sizes: Vec<(f32, f32, Option<ID2D1Bitmap>)> = (images.iter().enumerate())
            .map(|(k, img)| {
                let got = thumbs
                    .entry((i, k))
                    .or_insert_with(|| rich::thumbnail(self.gfx, &img.base64));
                match got {
                    Some((b, w, h)) => {
                        let k = (THUMB_W / *w as f32).min(THUMB_H / *h as f32).min(0.5);
                        (*w as f32 * k, *h as f32 * k, Some(b.clone()))
                    }
                    None => (THUMB_H, THUMB_H * 0.75, None),
                }
            })
            .collect();
        // Right aligned under the prompt, in rows.
        let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
        let mut used = 0.0;
        for (k, (w, _, _)) in sizes.iter().enumerate() {
            if used + w > cw && !rows.last().is_some_and(Vec::is_empty) {
                rows.push(Vec::new());
                used = 0.0;
            }
            if let Some(r) = rows.last_mut() {
                r.push(k);
            }
            used += w + 8.0;
        }
        let mut y = 4.0;
        for row in rows {
            let width: f32 = row.iter().map(|&k| sizes[k].0 + 8.0).sum::<f32>() - 8.0;
            let mut x = cw - width;
            let mut rh: f32 = 0.0;
            for k in row {
                let (w, h, b) = &sizes[k];
                let r = Rect::new(x, y, *w, *h);
                match b {
                    Some(b) => s.items.push(rich::Item::Image(b.clone(), r)),
                    None => {
                        s.fill(r, 8.0, self.pal.fg.with_alpha(0.06));
                        s.line("Image", F::Centre, r, self.pal.faint);
                    }
                }
                s.stroke(r, 6.0, self.pal.fg.with_alpha(0.15), 1.0);
                x += w + 8.0;
                rh = rh.max(*h);
            }
            y += rh + 8.0;
        }
        s.h = y - 4.0;
        s
    }

    /// A reply as markdown, with a copy button under it on hover. Each
    /// paragraph's scene from before is kept while it is unchanged.
    unsafe fn reply(
        &self,
        t: &str,
        cw: f32,
        old: Vec<(flow::Para, Scene)>,
    ) -> (Scene, Vec<(flow::Para, Scene)>) {
        let mut s = Scene::default();
        let mut old = old.into_iter();
        let mut paras = Vec::new();
        let mut y = 0.0;
        for p in flow::paras(t) {
            let ps = match old.next() {
                Some((was, scene)) if was == p => scene,
                _ => self.para(&p, cw),
            };
            y += p.gap;
            s.append(&ps, 0.0, y);
            y += ps.h;
            paras.push((p, ps));
        }
        let copy = Rect::new(-6.0, y + 4.0, 64.0, 22.0);
        s.hover.push(rich::Item::Line(
            "\u{E8C8}".into(),
            F::Icon,
            Rect::new(copy.x + 6.0, copy.y, 16.0, copy.h),
            self.pal.faint,
        ));
        s.hover.push(rich::Item::Line(
            "Copy".into(),
            F::Small,
            Rect::new(copy.x + 26.0, copy.y, 40.0, copy.h),
            self.pal.faint,
        ));
        s.hover_hits.push((copy, Hit::Tr(Act::Copy(t.to_string()))));
        s.h = y + 26.0;
        (s, paras)
    }

    unsafe fn thinking(&self, i: usize, t: &str, open: bool, cw: f32) -> Scene {
        let mut s = Scene::default();
        let r = Rect::new(0.0, 0.0, cw, LINE_H);
        self.fold_row(
            &mut s,
            ('\u{E82F}', self.pal.faint),
            &look::thinking_line(t),
            open,
            r,
            self.pal.faint,
        );
        s.hit(r, Hit::Fold(i));
        s.h = LINE_H;
        if open {
            if let Some(l) = self.layout(&self.gfx.italic, t, cw - NEST) {
                let (_, h) = render::text_size(&l);
                s.fill(
                    Rect::new(7.0, LINE_H + 2.0, 1.5, h + 2.0),
                    0.0,
                    self.pal.faint.with_alpha(0.25),
                );
                s.unit(&l, NEST, LINE_H + 4.0, self.pal.dim, t);
                s.h = LINE_H + 8.0 + h;
            }
        }
        s
    }

    unsafe fn tools(&self, i: usize, steps: &[Step], open: bool, st: &State, cw: f32) -> Scene {
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
        } else if let Some(lane) = (steps.iter())
            .filter(|s| s.state == StepState::Running)
            .find_map(|s| s.lane.as_ref())
        {
            // A subagent at work says what it is doing even folded.
            let line = flow::lane_line(lane, true);
            s.line(
                line,
                F::Small,
                Rect::new(NEST, y, cw - NEST, 18.0),
                self.pal.faint,
            );
            y += 18.0;
        }
        s.h = y;
        s
    }

    /// One step's line at `x`, `y`, and its detail when open. Returns
    /// where the next goes.
    unsafe fn step(&self, s: &mut Scene, step: &Step, x: f32, y: f32, cw: f32, st: &State) -> f32 {
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
        let label = if step.summary.is_empty() {
            step.tool.clone()
        } else {
            format!("{}  {}", step.tool, step.summary)
        };
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
            if let Some(lane) = step
                .lane
                .as_ref()
                .filter(|_| step.state == StepState::Running)
            {
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
            y = self.diff(s, &step.id, e, dx, y, dw, st);
        } else {
            y = self.output(s, step, dx, y, dw, st);
        }
        y + 6.0
    }

    /// A shell step's command and the end of its output, or any step's
    /// output, in mono on a dark plate.
    unsafe fn output(&self, s: &mut Scene, step: &Step, x: f32, y: f32, w: f32, st: &State) -> f32 {
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

    /// An edit's file, as a link that opens it, and its diff inline. `id`
    /// is what "Show all" is kept under: the step's, or the ask's.
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
    ) -> f32 {
        let head = Rect::new(x, y, w, 22.0);
        let what = if e.created { "Created" } else { "Changed" };
        s.line(what, F::Small, Rect::new(x, y, 60.0, 22.0), self.pal.faint);
        let file = Rect::new(x + 60.0, y, (w - 60.0).min(500.0), 22.0);
        let shown = flow::project_path(self.dir, &e.path).unwrap_or_else(|| e.path.clone());
        s.line(shown, F::Small, file, self.link_ink());
        s.hit(file, Hit::Tr(Act::Path(e.path.clone(), None)));
        let _ = head;
        let mut y = y + 24.0;
        let rows = flow::diff_rows(e);
        if rows.is_empty() {
            return y;
        }
        let all = st.all.contains(id);
        let shown = if all {
            rows.len()
        } else {
            rows.len().min(DIFF_ROWS)
        };
        let digits = (rows.iter())
            .filter_map(|r| r.old.max(r.new))
            .max()
            .unwrap_or(1)
            .to_string()
            .len() as f32;
        let gw = digits * 7.4 + 10.0;
        let num = |n: Option<u32>| n.map_or(String::new(), |n| n.to_string());
        let olds: Vec<String> = rows[..shown].iter().map(|r| num(r.old)).collect();
        let news: Vec<String> = rows[..shown].iter().map(|r| num(r.new)).collect();
        let text: Vec<String> = (rows[..shown].iter())
            .map(|r| match r.mark {
                '~' => "\u{22ef}".to_string(),
                m => format!("{m} {}", r.text),
            })
            .collect();
        let text = text.join("\n");
        let plate = Rect::new(x, y, w, shown as f32 * MONO_LINE + 12.0);
        s.fill(plate, 6.0, Color::rgb(0).with_alpha(0.3));
        s.items.push(rich::Item::Clip(plate));
        for (k, r) in rows[..shown].iter().enumerate() {
            let tint = match r.mark {
                '+' => theme::done().with_alpha(0.14),
                '-' => theme::error().with_alpha(0.16),
                _ => continue,
            };
            s.fill(
                Rect::new(x, y + 6.0 + k as f32 * MONO_LINE, w, MONO_LINE),
                0.0,
                tint,
            );
        }
        for (k, nums) in [olds, news].iter().enumerate() {
            if let Some(l) = self.layout(&self.fonts.rows, &nums.join("\n"), gw - 6.0) {
                let _ = l.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING);
                s.text(&l, x + k as f32 * gw, y + 6.0, self.pal.faint);
            }
        }
        if let Some(l) = self.layout(&self.fonts.rows, &text, 100_000.0) {
            s.unit(
                &l,
                x + 2.0 * gw + 6.0,
                y + 6.0,
                self.pal.fg.with_alpha(0.88),
                &text,
            );
        }
        s.items.push(rich::Item::Unclip);
        y = plate.bottom() + 2.0;
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

    /// A subagent's lane: its type and progress, its steps folded the
    /// same way, and what it reported.
    #[allow(clippy::too_many_arguments)]
    unsafe fn lane(
        &self,
        s: &mut Scene,
        step: &Step,
        lane: &Lane,
        x: f32,
        y: f32,
        cw: f32,
        st: &State,
    ) -> f32 {
        let top = y;
        let running = step.state == StepState::Running;
        let w = cw - x;
        s.line(
            flow::lane_line(lane, running),
            F::Small,
            Rect::new(x + 12.0, y, w - 12.0, 20.0),
            self.pal.faint,
        );
        let mut y = y + 20.0;
        for ls in &lane.steps {
            y = self.step(s, ls, x + 12.0, y, cw, st);
        }
        let report = lane.text.trim();
        if !report.is_empty() {
            y += 4.0;
            let mut paras = Scene::default();
            let mut py = 0.0;
            for p in flow::paras(report) {
                let ps = self.para(&p, w - 12.0);
                py += p.gap;
                paras.append(&ps, 0.0, py);
                py += ps.h;
            }
            s.append(&paras, x + 12.0, y);
            y += py;
        }
        s.fill(
            Rect::new(x + 2.0, top + 2.0, 2.0, y - top - 2.0),
            1.0,
            self.pal.accent.with_alpha(0.4),
        );
        y + 4.0
    }

    unsafe fn notice(&self, n: &horadric_core::folio::Notice, cw: f32) -> Scene {
        let mut s = Scene::default();
        let text = flow::notice(n);
        let Some(l) = self.one_line(&self.gfx.small, &text) else {
            return s;
        };
        let (tw, _) = render::text_size(&l);
        let tw = tw.min(cw - 80.0);
        let _ = l.SetMaxWidth(tw);
        let x = (cw - tw) / 2.0;
        let ink = self.pal.faint;
        s.fill(
            Rect::new(0.0, 11.0, x - 12.0, 1.0),
            0.0,
            self.pal.fg.with_alpha(0.08),
        );
        s.fill(
            Rect::new(x + tw + 12.0, 11.0, cw - x - tw - 12.0, 1.0),
            0.0,
            self.pal.fg.with_alpha(0.08),
        );
        s.unit(&l, x, 3.0, ink, &text);
        s.h = 22.0;
        s
    }

    fn footer(&self, b: &Block, cw: f32) -> Scene {
        let mut s = Scene::default();
        let Block::Footer {
            duration_ms,
            cost_usd,
            ending,
            usage,
        } = b
        else {
            return s;
        };
        let ink = match ending {
            Ending::Failed => theme::error().with_alpha(0.8),
            Ending::Done | Ending::Stopped => self.pal.faint,
        };
        let line = flow::footer(*duration_ms, *cost_usd, *ending, usage);
        s.line(line, F::Small, Rect::new(0.0, 0.0, cw, FOOTER_H), ink);
        // A short rule under the turn, in the project's colour.
        s.fill(
            Rect::new(0.0, FOOTER_H + 8.0, 28.0, 1.0),
            0.0,
            self.pal.accent.with_alpha(0.35),
        );
        s.h = FOOTER_H;
        s
    }

    /// A line the model did not know, kept: folded to what kind it is,
    /// open to the line itself.
    unsafe fn raw(&self, i: usize, t: &str, open: bool, cw: f32) -> Scene {
        let mut s = Scene::default();
        let r = Rect::new(0.0, 0.0, cw, LINE_H);
        let label = flow::raw_label(t);
        self.fold_row(
            &mut s,
            ('\u{E9CE}', self.pal.faint),
            &label,
            open,
            r,
            self.pal.faint,
        );
        s.hit(r, Hit::Fold(i));
        s.h = LINE_H;
        if open {
            let t = raw_text(t);
            if let Some(l) = self.layout(&self.fonts.code, &t, cw - NEST) {
                s.unit(&l, NEST, LINE_H + 2.0, self.pal.faint, &t);
                s.h = LINE_H + 6.0 + render::text_size(&l).1;
            }
        }
        s
    }
}

impl View {
    /// The conversation in `area`, above the composer: built where it
    /// changed, measured, scrolled to follow the end or kept in bounds,
    /// then drawn where it is in view.
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
        let Ui { state: st, cache } = &mut chat.tr;
        super::select::refind_in(st, cache);
        let made_for = (
            rt.as_raw() as usize,
            cw.to_bits(),
            [key_of(pal.fg), key_of(pal.accent)],
        );
        if cache.made_for != made_for {
            // Brushes and bitmaps belong to a render target, and every
            // layout to a width.
            let fonts = cache.fonts.take();
            *cache = Cache {
                made_for,
                fonts,
                ..Cache::default()
            };
        }
        if cache.fonts.is_none() {
            cache.fonts = Fonts::new(&self.shared.gpu).ok();
        }
        let Cache {
            fonts,
            brushes,
            blocks,
            thumbs,
            ..
        } = cache;
        let Some(fonts) = fonts.as_ref() else {
            return;
        };
        let ctx = Ctx {
            gfx,
            gpu: &self.shared.gpu,
            pal,
            fonts,
            brushes,
            dir: &chat.dir,
        };

        // The todo list stays at the top, the conversation scrolling under.
        let mut area = *area;
        let todo_h = if chat.folio.todos.is_empty() {
            0.0
        } else if st.todos_open {
            34.0 + chat.folio.todos.len() as f32 * 22.0 + 8.0
        } else {
            34.0
        };
        let todo_r = Rect::new(cx - 8.0, area.y + 10.0, cw + 16.0, todo_h);
        if todo_h > 0.0 {
            area.y += todo_h + 14.0;
            area.h -= todo_h + 14.0;
        }

        // Build what changed, of what shows.
        let folio = &chat.folio;
        let len = folio.blocks.len();
        let turns: Vec<usize> = (folio.blocks.iter().enumerate())
            .filter(|(_, b)| matches!(b, Block::Prompt(_)))
            .map(|(i, _)| i)
            .collect();
        let mut from =
            (st.from).map_or_else(|| flow::shown_from(&turns, len, SHOWN), |f| f.min(len));
        // Following a long session, what scrolled far up folds away, so
        // what is kept laid out stays bounded.
        if chat.follow && len - from > 2 * SHOWN {
            from = flow::shown_from(&turns, len, SHOWN);
        }
        st.from = Some(from);
        blocks.truncate(len);
        blocks.resize_with(len, || None);
        for b in &mut blocks[..from] {
            *b = None;
        }
        for (i, b) in folio.blocks.iter().enumerate().skip(from) {
            let open = chat.open.contains(&i);
            let stamp = stamp(b, open, st);
            if blocks[i].as_ref().is_some_and(|x| x.stamp == stamp) {
                continue;
            }
            let old = blocks[i].take().map(|b| b.paras).unwrap_or_default();
            let mut paras = Vec::new();
            let scene = match b {
                Block::Prompt(t) => ctx.prompt(t, cw),
                Block::Images(v) => ctx.images(i, v, thumbs, cw),
                Block::Reply(t) if t.trim().is_empty() => Scene::default(),
                Block::Reply(t) => {
                    let (s, p) = ctx.reply(t, cw, old);
                    paras = p;
                    s
                }
                Block::Thinking(t) => ctx.thinking(i, t, open, cw),
                Block::Tools(steps) => ctx.tools(i, steps, open, st, cw),
                Block::Ask { .. } => cards::ask(&ctx, b, st, cw),
                Block::Question { .. } => cards::question(&ctx, b, st, cw),
                Block::Plan { .. } => cards::plan(&ctx, b, st, cw),
                Block::Footer { .. } => ctx.footer(b, cw),
                Block::Notice(n) => ctx.notice(n, cw),
                Block::Raw(t) => ctx.raw(i, t, open, cw),
            };
            blocks[i] = Some(Built {
                stamp,
                scene,
                paras,
            });
        }

        // Measure, then follow the end or keep the scroll in bounds.
        let tail = if chat.folio.busy { 28.0 } else { 0.0 }
            + if chat.ended || chat.error.is_some() {
                LINE_H + GAP
            } else {
                0.0
            };
        st.tops.clear();
        st.prompts.clear();
        let earlier = from > 0;
        let mut y = TOP + if earlier { EARLIER_H } else { 0.0 };
        for (i, b) in chat.folio.blocks.iter().enumerate() {
            st.tops.push(y);
            let h = blocks[i].as_ref().map_or(0.0, |b| b.scene.h);
            if h > 0.0 {
                if matches!(b, Block::Prompt(_)) {
                    st.prompts.push(i);
                }
                y += h + gap_after(b);
            }
        }
        let content = y + tail + 8.0;
        chat.content_h = content;
        chat.view_h = area.h;
        chat.scroll = if chat.follow {
            (content - area.h).max(0.0)
        } else {
            look::clamp_scroll(chat.scroll, content, area.h)
        };
        st.area = area;

        rt.PushAxisAlignedClip(&d2d(&area), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        let waits = chat.pending.is_some();
        if chat.loading.is_some() {
            let r = Rect::new(cx, area.y + TOP, cw, LINE_H);
            gfx.text_in("Reading the conversation\u{2026}", &gfx.line, &r, pal.dim);
        } else if chat.folio.blocks.is_empty() && chat.error.is_none() && !chat.ended && !waits {
            self.empty(gfx, pal, &chat.dir, &area);
        }
        let origin = area.y - chat.scroll;
        let mouse = st.mouse;
        st.shown.clear();
        if earlier {
            let r = Rect::new(cx, origin + TOP, cw, EARLIER_H - 8.0);
            if r.bottom() >= area.y {
                let say = format!("Show earlier messages ({from} more)");
                let ink = pal.accent.mix(pal.fg, 0.45);
                gfx.text_in(&say, &gfx.small, &r, ink);
                if let Some(r) = look::clip(&r, &area) {
                    hits.push((r, Hit::Tr(Act::Earlier)));
                }
            }
        }
        for (i, b) in chat.folio.blocks.iter().enumerate() {
            let Some(built) = blocks[i].as_ref() else {
                continue;
            };
            let s = &built.scene;
            let top = origin + st.tops[i];
            if s.h <= 0.0 || top + s.h + 30.0 < area.y || top > area.bottom() {
                continue;
            }
            let whole = Rect::new(cx - 8.0, top, cw + 16.0, s.h + gap_after(b));
            let hovered = mouse.is_some_and(|(mx, my)| whole.contains(mx, my));
            rich::draw(gfx, fonts, s, cx, top, hovered);
            let mine = s.hits.iter().chain(s.hover_hits.iter().filter(|_| hovered));
            // Drawing is clipped to the area; what is hidden must not take
            // clicks either.
            hits.extend(mine.filter_map(|(r, h)| {
                let r = Rect::new(r.x + cx, r.y + top, r.w, r.h);
                Some((look::clip(&r, &area)?, h.clone()))
            }));
            for (k, u) in s.units.iter().enumerate() {
                let r = Rect::new(cx + u.x, top + u.y, u.w, u.h);
                if r.bottom() >= area.y && r.y <= area.bottom() {
                    st.shown.push((r, i, k as u32));
                }
            }
        }
        self.marks(gfx, pal, st, cache_units(blocks), &area);
        let mut y = origin + content - tail - 8.0;
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

        if todo_h > 0.0 {
            self.todos(gfx, pal, &chat.folio.todos, st.todos_open, &todo_r, hits);
        }
        if st.find.is_some() {
            self.find_bar(gfx, pal, st, cx, cw, &area, hits);
        }
        // Whatever button the pointer is on lights up.
        if let Some((mx, my)) = mouse {
            let lit = (hits.iter().rev()).find(|(r, _)| r.contains(mx, my));
            if let Some((r, h)) = lit {
                let text_link = matches!(h, Hit::Tr(Act::Link(_) | Act::Path(..) | Act::Field(_)));
                if !text_link {
                    gfx.fill_round(r, 6.0, pal.fg.with_alpha(0.06));
                }
            }
        }
    }

    /// The selection and what a find matched, over the text in view.
    unsafe fn marks(
        &self,
        gfx: &Gfx,
        pal: &Palette,
        st: &State,
        units: impl Fn(
            usize,
            u32,
        ) -> Option<(
            windows::Win32::Graphics::DirectWrite::IDWriteTextLayout,
            u32,
        )>,
        area: &Rect,
    ) {
        let _ = area;
        let current = st.find.as_ref().and_then(|f| f.hits.get(f.at));
        for (r, block, unit) in &st.shown {
            let Some((layout, len)) = units(*block, *unit) else {
                continue;
            };
            let paint = |range: std::ops::Range<u32>, c: Color| {
                for b in render::range_rects(&layout, range.start, range.end - range.start) {
                    gfx.fill(&Rect::new(r.x + b.x, r.y + b.y, b.w.max(3.0), b.h), c);
                }
            };
            if let Some(f) = &st.find {
                for h in f
                    .hits
                    .iter()
                    .filter(|h| h.block == *block && h.unit == *unit)
                {
                    let now = current == Some(h);
                    let c = if now {
                        Color::rgb(0xF2A33A).with_alpha(0.55)
                    } else {
                        Color::rgb(0xE8C547).with_alpha(0.28)
                    };
                    paint(h.range.clone(), c);
                }
            }
            if let Some((a, b)) = st.sel {
                if let Some(range) = flow::unit_range(a, b, *block, *unit, len) {
                    paint(range, pal.accent.mix(pal.fg, 0.3).with_alpha(0.38));
                }
            }
        }
    }

    /// The todo list, pinned over the top of the conversation.
    unsafe fn todos(
        &self,
        gfx: &Gfx,
        pal: &Palette,
        todos: &[horadric_core::folio::Todo],
        open: bool,
        r: &Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        gfx.fill_round(r, 10.0, pal.bg.mix(pal.fg, 0.06));
        gfx.stroke_round(r, 10.0, pal.fg.with_alpha(0.1), 1.0);
        let head = Rect::new(r.x + 12.0, r.y + 2.0, r.w - 24.0, 30.0);
        gfx.text_in(
            "\u{E9D5}",
            &gfx.icon,
            &Rect::new(head.x, head.y, 16.0, head.h),
            pal.accent.mix(pal.fg, 0.4),
        );
        gfx.text_in(
            &flow::todo_line(todos),
            &gfx.line,
            &Rect::new(head.x + 24.0, head.y, head.w - 48.0, head.h),
            pal.dim,
        );
        let chevron = if open { "\u{E70E}" } else { "\u{E70D}" };
        gfx.text_in(
            chevron,
            &gfx.icon,
            &Rect::new(head.right() - 16.0, head.y, 16.0, head.h),
            pal.faint,
        );
        hits.push((Rect::new(r.x, r.y, r.w, 34.0), Hit::Tr(Act::Todos)));
        if !open {
            return;
        }
        for (k, t) in todos.iter().enumerate() {
            let y = r.y + 34.0 + k as f32 * 22.0;
            let (icon, ink, text) = match t.status {
                TodoStatus::Completed => ("\u{E73A}", pal.faint, &t.content),
                TodoStatus::InProgress => (
                    "\u{E7C3}",
                    pal.fg,
                    if t.active_form.is_empty() {
                        &t.content
                    } else {
                        &t.active_form
                    },
                ),
                TodoStatus::Pending => ("\u{E739}", pal.dim, &t.content),
            };
            gfx.text_in(icon, &gfx.icon, &Rect::new(head.x, y, 16.0, 22.0), ink);
            gfx.text_in(
                text,
                &gfx.small,
                &Rect::new(head.x + 24.0, y, head.w - 24.0, 22.0),
                ink,
            );
        }
    }

    /// The find bar over the top right of the conversation.
    #[allow(clippy::too_many_arguments)]
    unsafe fn find_bar(
        &self,
        gfx: &Gfx,
        pal: &Palette,
        st: &State,
        cx: f32,
        cw: f32,
        area: &Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let Some(f) = &st.find else {
            return;
        };
        let w = FIND_W.min(cw);
        let r = Rect::new(cx + cw - w, area.y + 14.0, w, 34.0);
        gfx.fill_round(&r, 10.0, pal.bg.mix(pal.fg, 0.1));
        let focused = st.focus.as_deref() == Some(FIND_FIELD);
        let edge = if focused {
            pal.accent.with_alpha(0.7)
        } else {
            pal.fg.with_alpha(0.15)
        };
        gfx.stroke_round(&r, 10.0, edge, 1.0);
        let count = if f.field.text.is_empty() {
            String::new()
        } else if f.hits.is_empty() {
            "None".to_string()
        } else {
            format!("{} of {}", f.at + 1, f.hits.len())
        };
        let buttons = 3.0 * 26.0 + 6.0;
        let count_w = 64.0;
        let text_r = Rect::new(r.x + 12.0, r.y, r.w - buttons - count_w - 16.0, r.h);
        hits.push((text_r, Hit::Tr(Act::Field(FIND_FIELD.to_string()))));
        if f.field.text.is_empty() {
            gfx.text_in("Find in the conversation", &gfx.line, &text_r, pal.faint);
        } else {
            gfx.text_in(&f.field.text, &gfx.line, &text_r, pal.fg);
        }
        if focused {
            if let Ok(l) = render::wrapped(&self.shared.gpu, &gfx.line, &f.field.text, 10_000.0) {
                let at = crate::field::utf16_at(&f.field.text, f.field.caret);
                let c = render::caret_at(&l, at);
                let (_, lh) = render::text_size(&l);
                let x = text_r.x + c.x.min(text_r.w);
                gfx.fill(&Rect::new(x, r.y + (r.h - lh) / 2.0, 1.2, lh), pal.fg);
            }
        }
        let cr = Rect::new(text_r.right() + 4.0, r.y, count_w, r.h);
        gfx.text_in(&count, &gfx.small, &cr, pal.faint);
        let mut bx = r.right() - buttons;
        for (icon, act) in [
            ("\u{E70E}", Act::FindPrev),
            ("\u{E70D}", Act::FindNext),
            ("\u{E711}", Act::FindClose),
        ] {
            let b = Rect::new(bx, r.y + 4.0, 26.0, 26.0);
            gfx.text_in(icon, &gfx.icon, &b, pal.dim);
            hits.push((b, Hit::Tr(act)));
            bx += 26.0;
        }
    }

    /// What an empty Folio says, in the middle of the glass.
    pub(super) unsafe fn empty(
        &self,
        gfx: &Gfx,
        pal: &Palette,
        dir: &std::path::Path,
        area: &Rect,
    ) {
        let mid = area.y + area.h / 2.0;
        let x = area.w / 2.0;
        gfx.rune(3, x, mid - 70.0, 34.0, pal.accent.with_alpha(0.55), 2.0);
        let name = dir
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
            windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL,
            false,
            true,
        ) {
            let _ = f.SetTextAlignment(
                windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT_CENTER,
            );
            centred = f;
        }
        gfx.text_in(note, &centred, &r, pal.faint);
    }
}

fn cache_units(
    blocks: &[Option<Built>],
) -> impl Fn(
    usize,
    u32,
) -> Option<(
    windows::Win32::Graphics::DirectWrite::IDWriteTextLayout,
    u32,
)> + '_ {
    move |b, u| {
        let unit = blocks.get(b)?.as_ref()?.scene.units.get(u as usize)?;
        Some((unit.layout.clone(), unit.text.encode_utf16().count() as u32))
    }
}
