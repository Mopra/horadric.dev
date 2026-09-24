//! Colours and icons. Dark clay: deep plum slate surfaces that look
//! moulded, lit from the top left, so everything is either puffed up off
//! the window or pressed into it. Bright text, and phase colours saturated
//! enough to glow against the dark.
//!
//! Colour has two jobs and they never share a place. A phase is a tint and
//! a light: the fill and the edge of a tile, its icon, the line along a pane
//! header. A project is an accent: the wash down its cluster, the edge of
//! the stage showing it. So a project's colour is never mistaken for a
//! session needing you.

use horadric_core::{Phase, WaitReason};

use crate::files::Change;
use crate::layout::Button;

/// sRGB with straight alpha, 0.0 to 1.0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn rgb(hex: u32) -> Self {
        Color {
            r: ((hex >> 16) & 0xff) as f32 / 255.0,
            g: ((hex >> 8) & 0xff) as f32 / 255.0,
            b: (hex & 0xff) as f32 / 255.0,
            a: 1.0,
        }
    }

    pub const fn with_alpha(self, a: f32) -> Self {
        Color { a, ..self }
    }

    /// The same colour with its alpha scaled, for fading something out.
    pub fn fade(self, k: f32) -> Self {
        Color {
            a: self.a * k.clamp(0.0, 1.0),
            ..self
        }
    }

    /// `t` of the way from `self` to `other`, opaque.
    pub fn mix(self, other: Color, t: f32) -> Self {
        let l = |a: f32, b: f32| a + (b - a) * t;
        Color {
            r: l(self.r, other.r),
            g: l(self.g, other.g),
            b: l(self.b, other.b),
            a: 1.0,
        }
    }
}

/// The clay every window is moulded from: a deep plum slate.
pub const WINDOW_BG: Color = Color::rgb(0x1C1A27);
/// A surface puffed up off the window: a tile, a panel. A shade lighter
/// than the window, so it catches more of the light.
pub const SURFACE: Color = Color::rgb(0x282536);
/// A surface pressed into the window: an empty slot, an ended session.
pub const WELL: Color = Color::rgb(0x16141F);
pub const TEXT: Color = Color::rgb(0xF2F0F8);
pub const TEXT_DIM: Color = Color::rgb(0xA6A1B8);

/// The shadow anything raised casts on the window.
pub const CAST: Color = Color::rgb(0x000000).with_alpha(0.55);
/// The light catching the top left rim of anything raised, and the shade
/// on its far rim. On dark clay the light has to stay faint, or it reads
/// as a white edge rather than a curve.
pub const RIM_LIGHT: Color = Color::rgb(0xFFFFFF).with_alpha(0.09);
pub const RIM_SHADE: Color = Color::rgb(0x000000).with_alpha(0.35);
/// Inside a hollow: shade under its top left lip, light on the far side.
pub const HOLLOW_SHADE: Color = Color::rgb(0x000000).with_alpha(0.5);
pub const HOLLOW_LIGHT: Color = Color::rgb(0xFFFFFF).with_alpha(0.04);
/// A hollow's lower lip, catching the light on the window around it.
pub const LIP: Color = Color::rgb(0xFFFFFF).with_alpha(0.06);
/// The window's own rim.
pub const SLAB_LIGHT: Color = Color::rgb(0xFFFFFF).with_alpha(0.06);
pub const SLAB_SHADE: Color = Color::rgb(0x000000).with_alpha(0.3);

/// Behind a button under the cursor and one held down: white laid over
/// whatever is there, as Windows 11 does it, so it works on any surface.
pub const HOVER_FILL: Color = Color::rgb(0xFFFFFF).with_alpha(0.06);
pub const PRESS_FILL: Color = Color::rgb(0xFFFFFF).with_alpha(0.025);

pub const WORKING: Color = Color::rgb(0x3DB4FF);
pub const WAITING: Color = Color::rgb(0xFFB224);
pub const ERROR: Color = Color::rgb(0xFF5D66);
pub const DONE: Color = Color::rgb(0x3DD68C);
pub const IDLE: Color = Color::rgb(0x6E6882);

/// Git change colours, VS Code's dark theme ones, so a file looks the same
/// in the tile as in the editor.
pub const GIT_MODIFIED: Color = Color::rgb(0xE2C08D);
pub const GIT_ADDED: Color = Color::rgb(0x81B88B);
pub const GIT_UNTRACKED: Color = Color::rgb(0x73C991);
pub const GIT_DELETED: Color = Color::rgb(0xC74E39);
pub const GIT_CONFLICT: Color = Color::rgb(0xE4676B);

pub fn change_color(change: Change) -> Color {
    match change {
        Change::Modified => GIT_MODIFIED,
        Change::Added => GIT_ADDED,
        Change::Untracked | Change::Renamed => GIT_UNTRACKED,
        Change::Deleted => GIT_DELETED,
        Change::Conflict => GIT_CONFLICT,
    }
}

/// The fill behind a button and the colour of its glyph. Pressed dims back
/// below hover, which is what makes the press read as a push.
pub fn button_look(b: Button) -> (Option<Color>, Color) {
    match b {
        Button::Idle => (None, TEXT_DIM),
        Button::Hover => (Some(HOVER_FILL), TEXT),
        Button::Pressed => (Some(PRESS_FILL), TEXT_DIM),
    }
}

/// Project colours: distinct from each other and from every phase colour,
/// so an accent never reads as a state. Soft enough to sit beside text.
pub const ACCENTS: [Color; 8] = [
    Color::rgb(0xA78BFA), // violet
    Color::rgb(0x818CF8), // indigo
    Color::rgb(0xE879F9), // orchid
    Color::rgb(0xF472B6), // pink
    Color::rgb(0x2DD4BF), // teal
    Color::rgb(0xBEF264), // lime
    Color::rgb(0xE0976B), // copper
    Color::rgb(0xFDA4AF), // rose
];

/// A project's colour, the same every time for the same key.
pub fn accent(key: &str) -> Color {
    // FNV-1a: tiny, and stable across runs and builds, unlike std's hasher.
    let hash = key.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ b as u32).wrapping_mul(0x0100_0193)
    });
    ACCENTS[hash as usize % ACCENTS.len()]
}

/// A plain terminal's tile, and the button that opens one.
pub const SHELL_ICON: char = '\u{E756}';

/// The Segoe Fluent Icons glyph for a session: the tool it is in while it
/// works, otherwise what its phase is.
pub fn icon(phase: &Phase, tool: Option<&str>) -> char {
    match phase {
        Phase::Working => tool.map_or('\u{EA80}', tool_icon),
        Phase::Waiting(WaitReason::Permission) => '\u{E72E}',
        Phase::Waiting(WaitReason::Input) => '\u{E9CE}',
        Phase::Waiting(WaitReason::Dialog) => '\u{E8BD}',
        Phase::Waiting(WaitReason::Error(_)) => '\u{E783}',
        Phase::Done => '\u{E73E}',
        Phase::Paused => '\u{E769}',
        Phase::Ended => '\u{E7E8}',
        Phase::Idle => '\u{E708}',
    }
}

/// A Claude Code tool as an icon. Anything unknown is a wrench.
pub fn tool_icon(tool: &str) -> char {
    if tool.starts_with("mcp__") {
        return '\u{E950}';
    }
    match tool {
        "Bash" | "PowerShell" | "BashOutput" | "KillShell" | "Monitor" => '\u{E756}',
        "Read" => '\u{E8A5}',
        "Edit" | "MultiEdit" | "NotebookEdit" => '\u{E70F}',
        "Write" => '\u{E70B}',
        "Grep" | "Glob" | "ToolSearch" => '\u{E721}',
        "WebFetch" | "WebSearch" => '\u{E774}',
        "Task" | "Agent" | "SendMessage" => '\u{E716}',
        "TodoWrite" | "TaskCreate" | "TaskUpdate" => '\u{E9D5}',
        "Skill" | "Workflow" => '\u{E945}',
        "LSP" => '\u{E943}',
        _ => '\u{E90F}',
    }
}

/// How strongly a tile's edge glows at rest, by phase. Zero is no edge.
pub fn edge_strength(phase: &Phase) -> f32 {
    match phase {
        Phase::Waiting(_) => 0.34,
        Phase::Done => 0.10,
        Phase::Working => 0.09,
        Phase::Idle | Phase::Ended | Phase::Paused => 0.0,
    }
}

/// How far a tile stands off the window, by phase. One is a resting
/// pillow. Waiting needs you, so it puffs up furthest. A session that can
/// no longer act sinks into the clay, below zero.
pub fn depth(phase: &Phase) -> f32 {
    match phase {
        Phase::Waiting(_) => 1.4,
        Phase::Working | Phase::Done => 1.0,
        Phase::Idle => 0.8,
        Phase::Paused | Phase::Ended => -0.6,
    }
}

/// A session that cannot act on its own right now fades back, so the ones
/// that can, or that need you, come forward.
pub fn presence(phase: &Phase) -> f32 {
    match phase {
        Phase::Paused | Phase::Ended => 0.55,
        Phase::Idle => 0.8,
        _ => 1.0,
    }
}

/// The full strength colour for a phase.
pub fn phase_color(phase: &Phase) -> Color {
    match phase {
        Phase::Working => WORKING,
        Phase::Waiting(WaitReason::Error(_)) => ERROR,
        Phase::Waiting(_) => WAITING,
        Phase::Done => DONE,
        Phase::Idle | Phase::Ended | Phase::Paused => IDLE,
    }
}

/// A tile's clay, tinted by its phase. Waiting is the strongest because it
/// needs you. A session doing nothing keeps the plain surface, so colour
/// always means something, and one that has stopped is the pressed well.
pub fn phase_fill(phase: &Phase) -> Color {
    let strength = match phase {
        Phase::Waiting(_) => 0.2,
        Phase::Working => 0.1,
        Phase::Done => 0.12,
        Phase::Idle => return SURFACE,
        Phase::Ended | Phase::Paused => return WELL,
    };
    SURFACE.mix(phase_color(phase), strength)
}

/// How much of a limit or a context window is used, in percent, as a
/// colour: calm while there is room, amber getting close, red at the end.
pub fn fullness_color(percent: f32) -> Color {
    if percent >= 90.0 {
        ERROR
    } else if percent >= 75.0 {
        WAITING
    } else {
        WORKING
    }
}

/// What the age line says before the duration: "waiting 40 min".
pub fn phase_verb(phase: &Phase) -> &'static str {
    match phase {
        Phase::Idle => "idle",
        Phase::Working => "working",
        Phase::Waiting(WaitReason::Permission) => "needs permission",
        Phase::Waiting(WaitReason::Input) => "asked you",
        Phase::Waiting(WaitReason::Dialog) => "dialog open",
        Phase::Waiting(WaitReason::Error(_)) => "failed",
        Phase::Done => "done",
        Phase::Ended => "ended",
        Phase::Paused => "paused",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_ends_are_the_colours() {
        let a = Color::rgb(0x000000);
        let b = Color::rgb(0xFFFFFF);
        assert_eq!(a.mix(b, 0.0), a);
        assert_eq!(a.mix(b, 1.0), b);
        assert_eq!(a.mix(b, 0.5).g, 0.5);
    }

    #[test]
    fn a_button_brightens_on_hover_and_sinks_back_when_pressed() {
        let (idle, idle_ink) = button_look(Button::Idle);
        let (hover, hover_ink) = button_look(Button::Hover);
        let (press, press_ink) = button_look(Button::Pressed);
        assert!(idle.is_none());
        assert!(hover.unwrap().a > press.unwrap().a);
        assert_eq!(hover_ink, TEXT);
        assert_eq!(idle_ink, press_ink);
    }

    #[test]
    fn fuller_turns_amber_then_red() {
        assert_eq!(fullness_color(10.0), WORKING);
        assert_eq!(fullness_color(75.0), WAITING);
        assert_eq!(fullness_color(99.5), ERROR);
        assert_eq!(fullness_color(140.0), ERROR);
    }

    #[test]
    fn a_quiet_session_keeps_the_plain_clay_and_a_stopped_one_sinks() {
        assert_eq!(phase_fill(&Phase::Idle), SURFACE);
        for p in [Phase::Ended, Phase::Paused] {
            assert_eq!(phase_fill(&p), WELL);
            assert!(depth(&p) < 0.0);
        }
    }

    #[test]
    fn waiting_puffs_up_furthest() {
        let waiting = depth(&Phase::Waiting(WaitReason::Input));
        for p in [Phase::Working, Phase::Done, Phase::Idle] {
            assert!(waiting > depth(&p));
            assert!(depth(&p) > 0.0);
        }
    }

    #[test]
    fn a_project_keeps_its_accent_and_projects_spread_over_them() {
        assert_eq!(accent("c:/code/horadric"), accent("c:/code/horadric"));
        let keys = ["a", "b", "c", "horadric", "purrch", "opticore", "x/y", "zz"];
        let distinct: std::collections::HashSet<u32> = keys
            .iter()
            .map(|k| {
                let c = accent(k);
                ((c.r * 255.0) as u32) << 16 | ((c.g * 255.0) as u32) << 8 | (c.b * 255.0) as u32
            })
            .collect();
        assert!(
            distinct.len() >= 4,
            "eight keys landed on {}",
            distinct.len()
        );
    }

    #[test]
    fn no_accent_is_a_phase_colour() {
        for a in ACCENTS {
            for p in [WORKING, WAITING, ERROR, DONE, IDLE] {
                let d = (a.r - p.r).abs() + (a.g - p.g).abs() + (a.b - p.b).abs();
                assert!(d > 0.25, "{a:?} is too close to {p:?}");
            }
        }
    }

    #[test]
    fn the_icon_follows_the_tool_while_working_and_the_phase_otherwise() {
        assert_eq!(icon(&Phase::Working, Some("Bash")), '\u{E756}');
        assert_eq!(icon(&Phase::Working, Some("PowerShell")), '\u{E756}');
        assert_eq!(
            icon(&Phase::Working, Some("mcp__github__search")),
            '\u{E950}'
        );
        assert_eq!(icon(&Phase::Working, Some("SomethingNew")), '\u{E90F}');
        assert_eq!(icon(&Phase::Working, None), '\u{EA80}');
        // A stale tool never shows once the turn is over.
        assert_eq!(icon(&Phase::Done, Some("Bash")), '\u{E73E}');
        assert_eq!(
            icon(&Phase::Waiting(WaitReason::Permission), Some("Bash")),
            '\u{E72E}'
        );
    }

    #[test]
    fn waiting_glows_brightest_and_quiet_sessions_recede() {
        let waiting = edge_strength(&Phase::Waiting(WaitReason::Input));
        assert!(waiting > edge_strength(&Phase::Working));
        assert!(waiting > edge_strength(&Phase::Done));
        assert_eq!(edge_strength(&Phase::Idle), 0.0);
        assert_eq!(presence(&Phase::Working), 1.0);
        assert!(presence(&Phase::Paused) < presence(&Phase::Idle));
    }

    #[test]
    fn fading_scales_alpha_and_nothing_else() {
        let c = WORKING.with_alpha(0.5).fade(0.5);
        assert_eq!(c.a, 0.25);
        assert_eq!(c.r, WORKING.r);
        assert_eq!(WORKING.fade(2.0).a, 1.0);
    }

    #[test]
    fn waiting_is_tinted_hardest() {
        let dist = |c: Color| (c.r - SURFACE.r).abs() + (c.b - SURFACE.b).abs();
        let waiting = phase_fill(&Phase::Waiting(WaitReason::Input));
        let working = phase_fill(&Phase::Working);
        assert!(dist(waiting) > dist(working));
        assert!(waiting.r > waiting.b, "amber, not blue");
    }
}
