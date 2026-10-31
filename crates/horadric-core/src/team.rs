//! Teams: a WorkOS organization as a brotherhood of several people (see
//! docs/plan/the-brotherhood.md, "Teams"). A teammate's machine is a
//! brother like the human's own, joined with the same six digit code, but
//! what it gets by default is less: it watches this machine's sessions and
//! never types into them, and it gets no lore, since the lore holds the
//! human's conversations word for word.
//!
//! Which machines are a teammate's is the relay's to say, as it says which
//! are the human's own: it checks each login's WorkOS token, and only it
//! sees both. A machine never decides it from what a brother says of
//! itself. The relay marks a teammate's machine `"kin":"team"` in its
//! `brothers` list and on the `join` and `link` frames; a machine it does
//! not mark is the human's own, which is all a relay without teams ever
//! lists, so nothing changes until the relay groups by organization.

use std::collections::BTreeSet;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Whose a machine the relay lists is, beside this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kin {
    /// Signed in as the same person.
    Own,
    /// Signed in as someone else of the same organization.
    Team,
}

impl Kin {
    /// What a relay frame or a `brothers` entry says. Anything but `team`
    /// is the human's own, as every relay before teams means it.
    pub fn of(v: &Value) -> Kin {
        if v.get("kin").and_then(Value::as_str) == Some("team") {
            Kin::Team
        } else {
            Kin::Own
        }
    }

    /// Whether a brother may control this machine once joined, until the
    /// human switches it. A teammate typing into the human's shells is a
    /// thing they turn on, never a default.
    pub fn controls_at_join(self) -> bool {
        self == Kin::Own
    }

    /// Whether this machine sends a brother its lore for a project. A
    /// teammate gets none until conversations can be held back from what
    /// is sent: notes and quest summaries are meant for others, a
    /// conversation is not.
    pub fn shares_lore(self) -> bool {
        self == Kin::Own
    }
}

/// The ids of the machines in a relay `brothers` list it marks as a
/// teammate's.
pub fn team_in_list(v: &Value) -> Vec<String> {
    v.get("brothers")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|b| Kin::of(b) == Kin::Team)
                .filter_map(|b| Some(b.get("id")?.as_str()?.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

const FILE: &str = "team.json";

/// The machines this one learnt are teammates', kept in `team.json` beside
/// `standing.json`, so a link that comes up while the relay's list has not
/// come yet is still treated as a teammate's. Never shrinks by itself: a
/// relay that stops saying `team` (an older one after a rollback) must not
/// hand a teammate the human's own rights.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Team {
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub mates: BTreeSet<String>,
}

impl Team {
    /// The file in `dir`, empty when it is missing or unreadable.
    pub fn load(dir: &Path) -> Team {
        std::fs::read_to_string(dir.join(FILE))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        crate::portal::write_private(&dir.join(FILE), text.as_bytes())
    }

    /// Learns the machines the relay marks as teammates'. True when one is
    /// new, so it is worth saving.
    pub fn learn(&mut self, ids: impl IntoIterator<Item = String>) -> bool {
        let before = self.mates.len();
        self.mates.extend(ids);
        self.mates.len() != before
    }

    pub fn kin(&self, id: &str) -> Kin {
        if self.mates.contains(id) {
            Kin::Team
        } else {
            Kin::Own
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_a_machine_the_relay_marks_team_is_a_teammates() {
        assert_eq!(
            Kin::of(&json!({"join": 3, "id": "a", "kin": "team"})),
            Kin::Team
        );
        assert_eq!(Kin::of(&json!({"join": 3, "id": "a"})), Kin::Own);
        assert_eq!(
            Kin::of(&json!({"join": 3, "id": "a", "kin": "own"})),
            Kin::Own
        );
        let list = json!({"brothers": [
            {"id": "a", "name": "Desk"},
            {"id": "b", "name": "Ann's laptop", "kin": "team"},
        ]});
        assert_eq!(team_in_list(&list), vec!["b"]);
    }

    #[test]
    fn a_teammate_watches_and_gets_no_lore_by_default() {
        assert!(Kin::Own.controls_at_join());
        assert!(Kin::Own.shares_lore());
        assert!(!Kin::Team.controls_at_join());
        assert!(!Kin::Team.shares_lore());
    }

    #[test]
    fn a_teammate_stays_one_when_the_relay_stops_saying_so() {
        let mut t = Team::default();
        assert!(t.learn(["b".to_string()]));
        assert!(!t.learn(["b".to_string()]));
        assert!(!t.learn(Vec::new()));
        assert_eq!(t.kin("b"), Kin::Team);
        assert_eq!(t.kin("a"), Kin::Own);
        let dir = std::env::temp_dir().join(format!("horadric-team-{}", std::process::id()));
        t.save(&dir).unwrap();
        assert_eq!(Team::load(&dir), t);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
