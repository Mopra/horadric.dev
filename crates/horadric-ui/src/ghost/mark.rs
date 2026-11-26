//! The mark that goes ahead of every click: the session's rune, travelling
//! from where the pointer is to where the click will land, with the
//! session's name small beside it. The human sees the aim before the act.
//!
//! A rune and not an arrow: there is one pointer on the screen and it is
//! the human's. Each session has a rune of its own, picked by its name as
//! a hireling's is by its id, so two sessions at work are told apart.
//!
//! The exact point is a small dot at the rune's upper left, where a
//! pointer has its tip. On arrival the rune flares and throws a few
//! sparks, the flare a tool cast gives the runes round a terminal, and
//! nothing grows or rings.
//!
//! Its window is painted when it changes (a new session, a flare, a
//! stop) and only moved while it travels, which sends no pixels.

use std::time::Duration;

use windows::Win32::Graphics::DirectWrite::IDWriteTextLayout;
use windows_numerics::Vector2;

use super::layer::{d2rect, Pen};
use crate::particles::{self, Burst, Kind};
use crate::render::{self, color};
use crate::sparks::RUNES;
use crate::theme::{self, Color};

/// The mark's window, in DIPs, and where in it the point is: room round
/// the point for the sparks and beside it for the name.
pub(super) const SIZE: (f32, f32) = (236.0, 64.0);
pub(super) const TIP: f32 = 22.0;
/// The widest a name shows.
pub(super) const NAME_MAX: f32 = 150.0;
const RUNE_H: f32 = 22.0;
const RUNE_CUT: f32 = 2.0;
/// The rune's middle from the point, as a pointer's body lies from its
/// tip.
const RUNE_AT: (f32, f32) = (12.0, 17.0);
/// How long the rune stays hot after it lands.
pub(super) const FLARE: Duration = Duration::from_millis(520);
const SPARKS: usize = 7;

/// The rune a session of this name carries.
pub(super) fn rune_of(name: &str) -> usize {
    (particles::seed_of(name) % RUNES.len() as u64) as usize
}

/// How long the flourish plays: the flare, and the last spark's life.
pub(super) fn flourish() -> Duration {
    let sparks = Burst::new(0, SPARKS, Kind::Spark).span();
    FLARE.max(Duration::from_secs_f32(sparks))
}

pub(super) struct MarkScene<'a> {
    pub rune: usize,
    pub tint: Color,
    /// The session's name laid out, and how wide it came.
    pub name: Option<(&'a IDWriteTextLayout, f32)>,
    /// Seconds since it landed, while the flourish plays, and its seed.
    pub landed: Option<(f32, u64)>,
}

pub(super) unsafe fn draw(p: &Pen, s: &MarkScene) {
    let tip = (TIP, TIP);
    let heat = s.landed.map_or(0.0, |(age, _)| {
        crate::sparks::cool(age, FLARE)
    });
    let ink = particles::heat_color(s.tint, 0.25 + 1.1 * heat);
    let bed = Color::rgb(0x000000);

    // The point itself.
    p.circle(tip, 4.2, bed.with_alpha(0.5), None);
    p.circle(tip, 2.8, ink, None);

    let at = (TIP + RUNE_AT.0, TIP + RUNE_AT.1);
    p.rune_on_anything(s.rune, at, (0.0, -1.0), RUNE_H, ink, RUNE_CUT, 1.0);

    if let Some((layout, w)) = s.name {
        // A small dark plate, so the name reads on whatever is behind.
        let x = at.0 + 15.0;
        let y = at.1 - 9.0;
        let r = d2rect(x, y, w.min(NAME_MAX) + 14.0, 18.0);
        p.fill_rounded(r, 4.0, theme::plate_bottom().with_alpha(0.9));
        p.stroke_rounded(
            d2rect(x + 0.5, y + 0.5, w.min(NAME_MAX) + 13.0, 17.0),
            4.0,
            s.tint.with_alpha(0.45),
            1.0,
        );
        p.brush.SetColor(&color(theme::text()));
        p.rt.DrawTextLayout(
            Vector2 {
                X: x + 7.0,
                Y: y + (18.0 - render::text_size(layout).1) / 2.0,
            },
            layout,
            p.brush,
            windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_CLIP,
        );
    }

    if let Some((age, seed)) = s.landed {
        let burst = Burst::new(seed, SPARKS, Kind::Spark);
        for i in 0..burst.count {
            if let Some(spark) = burst.particle(i, age) {
                // Sparks fly a third as far here as off a terminal's rim:
                // this one lands on someone else's window.
                let c = particles::heat_color(s.tint, spark.heat).fade(spark.alpha);
                p.circle(
                    (tip.0 + spark.x * 0.45, tip.1 + spark.y * 0.45),
                    spark.size,
                    c,
                    None,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_keeps_its_rune_and_another_may_have_another() {
        assert_eq!(rune_of("alpha"), rune_of("alpha"));
        assert!(rune_of("alpha") < RUNES.len());
        let all: std::collections::HashSet<usize> =
            ["a", "b", "c", "d", "e", "f", "g", "h"].map(rune_of).into();
        assert!(all.len() > 1);
    }

    #[test]
    fn the_flourish_is_brief_and_its_sparks_stay_in_the_window() {
        assert!(flourish() <= Duration::from_millis(700));
        let burst = Burst::new(7, SPARKS, Kind::Spark);
        for i in 0..burst.count {
            for step in 0..20 {
                if let Some(s) = burst.particle(i, step as f32 * 0.03) {
                    assert!((s.x * 0.45).abs() < TIP && (s.y * 0.45).abs() < TIP);
                }
            }
        }
    }
}
