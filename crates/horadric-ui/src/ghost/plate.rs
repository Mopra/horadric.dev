//! The Thinking plate, drawn as the app's other plates are: flat dark
//! metal with a seam cut round it, legends printed on it, wells sunk into
//! it. What the session does shows in runes, as on a working tile: three
//! by the title cut over and over while it works in the program, and one
//! by each shell command still running.
//!
//! The parts that move (those runes, the bar that counts a glide down,
//! the Bypass switch while it is held) each know their own place, so a
//! beat draws them again and nothing else.

use std::collections::VecDeque;
use std::time::Duration;

use windows::core::w;
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_CLIP,
};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteTextFormat, IDWriteTextLayout, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_METRICS, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER,
};
use windows_numerics::Vector2;

use super::layer::{d2rect, Area, Pen};
use super::rules::*;
use super::shells::*;
use super::Now;
use crate::render::{self, color};
use crate::theme::{self, Color};

/// The three runes by the title, the dictation plate's own.
const TITLE_RUNES: [usize; 3] = [2, 7, 10];
const TITLE_RUNE_H: f32 = 13.0;
const TITLE_RUNE_STEP: f32 = 14.0;
const TITLE_Y: f32 = 24.0;
/// How many times a second a working rune is cut anew.
const CUTS: f32 = 0.9;
const RUNE_CUT: f32 = 1.1;

/// The plate's face. One flat colour, between the top and the bottom of
/// the app's plates: a shade that runs down a window this small, laid on
/// someone else's screen, shows as a band.
pub(super) fn face() -> Color {
    theme::plate_top().mix(theme::plate_bottom(), 0.5)
}

/// The colour of what the plate says is going on: work, or bypass.
fn lamp(bypass: bool) -> Color {
    if bypass {
        theme::waiting()
    } else {
        theme::working()
    }
}

pub(super) struct PlateScene<'a> {
    pub h: f32,
    pub radius: f32,
    pub now: &'a Now,
    pub reason: Option<&'a IDWriteTextLayout>,
    pub reason_h: f32,
    /// How much of the glide is left, while one runs.
    pub gliding: Option<f32>,
    pub history: &'a VecDeque<Entry>,
    pub speed: Duration,
    pub bypass: bool,
    /// The band under the controls while bypass is on.
    pub banner: &'a str,
    pub hover: Option<Control>,
    pub dragging: bool,
    /// The human's Esc is heard, so the plate may say it stops things.
    pub esc: bool,
    /// How far the hold that turns bypass on has come.
    pub arming: Option<f32>,
    /// The runes' clock, in seconds, none when they stand still, and
    /// whether the session works in the program now.
    pub t: Option<f32>,
    pub working: bool,
}

/// Where the plate's moving parts are, for a beat to draw them alone.
#[derive(Default)]
pub(super) struct Spots {
    /// The bar that counts the glide down.
    pub bar: Option<Area>,
    /// The rune of each shell command still running, and which rune.
    pub running: Vec<(Area, usize)>,
}

/// The title's runes' place.
pub(super) fn title_area() -> Area {
    (
        PAD - 2.0,
        TITLE_Y - 10.0,
        TITLE_RUNES.len() as f32 * TITLE_RUNE_STEP + 8.0,
        20.0,
    )
}

/// The Bypass switch's place.
pub(super) fn bypass_area() -> Area {
    (
        BYPASS_X - 1.0,
        CONTROLS_Y - 1.0,
        BYPASS_W + 2.0,
        CONTROLS_H + 2.0,
    )
}

fn area_rect(a: Area) -> windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F {
    d2rect(a.0, a.1, a.2, a.3)
}

/// How much of a rune is cut `t` seconds in, `k` of a turn behind the
/// first: cut, held a moment, and cut again, as the dictation plate
/// writes.
fn cut_at(t: Option<f32>, k: f32) -> f32 {
    t.map_or(1.0, |t| ((t * CUTS - k).rem_euclid(1.0) * 1.4).min(1.0))
}

/// The three runes by the title on the plate's face. Working, they are
/// cut one after another, round and round; else they rest, dim. Without
/// a clock they stand whole.
pub(super) unsafe fn draw_title_runes(p: &Pen, t: Option<f32>, working: bool, bypass: bool) {
    p.fill(area_rect(title_area()), face());
    for (i, &rune) in TITLE_RUNES.iter().enumerate() {
        let at = (PAD + 5.0 + i as f32 * TITLE_RUNE_STEP, TITLE_Y);
        let k = i as f32 / TITLE_RUNES.len() as f32;
        let (ink, drawn) = if working {
            (lamp(bypass), cut_at(t, k))
        } else {
            (theme::text_dim().mix(lamp(bypass), 0.25).fade(0.6), 1.0)
        };
        p.rune(rune, at, (0.0, -1.0), TITLE_RUNE_H, ink, RUNE_CUT, drawn);
    }
}

/// The bar that counts a glide down, `left` of it still to go.
pub(super) unsafe fn draw_bar(p: &Pen, area: Area, left: f32) {
    p.fill(area_rect(area), face());
    let (x, y, w) = (area.0, area.1 + 1.0, area.2);
    p.fill_rounded(d2rect(x, y, w, 2.0), 1.0, theme::well());
    p.fill_rounded(
        d2rect(x, y, w * left.clamp(0.0, 1.0), 2.0),
        1.0,
        theme::waiting(),
    );
}

/// A running command's rune, cut over and over while it runs.
pub(super) unsafe fn draw_running(p: &Pen, area: Area, rune: usize, t: Option<f32>) {
    p.fill(area_rect(area), face());
    let at = (area.0 + area.2 / 2.0, area.1 + area.3 / 2.0);
    p.rune(
        rune,
        at,
        (0.0, -1.0),
        11.0,
        theme::working(),
        RUNE_CUT,
        cut_at(t, 0.0),
    );
}

/// The Thinking plate.
pub(super) unsafe fn draw_plate(gpu: &render::Gpu, p: &Pen, s: &PlateScene, spots: &mut Spots) {
    let (w, h) = (BOX_W, s.h);
    let radius = s.radius.max(6.0);
    p.fill_rounded(d2rect(0.0, 0.0, w, h), radius, face());
    // No window border here to part it from what is behind: a hairline,
    // in the warning colour while bypass is on.
    let edge = if s.bypass {
        theme::waiting().with_alpha(0.85)
    } else {
        theme::text_dim().with_alpha(0.22)
    };
    p.stroke_rounded(d2rect(0.5, 0.5, w - 1.0, h - 1.0), radius, edge, 1.0);
    // The seam every plate of the app has: a groove, lit under its edge.
    let seam_r = (radius - 3.0).max(2.0);
    p.stroke_rounded(
        d2rect(5.5, 6.5, w - 11.0, h - 11.0),
        seam_r,
        theme::engrave_light(),
        1.0,
    );
    p.stroke_rounded(
        d2rect(5.5, 5.5, w - 11.0, h - 11.0),
        seam_r,
        theme::engrave_dark(),
        1.0,
    );

    let x = PAD;
    let cw = w - 2.0 * PAD;
    draw_title_runes(p, s.t, s.working, s.bypass);
    let title_x = x + TITLE_RUNES.len() as f32 * TITLE_RUNE_STEP + 8.0;
    p.text(
        &gpu.title,
        theme::text(),
        "Thinking",
        d2rect(title_x, 14.0, 80.0, 20.0),
    );
    let who = match (s.now.name.trim(), s.now.program.trim()) {
        ("", program) => program.to_string(),
        (name, "") => name.to_string(),
        (name, program) => format!("{name} in {program}"),
    };
    p.text(
        &gpu.small_right,
        theme::text_dim(),
        &who,
        d2rect(title_x + 84.0, 14.0, (x + cw - title_x - 84.0).max(0.0), 20.0),
    );

    draw_controls(gpu, p, s);
    let mut y = CONTROLS_Y + CONTROLS_STEP;
    if s.bypass {
        p.fill_rounded(d2rect(x, y - 2.0, cw, BANNER - 6.0), 4.0, theme::well());
        p.text(
            &gpu.small_bold,
            theme::waiting(),
            s.banner,
            d2rect(x + 8.0, y, cw - 16.0, ROW),
        );
        y += BANNER;
    }
    p.text(&gpu.chip, theme::legend(), "NOW", d2rect(x, y, 60.0, 16.0));
    // Only what is true: without the hooks Esc stops nothing, and the
    // plate does not say it does.
    let stop = s.gliding.is_some() && s.esc;
    // The session's id and start, since two sessions can share a name.
    let ident_w = if stop { cw - 150.0 } else { cw - 40.0 };
    p.text(
        &gpu.small,
        theme::legend(),
        s.now.ident.trim(),
        d2rect(x + 40.0, y - 1.0, ident_w, 16.0),
    );
    if stop {
        p.text(
            &gpu.small_right,
            theme::waiting(),
            "Hold Esc to stop",
            d2rect(x + 60.0, y, cw - 60.0, 16.0),
        );
    }
    if let Some(left) = s.gliding {
        let bar = (x, y + 17.0, cw, 4.0);
        draw_bar(p, bar, left);
        spots.bar = Some(bar);
    }
    y += 22.0;
    if let Some(reason) = s.reason {
        let shown = s.reason_h.min(REASON_LINES * REASON_LINE);
        p.rt.PushAxisAlignedClip(&d2rect(x, y, cw, shown), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        p.brush.SetColor(&color(if s.now.away.is_some() {
            theme::waiting()
        } else {
            theme::text()
        }));
        p.rt.DrawTextLayout(
            Vector2 { X: x, Y: y },
            reason,
            p.brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
        );
        p.rt.PopAxisAlignedClip();
        y += shown + 4.0;
    }
    let what = match (s.now.what.trim(), s.now.away.is_some()) {
        ("", false) => "looking".to_string(),
        (what, false) => what.to_string(),
        ("", true) => "Last in the program: looking".to_string(),
        (what, true) => format!("Last in the program: {what}"),
    };
    let ink = if s.now.away.is_some() {
        theme::legend()
    } else {
        theme::text_dim()
    };
    p.text(&gpu.small, ink, &what, d2rect(x, y, cw, ROW));
    y += ROW;

    if s.history.is_empty() {
        return;
    }
    y += 6.0;
    groove(p, x, cw, y);
    y += 8.0;
    p.text(
        &gpu.chip,
        theme::legend(),
        "HISTORY",
        d2rect(x, y, 80.0, 16.0),
    );
    y += 22.0;
    for (i, e) in s.history.iter().enumerate() {
        // The newest reads plainly, the older ones step back.
        let ink = if i == 0 {
            theme::text()
        } else {
            theme::text_dim()
        };
        p.text(&gpu.small, theme::legend(), &e.at, d2rect(x, y, 60.0, ROW));
        let mut buf = [0u8; 4];
        let mark = if e.unasked {
            theme::waiting()
        } else {
            e.mark.colour()
        };
        p.text(
            &gpu.icon_small,
            mark,
            e.mark.glyph().encode_utf8(&mut buf),
            d2rect(x + 58.0, y, 16.0, ROW),
        );
        p.text(
            &gpu.small,
            ink,
            &entry_line(e),
            d2rect(x + 80.0, y, cw - 80.0, ROW),
        );
        y += ROW;
    }
}

/// A straight groove across the plate, as the app's plates cut them.
unsafe fn groove(p: &Pen, x: f32, w: f32, y: f32) {
    p.fill(d2rect(x, y + 1.0, w, 1.0), theme::engrave_light());
    p.fill(d2rect(x, y, w, 1.0), theme::engrave_dark());
}

/// A command laid out once, for every paint it shows in.
pub(super) struct Laid {
    pub layout: IDWriteTextLayout,
    /// Lines of the whole command.
    pub total: usize,
    pub line_h: f32,
    /// The height it is trimmed at now.
    pub max_h: f32,
}

/// One command on the plate, laid out.
pub(super) struct ShellRow<'a> {
    pub shell: &'a Shell,
    pub command: IDWriteTextLayout,
    /// How tall its command shows.
    pub h: f32,
    /// Lines of it the plate does not show.
    pub hidden: usize,
}

/// The shell section: what the session ran in a shell while control was
/// on, under the rest of the plate.
pub(super) struct ShellsScene<'a> {
    pub top: f32,
    pub open: bool,
    pub count: String,
    pub rows: Vec<ShellRow<'a>>,
    /// The line under the commands, when there is room for it.
    pub note: Option<String>,
    pub hover: bool,
    /// The runes' clock, in seconds, none when they stand still.
    pub t: Option<f32>,
}

/// How many lines laid out text has, and how tall one is.
pub(super) fn line_metrics(l: &IDWriteTextLayout) -> (usize, f32) {
    let mut m = DWRITE_TEXT_METRICS::default();
    match unsafe { l.GetMetrics(&mut m) } {
        Ok(()) if m.lineCount > 0 => (m.lineCount as usize, m.height / m.lineCount as f32),
        _ => (1, REASON_LINE),
    }
}

/// The commands' face: monospaced, as a terminal shows them, trimmed with
/// an ellipsis where the plate stops showing them.
pub(super) unsafe fn mono_format(gpu: &render::Gpu) -> Option<IDWriteTextFormat> {
    let f = gpu
        .dw
        .CreateTextFormat(
            w!("Consolas"),
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            12.0,
            w!("en-us"),
        )
        .ok()?;
    let trimming = DWRITE_TRIMMING {
        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
        delimiter: 0,
        delimiterCount: 0,
    };
    if let Ok(sign) = gpu.dw.CreateEllipsisTrimmingSign(&f) {
        let _ = f.SetTrimming(&trimming, &sign);
    }
    Some(f)
}

/// The shell section: a header that folds it, and each command with when
/// it began, how it stands, what it is for, and the command itself.
pub(super) unsafe fn draw_shells(
    gpu: &render::Gpu,
    p: &Pen,
    s: &ShellsScene,
    spots: &mut Spots,
) {
    let x = PAD;
    let cw = BOX_W - 2.0 * PAD;
    groove(p, x, cw, s.top + 6.0);
    let mut y = s.top + SHELLS_GAP;
    let ink = if s.hover {
        theme::text()
    } else {
        theme::legend()
    };
    p.text(
        &gpu.chip,
        ink,
        "SHELL COMMANDS",
        d2rect(x, y, 140.0, FOLD_H),
    );
    p.text(
        &gpu.small_right,
        if s.hover {
            theme::text()
        } else {
            theme::text_dim()
        },
        &s.count,
        d2rect(x + 140.0, y, cw - 140.0 - 24.0, FOLD_H),
    );
    let mut buf = [0u8; 4];
    let chevron = if s.open { OPEN } else { FOLDED };
    p.text(
        &gpu.icon_small,
        ink,
        chevron.encode_utf8(&mut buf),
        d2rect(x + cw - 16.0, y, 16.0, FOLD_H),
    );
    y += FOLD_H;
    if !s.open {
        return;
    }
    let width = |text: &str, fmt: &IDWriteTextFormat| {
        let wide: Vec<u16> = text.encode_utf16().collect();
        gpu.dw
            .CreateTextLayout(&wide, fmt, cw, ROW)
            .map_or(80.0, |l| render::text_size(&l).0.ceil())
    };
    for row in &s.rows {
        let shell = row.shell;
        let running = shell.state == ShellState::Running;
        p.text(
            &gpu.small,
            theme::legend(),
            &shell.at,
            d2rect(x, y, 60.0, ROW),
        );
        if running {
            // What runs shows as a rune being cut, as a working tile's.
            let spot = (x + 57.0, y + 1.0, 16.0, ROW - 2.0);
            let rune = shell.serial as usize;
            draw_running(p, spot, rune, s.t);
            spots.running.push((spot, rune));
        } else {
            let mut buf = [0u8; 4];
            p.text(
                &gpu.icon_small,
                shell.state.colour(),
                shell.state.glyph().encode_utf8(&mut buf),
                d2rect(x + 58.0, y, 16.0, ROW),
            );
        }
        let mut left = x + 80.0;
        if shell.agent {
            p.fill_rounded(d2rect(left, y + 2.0, 42.0, ROW - 4.0), 4.0, theme::well());
            p.text(
                &gpu.chip,
                theme::legend(),
                "agent",
                d2rect(left + 6.0, y, 36.0, ROW),
            );
            left += 48.0;
        }
        let word = shell.state.word();
        let mut right = cw - width(&word, &gpu.small);
        p.text(
            &gpu.small_right,
            shell.state.colour(),
            &word,
            d2rect(x, y, cw, ROW),
        );
        // A cut at a line's end shows no ellipsis, so the rest is counted,
        // up here where it covers none of the command.
        if let Some(more) = more_lines(row.hidden) {
            let w = width(&more, &gpu.chip);
            right -= w + 8.0;
            p.text(
                &gpu.chip,
                theme::waiting(),
                &more,
                d2rect(x + right, y, w + 2.0, ROW),
            );
        }
        p.text(
            &gpu.small,
            if running {
                theme::text()
            } else {
                theme::text_dim()
            },
            &shell.title(),
            d2rect(left, y, (x + right - 10.0 - left).max(0.0), ROW),
        );
        y += ROW;
        let well = d2rect(x, y, cw, row.h + 2.0 * WELL_PAD);
        p.fill_rounded(well, 4.0, theme::well());
        if running {
            p.stroke_rounded(
                d2rect(x + 0.5, y + 0.5, cw - 1.0, row.h + 2.0 * WELL_PAD - 1.0),
                4.0,
                theme::working().with_alpha(0.45),
                1.0,
            );
        }
        p.rt.PushAxisAlignedClip(
            &d2rect(x, y + WELL_PAD, cw, row.h),
            D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
        );
        p.brush.SetColor(&color(if running {
            theme::text()
        } else {
            theme::text_dim()
        }));
        p.rt.DrawTextLayout(
            Vector2 {
                X: x + 6.0,
                Y: y + WELL_PAD,
            },
            &row.command,
            p.brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
        );
        p.rt.PopAxisAlignedClip();
        y += row.h + 2.0 * WELL_PAD + SHELL_GAP;
    }
    if let Some(note) = &s.note {
        p.text(&gpu.small, theme::legend(), note, d2rect(x, y, cw, ROW));
    }
}

/// The control row: the glide's speed on a slider drawn like the glide's
/// own bar, and the Bypass switch.
unsafe fn draw_controls(gpu: &render::Gpu, p: &Pen, s: &PlateScene) {
    let y = CONTROLS_Y;
    let mid = y + CONTROLS_H / 2.0;
    p.text(
        &gpu.chip,
        theme::legend(),
        "SPEED",
        d2rect(PAD, mid - 8.0, 48.0, 16.0),
    );
    let at = slider_of_glide(s.speed);
    let live = s.dragging || s.hover == Some(Control::Speed);
    p.fill_rounded(
        d2rect(SLIDER_X, mid - 1.5, SLIDER_W, 3.0),
        1.5,
        theme::well(),
    );
    p.fill_rounded(
        d2rect(SLIDER_X, mid - 1.5, SLIDER_W * at, 3.0),
        1.5,
        theme::working(),
    );
    // A key's cap on the track: lit along its edge under the mouse.
    let knob = (SLIDER_X + SLIDER_W * at, mid);
    p.circle(knob, 5.5, theme::surface(), None);
    let rim = if live {
        theme::text()
    } else {
        theme::working()
    };
    p.circle(knob, 5.5, rim, Some(1.3));
    p.text(
        &gpu.small,
        if live {
            theme::text()
        } else {
            theme::text_dim()
        },
        &speed_label(s.speed),
        d2rect(SLIDER_X + SLIDER_W + 12.0, mid - 9.0, 50.0, ROW),
    );
    draw_bypass(gpu, p, s.bypass, s.hover == Some(Control::Bypass), s.arming, s.esc);
}

/// The Bypass switch. Off, it is a well that fills while the mouse is
/// held on it; on, it is lit in the warning colour. Without the hooks
/// that tell a human's press from a posted one it can not be turned on,
/// and is printed as a legend, not as a key.
pub(super) unsafe fn draw_bypass(
    gpu: &render::Gpu,
    p: &Pen,
    on: bool,
    hover: bool,
    arming: Option<f32>,
    can: bool,
) {
    p.fill(area_rect(bypass_area()), face());
    let y = CONTROLS_Y;
    let button = d2rect(BYPASS_X, y, BYPASS_W, CONTROLS_H);
    if on {
        p.fill_rounded(button, 5.0, theme::waiting());
        p.text(&gpu.small_centre, Color::rgb(0x0B0C0E), "Bypass on", button);
        return;
    }
    p.fill_rounded(button, 5.0, theme::well());
    if let Some(f) = arming {
        p.rt.PushAxisAlignedClip(
            &d2rect(BYPASS_X, y, BYPASS_W * f.clamp(0.0, 1.0), CONTROLS_H),
            D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
        );
        p.fill_rounded(button, 5.0, theme::waiting().with_alpha(0.4));
        p.rt.PopAxisAlignedClip();
    }
    let edge = if !can {
        theme::engrave_dark()
    } else if hover || arming.is_some() {
        theme::waiting()
    } else {
        theme::engrave_light()
    };
    p.stroke_rounded(
        d2rect(BYPASS_X + 0.5, y + 0.5, BYPASS_W - 1.0, CONTROLS_H - 1.0),
        5.0,
        edge,
        1.0,
    );
    let (ink, label) = match (can, arming.is_some(), hover) {
        (false, _, _) => (theme::legend(), "Bypass"),
        (true, true, _) => (theme::text(), "Keep holding"),
        (true, false, true) => (theme::text(), "Hold for Bypass"),
        (true, false, false) => (theme::text_dim(), "Bypass"),
    };
    p.text(&gpu.small_centre, ink, label, button);
}
