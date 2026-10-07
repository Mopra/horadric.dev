//! The Segoe Fluent Icons glyphs the theme names, as SF Symbols: the Mac
//! draws the same icons from its own set. Pure, so every glyph the theme
//! uses is checked to have one.

/// The SF Symbol for a Segoe Fluent Icons glyph. Anything unknown is a
/// wrench, as an unknown tool is on Windows.
pub fn sf_symbol(glyph: char) -> &'static str {
    match glyph {
        // Phases.
        '\u{EA80}' => "ellipsis",
        '\u{E72E}' => "lock.fill",
        '\u{E9CE}' => "questionmark.bubble",
        '\u{E8BD}' => "text.bubble",
        '\u{E783}' => "exclamationmark.triangle",
        '\u{E73E}' => "checkmark",
        '\u{E769}' => "pause.fill",
        '\u{E7E8}' => "power",
        '\u{E708}' => "moon.zzz",
        // Tools.
        '\u{E950}' => "puzzlepiece.extension",
        '\u{E756}' => "terminal",
        '\u{E8A5}' => "doc.text",
        '\u{E70F}' => "pencil",
        '\u{E70B}' => "square.and.pencil",
        '\u{E721}' => "magnifyingglass",
        '\u{E774}' => "globe",
        '\u{E716}' => "person.2",
        '\u{E9D5}' => "checklist",
        '\u{E945}' => "sparkles",
        '\u{E943}' => "chevron.left.forwardslash.chevron.right",
        // Kinds of terminal.
        '\u{E968}' => "network",
        '\u{E753}' => "cloud",
        // Buttons and chevrons.
        '\u{E710}' => "plus",
        '\u{E76C}' => "chevron.right",
        '\u{E70D}' => "chevron.down",
        '\u{E711}' => "xmark",
        '\u{E8BB}' => "xmark",
        _ => "wrench.and.screwdriver",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;
    use horadric_core::session::WaitReason;
    use horadric_core::Phase;

    #[test]
    fn every_phase_and_tool_has_its_own_symbol() {
        let phases = [
            Phase::Working,
            Phase::Waiting(WaitReason::Permission),
            Phase::Waiting(WaitReason::Input),
            Phase::Done,
            Phase::Paused,
            Phase::Ended,
            Phase::Idle,
        ];
        for p in &phases {
            assert_ne!(
                sf_symbol(theme::icon(p, None)),
                "wrench.and.screwdriver",
                "{p:?}"
            );
        }
        for tool in ["Bash", "Read", "Edit", "Write", "Grep", "WebFetch", "Agent"] {
            assert_ne!(
                sf_symbol(theme::tool_icon(tool)),
                "wrench.and.screwdriver",
                "{tool}"
            );
        }
        assert_eq!(
            sf_symbol(theme::tool_icon("SomethingNew")),
            "wrench.and.screwdriver"
        );
        assert_eq!(sf_symbol(theme::SHELL_ICON), "terminal");
    }
}
