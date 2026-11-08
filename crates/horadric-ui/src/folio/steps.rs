//! A run of tool steps in the transcript: each step's line, and opened,
//! what it did: a file's lines, a search's hits, a command and what it
//! printed, the pages a search found.

use std::path::Path;

use horadric_core::folio::{
    GlobResult, GrepLine, GrepResult, Lane, ReadResult, ResultKind, ShellResult, Step, StepKind,
    StepState, ToolResult, WebSearchResult,
};
use windows::Win32::Graphics::DirectWrite::{IDWriteTextLayout, DWRITE_TEXT_ALIGNMENT_TRAILING};

use super::flow;
use super::rich::{self, colour_code, Ctx, Item, Scene, F, MONO_LINE};
use super::transcript::*;
use super::view::Hit;
use crate::folio_look::{self as look, RunState, Tone};
use crate::layout::Rect;
use crate::render;
use crate::theme::{self, Color};

/// A result plate's corner, padding, header and the row that opens it,
/// as a code block's.
const RADIUS: f32 = 8.0;
const PAD: f32 = 12.0;
const HEAD: f32 = 30.0;
const FOOT: f32 = 28.0;
/// A row in a list of files or pages.
const ROW: f32 = 24.0;
/// How much shows before "Show all": a Read's head, a Grep's lines, a
/// list's rows, an MCP tool's JSON, an error's lines.
const READ_LINES: usize = 14;
const GREP_LINES: usize = 24;
const LIST_ROWS: usize = 12;
const JSON_LINES: usize = 15;
const ERROR_LINES: usize = 8;
const LINKS: usize = 8;
/// How tall fetched prose runs before it folds.
const PROSE_H: f32 = 260.0;
/// Fetched text past this is not laid out until asked for.
const PROSE_CHARS: usize = 20_000;

/// Mono rows laid out, ready to draw.
struct Mono {
    l: IDWriteTextLayout,
    text: String,
    gutter: Option<IDWriteTextLayout>,
    /// Where the text starts, from the plate's left.
    gw: f32,
    h: f32,
    ink: Color,
}

impl Ctx<'_> {
    pub(super) unsafe fn tools(
        &self,
        i: usize,
        steps: &[Step],
        open: bool,
        st: &State,
        cw: f32,
    ) -> Scene {
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
        } else if let Some((step, lane)) = (steps.iter())
            .filter_map(|s| Some((s, s.lane.as_ref().filter(|l| at_work(s, l))?)))
            .next()
        {
            // A subagent at work says what it is doing even folded.
            let r = Rect::new(NEST, y, cw - NEST, 18.0);
            self.lane_folded(&mut s, step, lane, r);
            s.hit(r, Hit::Tr(Act::Agent(i, step.id.clone())));
            y += 18.0;
        }
        s.h = y;
        s
    }

    /// One step's line at `x`, `y`, and its detail when open. Returns
    /// where the next goes.
    pub(super) unsafe fn step(
        &self,
        s: &mut Scene,
        step: &Step,
        x: f32,
        y: f32,
        cw: f32,
        st: &State,
    ) -> f32 {
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
        let line = look::step_line(step);
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
        if !line.meta.is_empty() {
            let mw = self.width(&self.fonts.right, &line.meta) + 2.0;
            let r = Rect::new(x + right - mw, y, mw, STEP_H);
            s.line(line.meta.clone(), F::Right, r, self.pal.faint);
            right -= mw + 10.0;
        }
        for (say, tone) in line.pills.iter().rev() {
            right = self.state_pill(s, say, *tone, x + right, y + STEP_H / 2.0) - x - 6.0;
        }
        let mut lx = x + 20.0;
        if let Some(tag) = &line.tag {
            lx = self.server_tag(s, tag, lx, y + STEP_H / 2.0) + 7.0;
        }
        let mut label = line.label;
        if let Some(at) = step.began.filter(|_| st.verbose) {
            label = format!("{}  {label}", clock(at));
        }
        let ink = if step.state == StepState::Failed && !line.pills.is_empty() {
            self.pal.fg.with_alpha(0.72)
        } else {
            self.pal.dim
        };
        s.line(
            label,
            F::Small,
            Rect::new(lx, y, (x + right - lx).max(0.0), STEP_H),
            ink,
        );
        if detail {
            s.hit(row, Hit::Tr(Act::Step(step.id.clone())));
        }
        let mut y = y + STEP_H;
        if !open {
            if let Some(lane) = step.lane.as_ref().filter(|l| at_work(step, l)) {
                self.lane_folded(s, step, lane, Rect::new(x + NEST, y - 3.0, w - NEST, 16.0));
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
        } else if let Some(r) = &step.result {
            y = self.result(s, step, r, dx, y + 2.0, dw, st);
        } else {
            y = self.output(s, step, dx, y, dw, st);
        }
        if !step.images.is_empty() {
            y = self.step_images(s, step, dx, y + 6.0, dw);
        }
        y + 6.0
    }

    /// A subagent at work folded to one line in `r`: the rune its open
    /// lane turns while it runs, then its type, model and progress.
    fn lane_folded(&self, s: &mut Scene, step: &Step, lane: &Lane, r: Rect) {
        let ink = self.pal.accent.mix(self.pal.fg, 0.35);
        // Only shown while the agent works, so it turns on the frame clock too.
        let rune = lane.tool_uses.max(lane.steps.len() as u64) as usize;
        let h = 11.0;
        (s.items).push(Item::Rune(
            LIVE | rune,
            r.x + 5.0,
            r.y + (r.h - h) / 2.0,
            h,
            ink,
            1.3,
        ));
        let model = step.input["model"].as_str().unwrap_or_default();
        let line = look::with_model(flow::lane_line(lane, true), lane, model);
        let text = Rect::new(r.x + 16.0, r.y, (r.w - 16.0).max(0.0), r.h);
        s.line(line, F::Small, text, self.pal.faint);
    }

    /// The images a step's result carried, as thumbnails in rows.
    pub(super) unsafe fn step_images(
        &self,
        s: &mut Scene,
        step: &Step,
        x: f32,
        y: f32,
        w: f32,
    ) -> f32 {
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

    /// A step's result drawn for what it is. Returns where the next goes.
    #[allow(clippy::too_many_arguments)]
    unsafe fn result(
        &self,
        s: &mut Scene,
        step: &Step,
        r: &ToolResult,
        x: f32,
        y: f32,
        w: f32,
        st: &State,
    ) -> f32 {
        let all = st.all.contains(&step.id);
        let id = step.id.as_str();
        // A shell's error is its output, which it shows as such.
        if r.error && !matches!(r.kind, ResultKind::Shell(_)) {
            return self.error_box(s, id, &r.text, x, y, w, all);
        }
        match &r.kind {
            ResultKind::Read(read) => self.read_lines(s, id, read, x, y, w, all),
            ResultKind::Grep(g) => {
                let searched = step.input["path"].as_str().unwrap_or_default();
                self.grep_hits(s, id, g, searched, &r.text, x, y, w, all)
            }
            ResultKind::Glob(g) => self.glob_files(s, id, g, x, y, w, all),
            ResultKind::Shell(sh) => self.shell_run(s, step, r, sh, x, y, w, all),
            ResultKind::WebSearch(ws) => self.web_links(s, id, ws, x, y, w, all),
            ResultKind::Mcp(m) => match &m.json {
                Some(json) => self.mono_plate(s, id, json, "json", x, y, w, all, JSON_LINES),
                None => self.prose(s, id, &r.text, x, y, w, all),
            },
            ResultKind::Plain if step.tool == "WebFetch" => {
                self.prose(s, id, &r.text, x, y, w, all)
            }
            ResultKind::Plain => self.mono_plate(s, id, &r.text, "", x, y, w, all, OUTPUT_LINES),
        }
    }

    /// A Read: the file and range in the header, the lines it gave
    /// numbered from where they start and coloured for the file's kind.
    #[allow(clippy::too_many_arguments)]
    unsafe fn read_lines(
        &self,
        s: &mut Scene,
        id: &str,
        r: &ReadResult,
        x: f32,
        y: f32,
        w: f32,
        all: bool,
    ) -> f32 {
        let n = r.lines.len();
        let shown = if all { n } else { n.min(READ_LINES) };
        let at = s.items.len();
        // The header: the file, which opens it there, and the range.
        let mut range = r.range_label();
        if let Some(total) = r.total.filter(|&t| t > n && n > 0) {
            range.push_str(&format!(" of {total}"));
        }
        let rw = self.width(&self.fonts.right, &range) + 2.0;
        s.line(
            range,
            F::Right,
            Rect::new(x + w - PAD - rw, y, rw, HEAD),
            self.pal.faint,
        );
        let file = Rect::new(x + PAD, y, (w - 3.0 * PAD - rw).max(0.0), HEAD);
        s.line(
            self.short(&r.path),
            F::Small,
            file,
            self.pal.fg.with_alpha(0.78),
        );
        s.hit(
            file,
            Hit::Tr(Act::Path(r.path.clone(), Some(r.start as u32))),
        );
        let mut cy = y + HEAD;
        if shown > 0 {
            let nums: Vec<String> = (r.start..r.start + shown).map(|k| k.to_string()).collect();
            let ext = Path::new(&r.path)
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_default();
            let text = r.lines[..shown].join("\n");
            if let Some(m) = self.mono(
                text,
                &ext,
                Some(&nums),
                &[],
                w,
                self.pal.fg.with_alpha(0.88),
            ) {
                self.draw_mono(s, &m, x, cy + 8.0, w);
                cy += m.h + 18.0;
            }
        }
        if n > READ_LINES {
            let say = match (all, r.cut) {
                (true, true) => "Show less \u{b7} the rest was too long to keep".to_string(),
                (true, false) => "Show less".to_string(),
                (false, _) => format!("{} more lines", n - shown),
            };
            cy = self.foot(s, id, say, all, x, cy, w);
        }
        self.plate_at(s, at, Rect::new(x, y, w, cy - y), HEAD);
        cy + 2.0
    }

    /// A Grep: the files it found with their counts, or in content mode
    /// its lines grouped by file, context dim and gaps marked.
    #[allow(clippy::too_many_arguments)]
    unsafe fn grep_hits(
        &self,
        s: &mut Scene,
        id: &str,
        g: &GrepResult,
        searched: &str,
        text: &str,
        x: f32,
        y: f32,
        w: f32,
        all: bool,
    ) -> f32 {
        if g.lines.is_empty() {
            let files = g.files.iter().map(|f| (f.path.as_str(), f.count, None));
            let more = g.limited.then_some("The search was cut short at its limit");
            return self.path_list(s, id, files, g.files.len(), more, text, x, y, w, all);
        }
        let at = s.items.len();
        // Groups of lines by file, in the order they came.
        let mut groups: Vec<(&str, Vec<&GrepLine>)> = Vec::new();
        let mut left = if all { usize::MAX } else { GREP_LINES };
        for l in &g.lines {
            if left == 0 {
                break;
            }
            let file = if l.file.is_empty() {
                searched
            } else {
                l.file.as_str()
            };
            match groups.last_mut() {
                Some((f, list)) if *f == file => list.push(l),
                _ => groups.push((file, vec![l])),
            }
            left -= 1;
        }
        let mut cy = y + 4.0;
        for (k, (file, lines)) in groups.iter().enumerate() {
            if k > 0 {
                s.fill(Rect::new(x, cy, w, 1.0), 0.0, self.pal.fg.with_alpha(0.06));
            }
            if !file.is_empty() {
                let count = (g.files.iter())
                    .find(|f| f.path == *file)
                    .and_then(|f| f.count)
                    .or_else(|| Some(lines.iter().filter(|l| !l.context && !l.gap).count()));
                let first = lines.iter().find_map(|l| l.line).map(|n| n as u32);
                self.path_row(s, file, count, first, x + PAD, cy, w - 2.0 * PAD);
                cy += ROW;
            }
            let body: Vec<&str> = lines
                .iter()
                .map(|l| if l.gap { "" } else { l.text.as_str() })
                .collect();
            let nums: Vec<String> = (lines.iter())
                .map(|l| {
                    l.line
                        .filter(|_| !l.gap)
                        .map_or(String::new(), |n| n.to_string())
                })
                .collect();
            let dim = self.pal.fg.with_alpha(0.4);
            let tints: Vec<(usize, Color)> = (lines.iter().enumerate())
                .filter(|(_, l)| l.context)
                .map(|(k, _)| (k, dim))
                .collect();
            let ext = Path::new(file)
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_default();
            let ink = self.pal.fg.with_alpha(0.88);
            let Some(m) = self.mono(body.join("\n"), &ext, Some(&nums), &tints, w, ink) else {
                continue;
            };
            let top = cy + 2.0;
            self.draw_mono(s, &m, x, top, w);
            for (k, l) in lines.iter().enumerate() {
                let ry = top + k as f32 * MONO_LINE;
                if l.gap {
                    // A short rule where the lines between were skipped.
                    let r = Rect::new(x + m.gw, ry + MONO_LINE / 2.0, 28.0, 1.0);
                    s.fill(r, 0.0, self.pal.faint.with_alpha(0.5));
                } else if let Some(n) = l.line.filter(|_| !file.is_empty()) {
                    let r = Rect::new(x, ry, m.gw - 6.0, MONO_LINE);
                    s.hit(r, Hit::Tr(Act::Path(file.to_string(), Some(n as u32))));
                }
            }
            cy = top + m.h + 8.0;
        }
        let n = g.lines.len();
        if n > GREP_LINES {
            let say = if all {
                "Show less".to_string()
            } else {
                format!("Show all {n} lines")
            };
            cy = self.foot(s, id, say, all, x, cy, w);
        }
        self.plate_at(s, at, Rect::new(x, y, w, cy - y), 0.0);
        cy + 2.0
    }

    /// A Glob: the files it found, short where they are in the project.
    #[allow(clippy::too_many_arguments)]
    unsafe fn glob_files(
        &self,
        s: &mut Scene,
        id: &str,
        g: &GlobResult,
        x: f32,
        y: f32,
        w: f32,
        all: bool,
    ) -> f32 {
        let files = g.files.iter().map(|f| (f.as_str(), None, None));
        let note = (g.truncated && g.total > g.files.len())
            .then(|| format!("The first {} of {}", g.files.len(), g.total));
        let note = note.or_else(|| {
            g.truncated
                .then(|| "Only the first files were listed".to_string())
        });
        self.path_list(
            s,
            id,
            files,
            g.files.len(),
            note.as_deref(),
            "No files",
            x,
            y,
            w,
            all,
        )
    }

    /// Paths as rows on a plate, each opening its file, with a count
    /// beside it where there is one. Folds past [`LIST_ROWS`].
    #[allow(clippy::too_many_arguments)]
    unsafe fn path_list<'a>(
        &self,
        s: &mut Scene,
        id: &str,
        files: impl Iterator<Item = (&'a str, Option<usize>, Option<u32>)>,
        n: usize,
        note: Option<&str>,
        empty: &str,
        x: f32,
        y: f32,
        w: f32,
        all: bool,
    ) -> f32 {
        if n == 0 {
            let say = if empty.trim().is_empty() {
                "Nothing found"
            } else {
                empty.trim()
            };
            s.line(
                say.to_string(),
                F::Small,
                Rect::new(x, y, w, ROW),
                self.pal.faint,
            );
            return y + ROW;
        }
        let at = s.items.len();
        let mut cy = y + 5.0;
        for (path, count, line) in files.take(if all { n } else { LIST_ROWS }) {
            self.path_row(s, path, count, line, x + PAD, cy, w - 2.0 * PAD);
            cy += ROW;
        }
        if let Some(note) = note {
            let r = Rect::new(x + PAD, cy, w - 2.0 * PAD, ROW);
            s.line(note.to_string(), F::Small, r, self.pal.faint);
            cy += ROW;
        }
        cy += 5.0;
        if n > LIST_ROWS {
            let say = if all {
                "Show less".to_string()
            } else {
                format!("{} more", n - LIST_ROWS)
            };
            cy = self.foot(s, id, say, all, x, cy, w);
        }
        self.plate_at(s, at, Rect::new(x, y, w, cy - y), 0.0);
        cy + 2.0
    }

    /// A command: what ran, with its prompt, in a header, and under it
    /// the end of what it printed, its errors in the error ink.
    #[allow(clippy::too_many_arguments)]
    unsafe fn shell_run(
        &self,
        s: &mut Scene,
        step: &Step,
        tr: &ToolResult,
        r: &ShellResult,
        x: f32,
        y: f32,
        w: f32,
        all: bool,
    ) -> f32 {
        let at = s.items.len();
        let ps = step.tool == "PowerShell";
        let command = match r.command.trim_end() {
            "" => step.input["command"]
                .as_str()
                .unwrap_or_default()
                .trim_end(),
            c => c,
        };
        let mut cy = y;
        if !command.is_empty() {
            let gw = 18.0;
            let cx = x + PAD + gw;
            if let Some(l) = self.layout(&self.fonts.code, command, x + w - PAD - cx) {
                colour_code(self, &l, if ps { "powershell" } else { "bash" }, command);
                let (_, th) = render::text_size(&l);
                let top = cy + 9.0;
                if let Some(p) = self.one_line(&self.fonts.code, if ps { ">" } else { "$" }) {
                    s.text(&p, x + PAD, top, self.pal.faint.with_alpha(0.75));
                }
                s.unit(&l, cx, top, self.pal.fg.with_alpha(0.9), command);
                cy = top + th + 9.0;
            }
        }
        let head = cy - y;

        // What it printed: its output, then its errors.
        let (out, err) = (r.stdout.trim_end(), r.stderr.trim_end());
        let mut text = out.trim_start_matches('\n').to_string();
        let err_row = if text.is_empty() {
            0
        } else {
            text.split('\n').count()
        };
        if !err.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(err.trim_start_matches('\n'));
        }
        if text.is_empty() && r.interpretation.is_empty() {
            text = tr.text.trim_end().to_string();
        }
        let lines: Vec<&str> = if text.is_empty() {
            Vec::new()
        } else {
            text.split('\n').collect()
        };
        let cut = if all {
            0
        } else {
            lines.len().saturating_sub(OUTPUT_LINES)
        };
        let failed = step.state == StepState::Failed || tr.error;
        let ink = if failed && err.is_empty() {
            theme::error().mix(self.pal.fg, 0.35)
        } else {
            self.pal.fg.with_alpha(0.62)
        };
        let err_ink = theme::error().mix(self.pal.fg, 0.3);
        let tints: Vec<(usize, Color)> = if err.is_empty() {
            Vec::new()
        } else {
            (err_row.max(cut)..lines.len())
                .map(|k| (k - cut, err_ink))
                .collect()
        };
        if !lines.is_empty() {
            if lines.len() > OUTPUT_LINES {
                // The hidden part is above the tail, so its switch is too.
                let say = if all {
                    "Show the end only".to_string()
                } else {
                    format!("Show all, {cut} more lines above")
                };
                let r = Rect::new(x, cy, w, 24.0);
                s.line(
                    say,
                    F::Small,
                    Rect::new(x + PAD, cy + 2.0, w - 2.0 * PAD, 22.0),
                    self.link_ink(),
                );
                s.hit(r, Hit::Tr(Act::All(step.id.clone())));
                cy += 22.0;
            }
            let body = lines[cut..].join("\n");
            if let Some(m) = self.mono(body, "", None, &tints, w, ink) {
                self.draw_mono(s, &m, x, cy + 8.0, w);
                cy += m.h + 16.0;
            }
        }
        if !r.interpretation.is_empty() {
            // What the CLI made of a nonzero code: an answer, not a failure.
            let row = Rect::new(x + PAD, cy + 1.0, w - 2.0 * PAD, ROW);
            s.line(
                r.interpretation.trim().to_string(),
                F::Small,
                row,
                self.pal.dim,
            );
            cy += ROW + 2.0;
        }
        if cy == y {
            return y;
        }
        let lit = if cy - y > head + 0.5 { head } else { 0.0 };
        self.plate_at(s, at, Rect::new(x, y, w, cy - y), lit);
        if let Some(saved) = &r.saved_to {
            let say = format!("The whole output is in {}", self.short(saved));
            let row = Rect::new(x + 2.0, cy + 2.0, w - 2.0, 22.0);
            s.line(say, F::Small, row, self.link_ink());
            s.hit(row, Hit::Tr(Act::Path(saved.clone(), None)));
            cy += 24.0;
        }
        cy + 2.0
    }

    /// A WebSearch: the pages it found, each opening in the browser, and
    /// what it said of them when asked.
    #[allow(clippy::too_many_arguments)]
    unsafe fn web_links(
        &self,
        s: &mut Scene,
        id: &str,
        ws: &WebSearchResult,
        x: f32,
        y: f32,
        w: f32,
        all: bool,
    ) -> f32 {
        let mut cy = y;
        if !ws.links.is_empty() {
            let at = s.items.len();
            cy += 5.0;
            for link in ws.links.iter().take(if all { usize::MAX } else { LINKS }) {
                let row = Rect::new(x, cy, w, ROW);
                s.line(
                    "\u{E774}",
                    F::Icon,
                    Rect::new(x + PAD, cy, 14.0, ROW),
                    self.pal.faint.with_alpha(0.7),
                );
                let host = look::domain(&link.url);
                let title = if link.title.trim().is_empty() {
                    host.clone()
                } else {
                    link.title.trim().to_string()
                };
                let room = w - 2.0 * PAD - 24.0;
                let hw = self.width(&self.gfx.small, &host).min(room * 0.4) + 2.0;
                let tw = self.width(&self.gfx.small, &title).min(room - hw - 10.0) + 2.0;
                let tx = x + PAD + 24.0;
                s.line(title, F::Small, Rect::new(tx, cy, tw, ROW), self.link_ink());
                s.line(
                    host,
                    F::Small,
                    Rect::new(tx + tw + 8.0, cy, hw, ROW),
                    self.pal.faint,
                );
                s.hit(row, Hit::Tr(Act::Link(link.url.clone())));
                cy += ROW;
            }
            cy += 5.0;
            let more = ws.links.len().saturating_sub(LINKS);
            let said = !ws.summary.trim().is_empty();
            if more > 0 || said {
                let say = match (all, more, said) {
                    (true, _, _) => "Show less".to_string(),
                    (false, 0, _) => "Show what it said".to_string(),
                    (false, m, true) => format!("{m} more, and what it said"),
                    (false, m, false) => format!("{m} more"),
                };
                cy = self.foot(s, id, say, all, x, cy, w);
            }
            self.plate_at(s, at, Rect::new(x, y, w, cy - y), 0.0);
            cy += 2.0;
        }
        if (all || ws.links.is_empty()) && !ws.summary.trim().is_empty() {
            cy = self.markdown(s, &ws.summary, x, cy + 6.0, w, true).0;
        }
        cy
    }

    /// Words a tool fetched or a server sent: as a reply is set when they
    /// are markdown, else as plain text, folded when long.
    #[allow(clippy::too_many_arguments)]
    unsafe fn prose(
        &self,
        s: &mut Scene,
        id: &str,
        text: &str,
        x: f32,
        y: f32,
        w: f32,
        all: bool,
    ) -> f32 {
        let text = text.trim();
        if text.is_empty() {
            return y;
        }
        let (mut cy, more) = if look::looks_like_markdown(text) {
            self.markdown(s, text, x, y + 2.0, w, all)
        } else {
            let cut = if all {
                text
            } else {
                head_chars(text, PROSE_CHARS)
            };
            let Some(l) = self.layout(&self.fonts.small_wrap, cut, w - 14.0) else {
                return y;
            };
            let (_, th) = render::text_size(&l);
            let shown = if all { th } else { th.min(PROSE_H) };
            s.items.push(Item::Clip(Rect::new(x, y, w, shown)));
            s.unit(&l, x + 14.0, y + 2.0, self.pal.fg.with_alpha(0.7), cut);
            s.items.push(Item::Unclip);
            s.fill(
                Rect::new(x + 2.0, y + 2.0, 2.0, shown - 2.0),
                1.0,
                self.pal.fg.with_alpha(0.1),
            );
            (y + shown + 4.0, th > PROSE_H || cut.len() < text.len())
        };
        if more || all && text.len() > PROSE_CHARS {
            let say = if all { "Show less" } else { "Show all" };
            let r = Rect::new(x, cy, w, 24.0);
            s.line(
                say,
                F::Small,
                Rect::new(x + 14.0, cy, w - 14.0, 24.0),
                self.link_ink(),
            );
            s.hit(r, Hit::Tr(Act::All(id.to_string())));
            cy += 24.0;
        }
        cy
    }

    /// Markdown set as a reply's paragraphs, a rule at its left so it
    /// reads as quoted. Stops past [`PROSE_H`] unless `all`. Returns
    /// where it ends and whether it stopped short.
    unsafe fn markdown(
        &self,
        s: &mut Scene,
        text: &str,
        x: f32,
        y: f32,
        w: f32,
        all: bool,
    ) -> (f32, bool) {
        let src = if all {
            text
        } else {
            head_chars(text, PROSE_CHARS)
        };
        let mut cy = y;
        let mut short = src.len() < text.len();
        for (k, p) in flow::paras(src).into_iter().enumerate() {
            if !all && cy - y > PROSE_H {
                short = true;
                break;
            }
            let ps = self.para(&p, w - 14.0);
            if k > 0 {
                cy += p.gap;
            }
            s.append(&ps, x + 14.0, cy);
            cy += ps.h;
        }
        s.fill(
            Rect::new(x + 2.0, y, 2.0, cy - y),
            1.0,
            self.pal.fg.with_alpha(0.1),
        );
        (cy + 6.0, short)
    }

    /// Text in mono on a plate, its head shown and the rest folded past
    /// `max` lines.
    #[allow(clippy::too_many_arguments)]
    unsafe fn mono_plate(
        &self,
        s: &mut Scene,
        id: &str,
        text: &str,
        lang: &str,
        x: f32,
        y: f32,
        w: f32,
        all: bool,
        max: usize,
    ) -> f32 {
        let text = text.trim_end().trim_start_matches('\n');
        if text.is_empty() {
            return y;
        }
        let lines: Vec<&str> = text.split('\n').collect();
        let n = lines.len();
        let shown = if all { n } else { n.min(max) };
        let at = s.items.len();
        let mut cy = y;
        if let Some(m) = self.mono(
            lines[..shown].join("\n"),
            lang,
            None,
            &[],
            w,
            self.pal.fg.with_alpha(0.78),
        ) {
            self.draw_mono(s, &m, x, cy + 8.0, w);
            cy += m.h + 16.0;
        }
        if n > max {
            let say = if all {
                "Show less".to_string()
            } else {
                format!("Show all {n} lines")
            };
            cy = self.foot(s, id, say, all, x, cy, w);
        }
        self.plate_at(s, at, Rect::new(x, y, w, cy - y), 0.0);
        cy + 2.0
    }

    /// A tool's error: its message in the error ink on a plate tinted to
    /// match, nothing of the tags it came in.
    #[allow(clippy::too_many_arguments)]
    unsafe fn error_box(
        &self,
        s: &mut Scene,
        id: &str,
        text: &str,
        x: f32,
        y: f32,
        w: f32,
        all: bool,
    ) -> f32 {
        let text = text.trim();
        let text = if text.is_empty() {
            "It failed without saying why"
        } else {
            text
        };
        let lines: Vec<&str> = text.split('\n').collect();
        let n = lines.len();
        let shown = if all {
            text.to_string()
        } else {
            lines[..n.min(ERROR_LINES)].join("\n")
        };
        let Some(l) = self.layout(&self.fonts.small_wrap, &shown, w - 2.0 * PAD - 24.0) else {
            return y;
        };
        let (_, th) = render::text_size(&l);
        let mut h = th + 18.0;
        let fold = n > ERROR_LINES;
        if fold {
            h += 22.0;
        }
        let r = Rect::new(x, y, w, h);
        let red = theme::error();
        s.fill(r, RADIUS, red.with_alpha(0.07));
        s.stroke(r, RADIUS, red.with_alpha(0.22), 1.0);
        s.line(
            "\u{E783}",
            F::Icon,
            Rect::new(x + PAD, y + 9.0, 16.0, 20.0),
            red.with_alpha(0.85),
        );
        s.unit(
            &l,
            x + PAD + 24.0,
            y + 9.0,
            red.mix(self.pal.fg, 0.45),
            &shown,
        );
        if fold {
            let say = if all {
                "Show less".to_string()
            } else {
                format!("Show all {n} lines")
            };
            let row = Rect::new(x, y + th + 14.0, w, 22.0);
            s.line(
                say,
                F::Small,
                Rect::new(x + PAD + 24.0, row.y, w / 2.0, 22.0),
                self.link_ink(),
            );
            s.hit(row, Hit::Tr(Act::All(id.to_string())));
        }
        y + h + 2.0
    }

    /// A shell step's command and the end of its output, or any step's
    /// output, in mono on a dark plate: what a step without a read result
    /// shows.
    pub(super) unsafe fn output(
        &self,
        s: &mut Scene,
        step: &Step,
        x: f32,
        y: f32,
        w: f32,
        st: &State,
    ) -> f32 {
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
        s.fill(plate, RADIUS, Color::rgb(0).with_alpha(0.3));
        s.items.push(rich::Item::Clip(plate));
        s.unit(&l, x + 10.0, top + 8.0, self.pal.fg.with_alpha(0.9), &text);
        s.items.push(rich::Item::Unclip);
        plate.bottom()
    }

    /// Lines laid out one to a row in mono, coloured as `lang` when it
    /// names one, the rows in `tints` in their own ink, with a number or
    /// mark per row beside them when `gutter` is given.
    unsafe fn mono(
        &self,
        text: String,
        lang: &str,
        gutter: Option<&[String]>,
        tints: &[(usize, Color)],
        w: f32,
        ink: Color,
    ) -> Option<Mono> {
        let gw = match gutter {
            Some(g) => {
                let chars = g
                    .iter()
                    .map(|n| n.chars().count())
                    .max()
                    .unwrap_or(1)
                    .max(2);
                chars as f32 * self.width(&self.fonts.rows, "0") + PAD + 10.0
            }
            None => PAD,
        };
        let l = self.layout(&self.fonts.rows, &text, (w - gw - PAD).max(40.0))?;
        if !lang.is_empty() {
            colour_code(self, &l, lang, &text);
        }
        if !tints.is_empty() {
            let starts = row_starts(&text);
            for &(row, c) in tints {
                if let Some(&(a, n)) = starts.get(row) {
                    self.paint(&l, a..a + n, c);
                }
            }
        }
        let gutter = gutter.and_then(|g| {
            let gl = self.layout(&self.fonts.rows, &g.join("\n"), gw - 10.0)?;
            let _ = gl.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING);
            Some(gl)
        });
        let (_, h) = render::text_size(&l);
        Some(Mono {
            l,
            text,
            gutter,
            gw,
            h,
            ink,
        })
    }

    /// Mono rows at (`x`, `y`) on a plate `w` wide, clipped to it since
    /// a row never wraps.
    unsafe fn draw_mono(&self, s: &mut Scene, m: &Mono, x: f32, y: f32, w: f32) {
        s.items
            .push(Item::Clip(Rect::new(x + 1.0, y, w - 2.0, m.h)));
        if let Some(g) = &m.gutter {
            s.text(g, x, y, self.pal.faint.with_alpha(0.7));
        }
        s.unit(&m.l, x + m.gw, y, m.ink, &m.text);
        s.items.push(Item::Unclip);
    }

    /// A result's plate under what was drawn since `at`: dark, a hairline
    /// round it, its top `head` a shade lighter as a code block's header.
    fn plate_at(&self, s: &mut Scene, at: usize, r: Rect, head: f32) {
        let mut under = vec![Item::Fill(r, RADIUS, Color::rgb(0).with_alpha(0.32))];
        if head > 0.0 {
            under.push(Item::Clip(Rect::new(r.x, r.y, r.w, head)));
            under.push(Item::Fill(r, RADIUS, self.pal.fg.with_alpha(0.035)));
            under.push(Item::Unclip);
            let rule = Rect::new(r.x, r.y + head - 1.0, r.w, 1.0);
            under.push(Item::Fill(rule, 0.0, self.pal.fg.with_alpha(0.06)));
        }
        under.push(Item::Stroke(r, RADIUS, self.pal.fg.with_alpha(0.07), 1.0));
        let at = at.min(s.items.len());
        s.items.splice(at..at, under);
    }

    /// The row along a plate's foot that shows all of it or less.
    #[allow(clippy::too_many_arguments)]
    fn foot(
        &self,
        s: &mut Scene,
        id: &str,
        say: String,
        open: bool,
        x: f32,
        y: f32,
        w: f32,
    ) -> f32 {
        s.fill(Rect::new(x, y, w, 1.0), 0.0, self.pal.fg.with_alpha(0.06));
        let ink = self.link_ink();
        let icon = if open { "\u{E70E}" } else { "\u{E70D}" };
        s.line(icon, F::Icon, Rect::new(x + PAD, y, 16.0, FOOT), ink);
        s.line(
            say,
            F::Small,
            Rect::new(x + PAD + 22.0, y, w - 2.0 * PAD - 22.0, FOOT),
            ink,
        );
        s.hit(Rect::new(x, y, w, FOOT), Hit::Tr(Act::All(id.to_string())));
        y + FOOT
    }

    /// A file as a row: its folder faint, its name in the link ink, its
    /// count at the right. A click opens it, at `line` when given.
    #[allow(clippy::too_many_arguments)]
    fn path_row(
        &self,
        s: &mut Scene,
        path: &str,
        count: Option<usize>,
        line: Option<u32>,
        x: f32,
        y: f32,
        w: f32,
    ) {
        let shown = self.short(path);
        let (dir, name) = match shown.rfind('/') {
            Some(i) => (&shown[..=i], &shown[i + 1..]),
            None => ("", shown.as_str()),
        };
        let mut right = x + w;
        if let Some(n) = count {
            let r = Rect::new(right - 48.0, y, 48.0, ROW);
            s.line(n.to_string(), F::Right, r, self.pal.faint.with_alpha(0.8));
            right -= 56.0;
        }
        let room = (right - x).max(0.0);
        let dw = if dir.is_empty() {
            0.0
        } else {
            (self.width(&self.gfx.small, dir) + 1.0).min(room * 0.6)
        };
        if dw > 0.0 {
            s.line(
                dir.to_string(),
                F::Small,
                Rect::new(x, y, dw, ROW),
                self.pal.faint,
            );
        }
        let r = Rect::new(x + dw, y, (room - dw).max(0.0), ROW);
        s.line(name.to_string(), F::Small, r, self.link_ink());
        s.hit(
            Rect::new(x, y, room, ROW),
            Hit::Tr(Act::Path(path.to_string(), line)),
        );
    }

    /// A state on a step's line as a small rounded mark ending at
    /// `right`, centred on `mid`. Returns its left edge.
    fn state_pill(&self, s: &mut Scene, say: &str, tone: Tone, right: f32, mid: f32) -> f32 {
        let tw = self.width(&self.fonts.centre, say);
        let r = Rect::new(right - tw - 14.0, mid - 9.0, tw + 14.0, 18.0);
        let (fill, ink) = match tone {
            Tone::Error => (
                theme::error().with_alpha(0.14),
                theme::error().mix(self.pal.fg, 0.35),
            ),
            Tone::Quiet => (self.pal.fg.with_alpha(0.06), self.pal.dim),
        };
        s.fill(r, 4.0, fill);
        s.line(say.to_string(), F::Centre, r, ink);
        r.x
    }

    /// An MCP server's name as a small tag starting at `x`, centred on
    /// `mid`. Returns its right edge.
    fn server_tag(&self, s: &mut Scene, say: &str, x: f32, mid: f32) -> f32 {
        let tw = self.width(&self.fonts.centre, say).min(160.0);
        let r = Rect::new(x, mid - 9.0, tw + 14.0, 18.0);
        s.fill(r, 4.0, self.pal.accent.with_alpha(0.12));
        s.line(
            say.to_string(),
            F::Centre,
            r,
            self.pal.accent.mix(self.pal.fg, 0.5),
        );
        r.right()
    }

    /// How wide `t` is set in `f` on one line.
    fn width(&self, f: &windows::Win32::Graphics::DirectWrite::IDWriteTextFormat, t: &str) -> f32 {
        self.one_line(f, t)
            .map_or(t.chars().count() as f32 * 7.0, |l| render::text_size(&l).0)
    }

    /// A path as the human knows it: inside the project, from its folder.
    fn short(&self, path: &str) -> String {
        flow::project_path(self.dir, path).unwrap_or_else(|| path.replace('\\', "/"))
    }
}

/// The start of `text`, at most `max` bytes and cut at a line's end.
fn head_chars(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let cut = &text[..end];
    cut.rfind('\n').map_or(cut, |n| &cut[..n])
}

/// Where each row starts in `text` in UTF-16 units, and how long it is.
fn row_starts(text: &str) -> Vec<(u32, u32)> {
    let mut at = 0u32;
    (text.split('\n'))
        .map(|l| {
            let n = l.encode_utf16().count() as u32;
            let r = (at, n);
            at += n + 1;
            r
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_start_after_each_newline_in_utf16() {
        assert_eq!(row_starts("ab\n\u{1F600}x\n"), [(0, 2), (3, 3), (7, 0)]);
    }

    #[test]
    fn a_long_text_is_cut_at_a_line() {
        assert_eq!(head_chars("one\ntwo\nthree", 9), "one\ntwo");
        assert_eq!(head_chars("short", 9), "short");
        assert_eq!(head_chars("\u{e9}\u{e9}\u{e9}", 3), "\u{e9}");
    }
}
