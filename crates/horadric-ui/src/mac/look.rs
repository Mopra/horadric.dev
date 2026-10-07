//! How a cluster looks on a Mac: the Windows faceplate, ported a call at a
//! time onto [`Painter`]. Matte metal lit from above, sessions as keys
//! standing up off it, each with a lamp that says what its session does.

use std::time::{Duration, SystemTime};

use horadric_core::diff::Diff;
use horadric_core::{format_age, Phase, Session};

use crate::anim::{Look, Stance};
use crate::layout::{self, Button, ClusterLayout, Hit, Metrics, Rect};
use crate::motion::{self, ORBIT};
use crate::theme::{self, Color};

use super::paint::{measure, Font, Painter};

/// Where the name and the lines under it start in a tile, after the icon.
const TILE_TEXT_X: f32 = 56.0;
const INNER_PAD: f32 = 14.0;
pub const TRACE_BARS: usize = 20;
const TRACE_BAR_W: f32 = 1.6;
const TRACE_GAP: f32 = 0.8;
const TRACE_H: f32 = 11.0;
const LAND_RISE: f32 = 3.0;
const MAX_SPARKS: usize = 5;
const SPARK_REACH: f32 = 7.0;
const SPARK_ORBIT: Duration = Duration::from_millis(2400);
const SHIMMER: Duration = Duration::from_millis(2600);

/// Everything one frame of a cluster needs, owned, so a view can keep it
/// and draw whenever AppKit asks.
#[derive(Clone)]
pub struct Scene {
    pub name: String,
    pub accent: Color,
    pub sessions: Vec<Session>,
    pub layout: ClusterLayout,
    pub looks: Vec<Look>,
    pub hot: Hit,
    pub pressed: Option<Hit>,
    pub collapsed: bool,
    /// The project the stage shows.
    pub on_stage: bool,
    /// The tile whose pane has the keyboard on the stage.
    pub selected: Option<usize>,
    pub now: SystemTime,
    /// Whether light that never stops may move.
    pub ambient: bool,
}

impl Scene {
    fn button(&self, which: Hit) -> Button {
        layout::button(which, self.hot, self.pressed)
    }
}

/// Draws the whole cluster.
pub fn draw(p: &Painter, m: &Metrics, s: &Scene) {
    plate(p, m, s.layout.size);
    wash(p, s);
    if s.on_stage {
        frame(p, m, s);
    }
    header(p, s);
    for (i, (r, session, look)) in tiles(s).enumerate() {
        tile(p, m, s, i, &r, session, &look);
    }
    if let Some(add) = &s.layout.add {
        add_button(p, m, add, s.button(Hit::Add), '\u{E710}');
    }
    if let Some(shell) = &s.layout.shell {
        add_button(p, m, shell, s.button(Hit::Shell), theme::SHELL_ICON);
    }
    if s.ambient {
        light(p, m, s);
    }
}

/// Each tile with where it is this frame and how it looks.
fn tiles(s: &Scene) -> impl Iterator<Item = (Rect, &Session, Look)> + '_ {
    s.layout
        .tiles
        .iter()
        .zip(&s.sessions)
        .enumerate()
        .map(|(i, (rect, session))| {
            let look = s
                .looks
                .get(i)
                .copied()
                .unwrap_or_else(|| Look::still(rect.y));
            (Rect::new(rect.x, look.y, rect.w, rect.h), session, look)
        })
}

fn icon_centre(r: &Rect) -> (f32, f32) {
    (r.x + 33.0, r.y + r.h / 2.0)
}

fn lamp_rect(r: &Rect) -> Rect {
    Rect::new(r.x + 12.0, r.y + 14.0, 4.0, r.h - 28.0)
}

/// The faceplate: matte metal, lighter at the top where the light falls,
/// with a seam cut round it inside the window's edge.
fn plate(p: &Painter, m: &Metrics, (w, h): (f32, f32)) {
    let all = Rect::new(0.0, 0.0, w, h);
    p.fill_rounded(&all, m.window_radius, theme::plate_bottom());
    p.save();
    p.masked(&all, m.window_radius, 1.0, || {
        p.fill_gradient(
            &all,
            (0.0, h),
            &[(0.0, theme::plate_top()), (1.0, theme::plate_bottom())],
        );
    });
    p.restore();
    let seam = Rect::new(5.5, 5.5, w - 11.0, h - 11.0);
    engrave(p, &seam, (m.window_radius - 3.0).max(2.0));
}

fn wash(p: &Painter, s: &Scene) {
    let (w, h) = s.layout.size;
    let depth = 64.0f32.min(h);
    p.fill_gradient(
        &Rect::new(0.0, 0.0, w, depth),
        (0.0, depth),
        &[
            (0.0, s.accent.with_alpha(0.05)),
            (1.0, s.accent.with_alpha(0.0)),
        ],
    );
}

/// The stage shows a whole project, so the project is marked: its colour
/// around everything in the window.
fn frame(p: &Painter, m: &Metrics, s: &Scene) {
    let (w, h) = s.layout.size;
    let inset = 1.0;
    let r = Rect::new(inset, inset, w - 2.0 * inset, h - 2.0 * inset);
    p.stroke_rounded(&r, m.window_radius - inset, s.accent.with_alpha(0.55), 1.5);
}

fn engrave(p: &Painter, r: &Rect, radius: f32) {
    let lit = Rect::new(r.x, r.y + 1.0, r.w, r.h);
    p.stroke_rounded(&lit, radius, theme::engrave_light(), 1.0);
    p.stroke_rounded(r, radius, theme::engrave_dark(), 1.0);
}

fn groove(p: &Painter, x0: f32, x1: f32, y: f32) {
    p.fill_rect(
        &Rect::new(x0, y + 1.0, x1 - x0, 1.0),
        theme::engrave_light(),
    );
    p.fill_rect(&Rect::new(x0, y, x1 - x0, 1.0), theme::engrave_dark());
}

fn header(p: &Painter, s: &Scene) {
    let l = &s.layout;
    let h = l.header;
    let header_button = s.button(Hit::Header);
    if let (Some(fill), _) = theme::button_look(header_button) {
        let x = h.x - 4.0;
        let r = Rect::new(x, h.y + 3.0, l.new.x - 2.0 - x, h.h - 6.0);
        p.fill_rounded(&r, 6.0, fill);
    }
    let cy = h.y + h.h / 2.0;
    let name_x = h.x + INNER_PAD;
    let name_w = measure(Font::Display, &s.name).min(h.w * 0.62);
    p.text(
        Font::Display,
        theme::text(),
        &s.name,
        Rect::new(name_x, h.y, name_w + 2.0, h.h),
    );
    if s.collapsed || header_button != Button::Idle {
        let chevron = if s.collapsed { '\u{E76C}' } else { '\u{E70D}' };
        p.icon(
            chevron,
            theme::text_dim(),
            9.0,
            Rect::new(name_x + name_w + 4.0, h.y + 1.0, 14.0, h.h),
        );
    }
    let waiting = s.sessions.iter().filter(|x| x.phase.is_waiting()).count();
    let working = s
        .sessions
        .iter()
        .filter(|x| x.phase == Phase::Working)
        .count();
    let mut right = l.new.x - 2.0;
    for (n, label, c) in [
        (waiting, "waiting", theme::waiting()),
        (working, "working", theme::working()),
    ] {
        if n == 0 {
            continue;
        }
        let text = format!("{n} {label}");
        let text_w = measure(Font::Chip, &text) + 2.0;
        let w = text_w + 12.0;
        let x = right - w;
        if x < name_x + name_w + 20.0 {
            break;
        }
        led(p, x + 3.0, cy, c);
        p.text(
            Font::Chip,
            theme::text_dim(),
            &text,
            Rect::new(x + 12.0, cy - 9.0, text_w, 18.0),
        );
        right = x - 10.0;
    }
    groove(p, h.x, h.right(), h.bottom() + 3.0);

    let b = s.button(Hit::New);
    let (_, ink) = theme::button_look(b);
    let depth = match b {
        Button::Idle => 0.35,
        Button::Hover => 0.6,
        Button::Pressed => 0.1,
    };
    let cap = l.new.inset(4.0);
    key(p, &cap, cap.h / 2.0, theme::surface(), depth, 1.0);
    p.icon('\u{E710}', ink, 10.0, l.new);
}

/// A key standing `depth` off the plate.
fn key(p: &Painter, r: &Rect, radius: f32, face: Color, depth: f32, opacity: f32) {
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 0.0 {
        return;
    }
    let d = depth.clamp(0.0, 2.5);
    let side = 1.0 + 2.5 * d;
    let shadow = theme::cast().fade(opacity * (0.35 + 0.35 * d.min(1.0)));
    p.cast(r, radius, (0.0, side + 1.5 * d), 3.0 + 6.0 * d, shadow);
    let below = Rect::new(r.x, r.y + side, r.w, r.h);
    let black = Color::rgb(0);
    p.fill_rounded(&below, radius, face.mix(black, 0.6).fade(opacity));
    let white = Color::rgb(0xFFFFFF);
    p.fill_rounded_gradient(
        r,
        radius,
        &[(0.0, face.mix(white, 0.07)), (1.0, face.mix(black, 0.1))],
        opacity,
    );
    let shade = theme::bevel_shade().fade(0.6);
    p.inner(r, radius, (1.0, 2.0), theme::bevel_light(), shade, opacity);
}

fn sunk(p: &Painter, r: &Rect, radius: f32, fill: Color) {
    let lip = Rect::new(r.x, r.y + 1.0, r.w, r.h).inset(-0.5);
    p.stroke_rounded(&lip, radius + 0.5, theme::engrave_light(), 1.0);
    p.fill_rounded(r, radius, fill);
    let (near, far) = (theme::hollow_shade(), theme::hollow_light());
    p.inner(r, radius, (1.5, 5.0), near, far, 1.0);
}

fn latched(p: &Painter, r: &Rect, radius: f32, opacity: f32) {
    let lip = Rect::new(r.x, r.y + 1.0, r.w, r.h).inset(-0.5);
    p.stroke_rounded(
        &lip,
        radius + 0.5,
        theme::engrave_light().fade(opacity),
        1.0,
    );
    let (near, far) = (theme::hollow_shade(), theme::hollow_light());
    p.inner(r, radius, (2.0, 6.0), near, far, opacity);
}

fn lamp(p: &Painter, r: &Rect, c: Color, level: f32) {
    let round = r.w / 2.0;
    let housing = r.inset(-1.5);
    let black = Color::rgb(0);
    p.fill_rounded(&housing, round + 1.5, black.with_alpha(0.55));
    if level <= 0.0 {
        p.fill_rounded(r, round, theme::lamp_off());
        let glint = Rect::new(r.x + 1.0, r.y + 2.0, r.w - 2.0, r.h * 0.3);
        p.fill_rounded(&glint, 1.0, Color::rgb(0xFFFFFF).with_alpha(0.08));
        return;
    }
    let level = level.min(1.0);
    let rings = 4;
    for i in 0..rings {
        let s = 1.5 + i as f32 * 2.0;
        let k = 1.0 - i as f32 / rings as f32;
        p.stroke_rounded(
            &r.inset(-s),
            round + s,
            c.with_alpha(level * 0.16 * k * k),
            2.0,
        );
    }
    p.fill_rounded(r, round, theme::lamp_off().mix(c, level));
    let hot = c.mix(Color::rgb(0xFFFFFF), 0.45).fade(level);
    p.fill_rounded(&r.inset(1.0), (round - 1.0).max(0.5), hot);
}

fn led(p: &Painter, x: f32, y: f32, c: Color) {
    p.glow_dot(x, y, 6.0, c, 0.45);
    p.fill_ellipse(x, y, 2.5, 2.5, c.mix(Color::rgb(0xFFFFFF), 0.3));
}

fn tank(p: &Painter, r: &Rect, fraction: f32, c: Color, segments: usize) {
    let gap = 1.0;
    let w = (r.w - gap * (segments as f32 - 1.0)) / segments as f32;
    let level = fraction.clamp(0.0, 1.0) * segments as f32;
    for i in 0..segments {
        let seg = Rect::new(r.x + i as f32 * (w + gap), r.y, w, r.h);
        p.fill_rounded(&seg, 1.0, theme::lamp_off());
        let full = (level - i as f32).clamp(0.0, 1.0);
        if full > 0.0 {
            p.fill_rounded(&Rect::new(seg.x, seg.y, seg.w * full, seg.h), 1.0, c);
        }
    }
}

fn halo(p: &Painter, r: &Rect, radius: f32, c: Color, strength: f32) {
    if strength <= 0.0 {
        return;
    }
    let rings = 5;
    for i in 0..rings {
        let s = 1.0 + i as f32 * 2.0;
        let k = 1.0 - i as f32 / rings as f32;
        let a = strength * 0.2 * k * k;
        p.stroke_rounded(&r.inset(-s), radius + s, c.with_alpha(a), 2.0);
    }
    p.stroke_rounded(
        &r.inset(0.75),
        radius - 0.75,
        c.with_alpha(strength * 0.75),
        1.5,
    );
}

fn scan(p: &Painter, r: &Rect, c: Color, t: f32, strength: f32) {
    let travel = (1.0 - (t * std::f32::consts::TAU).cos()) / 2.0;
    let y = r.y + 2.0 + (r.h - 4.0) * travel;
    let hot = c.mix(Color::rgb(0xFFFFFF), 0.5);
    p.glow_dot(r.x + r.w / 2.0, y, 7.0, hot, strength);
}

fn sparks(p: &Painter, r: &Rect, c: Color, n: usize, age: Duration, strength: f32) {
    if n == 0 {
        return;
    }
    let white = Color::rgb(0xFFFFFF);
    let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
    let (rx, ry) = (SPARK_REACH, r.h / 2.0 + 3.0);
    let turn = motion::cycle(age, SPARK_ORBIT);
    for k in 0..n {
        let a = std::f32::consts::TAU * (turn + k as f32 / n as f32);
        let (x, y) = (cx + rx * a.cos(), cy + ry * a.sin());
        p.glow_dot(x, y, 5.0, c.mix(white, 0.4), 0.8 * strength);
        p.glow_dot(x, y, 1.6, white, strength);
    }
}

fn shimmer(p: &Painter, r: &Rect, fraction: f32, t: f32, c: Color) {
    let lit = r.w * fraction.clamp(0.0, 1.0);
    if lit <= 0.0 {
        return;
    }
    let white = Color::rgb(0xFFFFFF);
    let x = r.x + lit * motion::ease_in_out(t);
    let fade = (std::f32::consts::PI * t).sin();
    p.glow_dot(x, r.y + r.h / 2.0, 5.0, c.mix(white, 0.5), 0.7 * fade);
}

/// The light that never stops while a session works or waits.
fn light(p: &Painter, m: &Metrics, s: &Scene) {
    for (r, session, look) in tiles(s) {
        let phase = &session.phase;
        let c = theme::phase_color(phase);
        match phase {
            Phase::Working => {
                let t = motion::cycle(look.phase_age, ORBIT);
                scan(p, &lamp_rect(&r), c, t, look.enter);
                let context = session.status.as_ref().and_then(|st| st.context);
                if let Some(c) = context.filter(|&c| c >= 75.0) {
                    let (ix, iy) = icon_centre(&r);
                    let track = Rect::new(ix - 10.0, iy + 14.0, 20.0, 3.0);
                    let t = motion::cycle(look.phase_age, SHIMMER);
                    shimmer(p, &track, look.context, t, theme::fullness_color(c));
                }
                let n = session.subagents(s.now).min(MAX_SPARKS);
                sparks(p, &lamp_rect(&r), c, n, look.phase_age, look.enter);
            }
            Phase::Waiting(_) => {
                let breath = motion::waiting_breath(look.phase_age);
                halo(p, &r, m.tile_radius, c, (0.3 + 0.4 * breath) * look.enter);
                let level = (0.55 + 0.45 * breath) * look.enter;
                lamp(p, &lamp_rect(&r), c, level);
            }
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn tile(p: &Painter, m: &Metrics, s: &Scene, i: usize, r: &Rect, session: &Session, look: &Look) {
    let b = s.button(Hit::Tile(i));
    let landing = if s.ambient && session.phase == Phase::Done {
        motion::land(look.phase_age)
    } else {
        0.0
    };
    let r = &Rect::new(r.x, r.y - LAND_RISE * landing, r.w, r.h);
    let phase = &session.phase;
    let c = theme::phase_color(phase);
    let lit = if *phase == Phase::Done && !session.unread() {
        &Phase::Idle
    } else {
        phase
    };
    let radius = m.tile_radius;
    let ambient = s.ambient;
    let selected = s.selected == Some(i);
    let stance = Stance {
        depth: theme::key_depth(phase, selected),
        presence: theme::presence(lit),
        lamp: theme::lamp(lit),
    };
    let stance = if selected || !ambient {
        stance
    } else {
        stance.from(look.was, look.settle)
    };
    let rest = stance.depth;
    let presence = stance.presence * look.enter;
    let lift = match b {
        Button::Pressed => (0.15 - rest).min(0.0),
        _ if selected => 0.0,
        _ => 0.3 * look.hover,
    };
    let depth = rest + lift + 0.9 * landing;
    let face = theme::phase_fill(phase).mix(theme::text(), 0.03 * look.hover);
    let face = if selected {
        face.mix(theme::well(), 0.25)
    } else {
        face
    };
    key(p, r, radius, face, depth, look.enter);
    if selected {
        latched(p, r, radius, look.enter);
    }

    let arrival = if ambient { look.arrival } else { 0.0 };
    match phase {
        Phase::Waiting(_) => {
            let glow = c.with_alpha(0.3);
            p.inner(r, radius, (2.0, 8.0), glow, glow, look.enter);
            if !ambient {
                halo(p, r, radius, c, 0.6);
            }
            if arrival > 0.0 {
                let spread = (1.0 - arrival) * 5.0;
                let ring = r.inset(-spread);
                p.stroke_rounded(&ring, radius + spread, c.with_alpha(arrival * 0.45), 1.2);
            }
        }
        Phase::Done if arrival > 0.0 => {
            p.fill_rounded(r, radius, c.with_alpha(0.14 * arrival * look.enter));
        }
        _ => {}
    }
    let breathing = ambient && phase.is_waiting();
    let level = if breathing {
        0.0
    } else {
        (stance.lamp + 0.3 * arrival) * look.enter
    };
    lamp(p, &lamp_rect(r), c, level);

    let icon_c = if matches!(lit, Phase::Idle | Phase::Ended | Phase::Paused) {
        theme::text_dim()
    } else {
        c
    };
    let (ix, iy) = icon_centre(r);
    let context = session
        .status
        .as_ref()
        .and_then(|st| st.context)
        .filter(|_| !matches!(phase, Phase::Paused | Phase::Ended));
    if let Some(c) = context {
        let ink = if c >= 75.0 {
            theme::fullness_color(c)
        } else {
            theme::text_dim().with_alpha(0.7)
        };
        let track = Rect::new(ix - 10.0, iy + 14.0, 20.0, 3.0);
        let level = if ambient { look.context } else { c / 100.0 };
        tank(p, &track, level, ink.fade(presence), 5);
    }
    let icon_r = Rect::new(ix - 14.0, iy - 14.0, 28.0, 28.0);
    p.icon(
        theme::tile_icon(session),
        icon_c.fade(presence),
        15.0,
        icon_r,
    );

    let pad = INNER_PAD;
    let left = r.x + TILE_TEXT_X;
    let width = r.right() - pad - left;
    let row_h = r.h / 2.0;
    let top = Rect::new(left, r.y + 5.0, width, row_h - 3.0);
    let bottom = Rect::new(left, r.y + row_h - 1.0, width, row_h - 5.0);

    let age = format_age(s.now.duration_since(session.since).unwrap_or_default());
    let age = match phase {
        Phase::Waiting(_) | Phase::Paused | Phase::Ended => {
            format!("{} {age}", theme::phase_verb(phase))
        }
        _ => age,
    };
    let age_w = measure(Font::Small, &age).min(top.w * 0.55);
    let mut name_rect = Rect::new(top.x, top.y, top.w - age_w - 8.0, top.h);
    if let Some(tag) = session.agent.mark() {
        let tag_w = measure(Font::Small, tag);
        if tag_w < name_rect.w * 0.4 {
            let ink = theme::text_dim().with_alpha(0.75).fade(presence);
            p.text(Font::SmallRight, ink, tag, name_rect);
            name_rect.w -= tag_w + 8.0;
        }
    }
    let ink = if horadric_core::warriv::is_warriv(&session.id)
        || horadric_core::runeword::is_errand(&session.id)
    {
        theme::warriv()
    } else {
        theme::rarity_color(session.rarity())
    };
    p.text(Font::Name, ink.fade(presence), session.label(), name_rect);
    let age_c = match phase {
        Phase::Waiting(_) => c,
        _ => theme::text_dim(),
    };
    p.text(Font::SmallRight, age_c.fade(presence), &age, top);

    let last = if session.last_line.is_empty() {
        &session.cwd
    } else {
        &session.last_line
    };
    let mut bottom = bottom;
    let diff = session
        .diff
        .as_ref()
        .map(Diff::totals)
        .filter(|&(added, removed)| added + removed > 0)
        .map(|(added, removed)| {
            let minus = format!("\u{2212}{removed}");
            let plus = format!("+{added}");
            let minus_w = measure(Font::Small, &minus);
            let plus_w = measure(Font::Small, &plus);
            (plus, minus, plus_w, minus_w)
        });
    let crowded = context.filter(|&c| c >= 75.0);
    let crowded_w = 58.0;
    let (activity, scrolled) = session.trace(s.now, TRACE_BARS);
    let trace_w = TRACE_BARS as f32 * (TRACE_BAR_W + TRACE_GAP) - TRACE_GAP;
    let busy = activity.iter().any(|&a| a > 0.0);
    let gap = 8.0;
    let parts = layout::tile_line(
        bottom.w,
        diff.as_ref().map(|d| d.2 + 4.0 + d.3 + gap),
        crowded.map(|_| crowded_w + gap),
        busy.then_some(trace_w + gap),
    );
    if let Some((plus, minus, plus_w, minus_w)) = diff.filter(|_| parts.diff) {
        p.text(
            Font::SmallRight,
            theme::git_deleted().fade(presence),
            &minus,
            bottom,
        );
        let left = Rect::new(bottom.x, bottom.y, bottom.w - minus_w - 4.0, bottom.h);
        p.text(
            Font::SmallRight,
            theme::git_added().fade(presence),
            &plus,
            left,
        );
        bottom.w -= minus_w + 4.0 + plus_w + gap;
    }
    if let Some(c) = crowded.filter(|_| parts.context) {
        let at = Rect::new(bottom.right() - crowded_w, bottom.y, crowded_w, bottom.h);
        p.text(
            Font::SmallRight,
            theme::fullness_color(c).fade(presence),
            &format!("ctx {}%", c.round()),
            at,
        );
        bottom.w -= crowded_w + gap;
    }
    if parts.trace {
        let trace_c = if icon_c == theme::text_dim() {
            theme::text_dim().with_alpha(0.45)
        } else {
            c.with_alpha(0.7)
        };
        let base = bottom.y + bottom.h / 2.0 + TRACE_H / 2.0;
        trace(
            p,
            &activity,
            scrolled,
            bottom.right() - trace_w,
            base,
            trace_c.fade(presence),
        );
        bottom.w -= trace_w + gap;
    }
    p.text(Font::Small, theme::text_dim().fade(presence), last, bottom);
}

fn trace(p: &Painter, activity: &[f32], scrolled: f32, x: f32, base: f32, c: Color) {
    let step = TRACE_BAR_W + TRACE_GAP;
    for (i, &a) in activity.iter().enumerate() {
        let bx = x + (i as f32 - scrolled) * step;
        let c = if i == 0 { c.fade(1.0 - scrolled) } else { c };
        let h = if a > 0.0 {
            2.0 + a * (TRACE_H - 2.0)
        } else {
            1.0
        };
        let alpha = if a > 0.0 { 1.0 } else { 0.35 };
        p.fill_rounded(
            &Rect::new(bx, base - h, TRACE_BAR_W, h),
            TRACE_BAR_W / 2.0,
            c.fade(alpha),
        );
    }
}

/// Another session in this project, or a plain terminal: an empty bay
/// where the next key would go.
fn add_button(p: &Painter, m: &Metrics, r: &Rect, b: Button, glyph: char) {
    let (_, ink) = theme::button_look(b);
    match b {
        Button::Hover => key(p, r, m.tile_radius, theme::surface(), 0.6, 1.0),
        Button::Idle | Button::Pressed => {
            sunk(p, r, m.tile_radius, theme::well());
            let edge = r.inset(2.5);
            let radius = m.tile_radius - 2.5;
            let length = motion::perimeter(edge.w, edge.h, radius);
            let count = (length / 7.0).round().max(1.0);
            let unit = length / count / 1.2;
            p.dashed_rounded(
                &edge,
                radius,
                theme::legend().with_alpha(0.35),
                unit * 0.35,
                unit * 0.65,
            );
        }
    }
    p.icon(glyph, ink, 11.0, *r);
}
