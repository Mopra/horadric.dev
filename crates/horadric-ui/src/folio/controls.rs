//! What the composer's controls say and cycle through, without any window:
//! the permission modes in Shift+Tab's order, the model's name, and the
//! context meter's words.

use horadric_core::folio::ModelInfo;

/// The permission modes in the order Shift+Tab walks them, with what the
/// control calls each.
pub const MODES: &[(&str, &str)] = &[
    ("default", "Ask before edits"),
    ("acceptEdits", "Accept edits"),
    ("plan", "Plan"),
    ("auto", "Auto"),
    ("bypassPermissions", "Bypass permissions"),
];

const BYPASS: &str = "bypassPermissions";

/// The mode after `now` for Shift+Tab. Bypass is in the round only when
/// the session was started allowed to bypass.
pub fn next_mode(now: &str, bypass: bool) -> &'static str {
    let round: Vec<&'static str> = MODES
        .iter()
        .map(|(m, _)| *m)
        .filter(|m| bypass || *m != BYPASS)
        .collect();
    let at = round.iter().position(|m| *m == now);
    round[at.map_or(0, |i| (i + 1) % round.len())]
}

/// The modes the picker offers.
pub fn modes(bypass: bool) -> impl Iterator<Item = (&'static str, &'static str)> {
    MODES
        .iter()
        .copied()
        .filter(move |(m, _)| bypass || *m != BYPASS)
}

pub fn mode_label(mode: &str) -> &str {
    MODES
        .iter()
        .find(|(m, _)| *m == mode)
        .map_or(mode, |(_, l)| l)
}

/// What the model control says: the picked model's short name, else the
/// id the CLI runs, shortened.
pub fn model_label(id: Option<&str>, picked: Option<&str>, models: &[ModelInfo]) -> String {
    if let Some(m) = picked.and_then(|p| models.iter().find(|m| m.value == p)) {
        return short_name(&m.display_name);
    }
    let Some(id) = id else {
        // Before the first turn the CLI has not said; it runs its default.
        return models
            .iter()
            .find(|m| m.value == "default")
            .map_or_else(|| "Model".to_string(), |m| short_name(&m.display_name));
    };
    if let Some(m) = models
        .iter()
        .find(|m| m.resolved == id && m.value != "default")
    {
        return short_name(&m.display_name);
    }
    pretty_id(id)
}

/// "Default (recommended)" is "Default".
fn short_name(s: &str) -> String {
    s.split(" (").next().unwrap_or(s).to_string()
}

/// `claude-opus-5-5` is "Opus 5.5", `claude-sonnet-4-5-20250929` is
/// "Sonnet 4.5".
pub fn pretty_id(id: &str) -> String {
    let rest = id.strip_prefix("claude-").unwrap_or(id);
    let rest = rest.split('[').next().unwrap_or(rest);
    let mut parts = rest.split('-');
    let Some(family) = parts.next() else {
        return id.to_string();
    };
    let version: Vec<&str> = parts
        .take_while(|p| p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit()))
        .collect();
    let mut name: String = family
        .chars()
        .take(1)
        .flat_map(char::to_uppercase)
        .chain(family.chars().skip(1))
        .collect();
    if !version.is_empty() {
        name.push(' ');
        name.push_str(&version.join("."));
    }
    name
}

/// Tokens in a few characters: 950, 12k, 1M.
pub fn tokens(n: u64) -> String {
    if n >= 1_000_000 {
        let m = n as f64 / 1_000_000.0;
        if m.fract() < 0.05 {
            format!("{}M", m.round() as u64)
        } else {
            format!("{m:.1}M")
        }
    } else if n >= 1000 {
        format!("{}k", (n as f64 / 1000.0).round() as u64)
    } else {
        n.to_string()
    }
}

/// The context meter: how full, 0 to 1, and its words.
pub fn meter(used: u64, limit: u64) -> (f32, String) {
    if limit == 0 {
        return (0.0, tokens(used));
    }
    let full = (used as f64 / limit as f64).clamp(0.0, 1.0);
    let pct = (full * 100.0).round() as u64;
    (full as f32, format!("{pct}% of {}", tokens(limit)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_tab_walks_the_modes_and_wraps() {
        assert_eq!(next_mode("default", false), "acceptEdits");
        assert_eq!(next_mode("acceptEdits", false), "plan");
        assert_eq!(next_mode("plan", false), "auto");
        assert_eq!(next_mode("auto", false), "default");
        assert_eq!(next_mode("auto", true), "bypassPermissions");
        assert_eq!(next_mode("bypassPermissions", true), "default");
    }

    #[test]
    fn an_unknown_mode_goes_to_the_first() {
        assert_eq!(next_mode("", false), "default");
        assert_eq!(next_mode("bypassPermissions", false), "default");
    }

    #[test]
    fn the_picker_offers_bypass_only_when_allowed() {
        assert_eq!(modes(false).count(), 4);
        assert_eq!(modes(true).count(), 5);
        assert_eq!(mode_label("plan"), "Plan");
        assert_eq!(mode_label("odd"), "odd");
    }

    fn model(value: &str, resolved: &str, name: &str) -> ModelInfo {
        ModelInfo {
            value: value.into(),
            resolved: resolved.into(),
            display_name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn the_model_is_named_by_what_was_picked_else_by_its_id() {
        let models = vec![
            model("default", "claude-opus-5-5", "Default (recommended)"),
            model("opus", "claude-opus-5-5", "Opus 5.5"),
            model("haiku", "claude-haiku-4-5", "Haiku 4.5"),
        ];
        assert_eq!(model_label(Some("x"), Some("default"), &models), "Default");
        assert_eq!(
            model_label(Some("claude-opus-5-5"), None, &models),
            "Opus 5.5"
        );
        assert_eq!(
            model_label(Some("claude-sonnet-4-5-20250929"), None, &models),
            "Sonnet 4.5"
        );
        assert_eq!(model_label(None, None, &[]), "Model");
        assert_eq!(model_label(None, None, &models), "Default");
    }

    #[test]
    fn ids_read_as_names() {
        assert_eq!(pretty_id("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(pretty_id("claude-opus-4-6[1m]"), "Opus 4.6");
        assert_eq!(pretty_id("custom"), "Custom");
    }

    #[test]
    fn tokens_are_short() {
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(12_400), "12k");
        assert_eq!(tokens(200_000), "200k");
        assert_eq!(tokens(1_000_000), "1M");
        assert_eq!(tokens(1_500_000), "1.5M");
    }

    #[test]
    fn the_meter_says_how_full() {
        let (f, s) = meter(50_000, 200_000);
        assert!((f - 0.25).abs() < 1e-6);
        assert_eq!(s, "25% of 200k");
        assert_eq!(meter(10, 0), (0.0, "10".to_string()));
    }
}
