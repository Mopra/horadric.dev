//! Colours. Dark only for now, matching a Windows 11 dark desktop.

use glance_core::{Phase, WaitReason};

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
}

pub const WINDOW_BG: Color = Color::rgb(0x1B1B1F);
pub const TILE_BG: Color = Color::rgb(0x27272C);
pub const TILE_BG_WAITING: Color = Color::rgb(0x3A2F1D);
pub const TEXT: Color = Color::rgb(0xECECEE);
pub const TEXT_DIM: Color = Color::rgb(0x9A9AA2);

pub const WORKING: Color = Color::rgb(0x4FC3F7);
pub const WAITING: Color = Color::rgb(0xFFB74D);
pub const ERROR: Color = Color::rgb(0xEF5350);
pub const DONE: Color = Color::rgb(0x81C784);
pub const IDLE: Color = Color::rgb(0x6E6E76);

/// The dot colour for a phase.
pub fn phase_color(phase: &Phase) -> Color {
    match phase {
        Phase::Working => WORKING,
        Phase::Waiting(WaitReason::Error(_)) => ERROR,
        Phase::Waiting(_) => WAITING,
        Phase::Done => DONE,
        Phase::Idle | Phase::Ended => IDLE,
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
    }
}
