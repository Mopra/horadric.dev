//! The band above a Folio's composer: the quest the session holds and the
//! files another session changed too and has not committed. Horadric's mod
//! draws the same above a terminal's prompt, but its `ui.render` never runs
//! headless (no screen: `session.start` says `surface: null`), so the
//! Folio draws it itself from what the app knows, as `/horadric/know`
//! would answer.

use crate::layout::Rect;
use crate::render::{self, Gpu};
use crate::theme::Color;

use super::view::{Gfx, Palette};

/// What the band says, empty when there is nothing to say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Band {
    pub quest: Option<String>,
    pub shared: Vec<Shared>,
}

/// A file this session changed that others changed too, uncommitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shared {
    /// The file's name alone: the path is the agent's, and long.
    pub file: String,
    /// The other sessions, by their tile labels.
    pub others: Vec<String>,
}

impl Band {
    /// From the quest held and each shared path with the sessions that
    /// changed it. A path whose sessions have all gone says nothing.
    pub fn new(quest: Option<String>, shared: Vec<(String, Vec<String>)>) -> Band {
        let shared = shared
            .into_iter()
            .filter(|(_, others)| !others.is_empty())
            .map(|(path, others)| Shared {
                file: path.rsplit(['/', '\\']).next().unwrap_or(&path).to_string(),
                others,
            })
            .collect();
        Band {
            quest: quest.filter(|q| !q.trim().is_empty()),
            shared,
        }
    }
}

/// The sessions named as the mod names them: "A", "A and B", "A, B and C".
pub fn others(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// More shared files than this fold into one line, so a busy tree does
/// not eat the conversation in a short pane.
const MOST: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lead {
    Quest,
    File,
    More,
}

/// One line of the band: a lead in its own colour, then the rest dimmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub lead: String,
    pub kind: Lead,
    pub rest: String,
}

pub fn rows(b: &Band) -> Vec<Row> {
    let mut out = Vec::new();
    if let Some(q) = &b.quest {
        out.push(Row {
            lead: "Quest ".into(),
            kind: Lead::Quest,
            rest: q.clone(),
        });
    }
    let shown = if b.shared.len() > MOST {
        MOST - 1
    } else {
        MOST
    };
    for s in b.shared.iter().take(shown) {
        out.push(Row {
            lead: s.file.clone(),
            kind: Lead::File,
            rest: format!(" also changed by {}, not committed", others(&s.others)),
        });
    }
    let left = b.shared.len().saturating_sub(shown);
    if left > 0 {
        out.push(Row {
            lead: format!("{left} more files"),
            kind: Lead::More,
            rest: " also changed by other sessions, not committed".into(),
        });
    }
    out
}

const LINE: f32 = 19.0;
const PAD: f32 = 14.0;

/// Draws the band with its bottom at `bottom` and gives its top, which is
/// `bottom` when there is nothing to show.
pub(super) unsafe fn draw(
    gfx: &Gfx,
    gpu: &Gpu,
    pal: &Palette,
    b: &Band,
    cx: f32,
    cw: f32,
    bottom: f32,
) -> f32 {
    let rows = rows(b);
    if rows.is_empty() {
        return bottom;
    }
    let top = bottom - 4.0 - LINE * rows.len() as f32;
    let warn = Color::rgb(0xE8A33A).mix(pal.fg, 0.2);
    for (i, row) in rows.iter().enumerate() {
        let y = top + LINE * i as f32;
        let x = cx + PAD;
        let w = (cw - 2.0 * PAD).max(0.0);
        let lead_w = render::wrapped(gpu, &gfx.small, &row.lead, w)
            .map(|l| render::text_size(&l).0)
            .unwrap_or(0.0)
            .min(w);
        let (lead_c, rest_c) = match row.kind {
            Lead::Quest => (pal.faint, pal.caret()),
            Lead::File | Lead::More => (warn, pal.dim),
        };
        gfx.text_in(
            &row.lead,
            &gfx.small,
            &Rect::new(x, y, lead_w + 1.0, LINE),
            lead_c,
        );
        let rest = Rect::new(x + lead_w, y, (w - lead_w).max(0.0), LINE);
        gfx.text_in(&row.rest, &gfx.small, &rest, rest_c);
    }
    top
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn others_are_named_as_the_mod_names_them() {
        assert_eq!(others(&names(&["A"])), "A");
        assert_eq!(others(&names(&["A", "B"])), "A and B");
        assert_eq!(others(&names(&["A", "B", "C"])), "A, B and C");
    }

    #[test]
    fn a_band_keeps_the_file_name_and_drops_files_no_one_else_has() {
        let b = Band::new(
            Some("Fix the API".into()),
            vec![
                ("crates\\api\\src\\lib.rs".into(), names(&["Other"])),
                ("src/gone.rs".into(), vec![]),
            ],
        );
        let rows = rows(&b);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].rest, "Fix the API");
        assert_eq!(rows[1].lead, "lib.rs");
        assert_eq!(rows[1].rest, " also changed by Other, not committed");
    }

    #[test]
    fn nothing_to_say_is_no_band() {
        assert!(rows(&Band::new(Some("  ".into()), vec![])).is_empty());
    }

    #[test]
    fn many_shared_files_fold_into_one_line() {
        let shared = (0..6)
            .map(|i| (format!("f{i}.rs"), names(&["X"])))
            .collect();
        let rows = rows(&Band::new(None, shared));
        assert_eq!(rows.len(), MOST);
        assert_eq!(rows[MOST - 1].lead, "3 more files");
    }
}
