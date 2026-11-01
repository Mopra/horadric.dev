//! A brother's sessions held as tiles in this machine's registry, for a
//! client that keeps every brother's tile in that brother's own cluster
//! (the Mac), and a handed off tile in the place it left ([`Places`],
//! [`hold_placed`]): a cluster's tiles stand in the order they were made,
//! so the place is the cluster and the time. The Windows client keeps a
//! tile's slot in its own order too (`crates/horadric-ui/src/far.rs`);
//! what the two share is [`crate::afar`] and [`went`].

use std::collections::{HashMap, HashSet};
use std::time::SystemTime;

use crate::afar::{self, Entry, Left, OnBrother};
use crate::Registry;

/// Tiles that went a moment ago, kept for the same session to come back
/// into on another brother, and sessions that came a moment before the
/// tile they were went.
#[derive(Debug, Default)]
pub struct Places {
    /// By the session's id on its brother.
    pub left: HashMap<String, Left>,
    /// By the session's id on its brother: its tile here, and when.
    pub arrived: HashMap<String, (String, SystemTime)>,
}

impl Places {
    fn fresh(&mut self, now: SystemTime) {
        let fresh = |at: SystemTime| now.duration_since(at).map_or(true, |d| d < afar::LEFT_FOR);
        self.left.retain(|_, l| fresh(l.at));
        self.arrived.retain(|_, (_, at)| fresh(*at));
    }

    /// Tile `id`, this machine's own session, goes because it was handed
    /// to a brother: its place is kept for it to come back as that
    /// brother's, `home` being its cluster.
    pub fn leave(&mut self, r: &mut Registry, id: &str, home: &str, now: SystemTime) {
        let Some(created) = r.get(id).map(|s| s.created) else {
            return;
        };
        let left = Left {
            home: home.to_string(),
            slot: None,
            created,
            at: now,
        };
        went(r, &mut self.arrived, &mut self.left, id, id, left);
    }
}

/// A tile to put in a cluster's order: its id, the cluster, the place it
/// takes when nothing stands there, and the tile whose place it takes.
pub type Placed = (String, String, Option<usize>, String);

/// Tile `gone` (session `remote`) went, leaving `left`. When the same
/// session arrived on another brother a moment before, that tile takes
/// its place now, and says where to put it; else the place is kept for
/// it to arrive.
pub fn went(
    r: &mut Registry,
    arrived: &mut HashMap<String, (String, SystemTime)>,
    kept: &mut HashMap<String, Left>,
    remote: &str,
    gone: &str,
    left: Left,
) -> Option<Placed> {
    let early = arrived
        .get(remote)
        .filter(|(id, _)| id != gone)
        .map(|(id, ..)| id.clone());
    if let Some(s) = early.as_deref().and_then(|id| r.get_mut(id)) {
        afar::take_place(s, &left);
        arrived.remove(remote);
        let home = left.home.clone();
        return Some((s.id.clone(), home, left.slot, gone.to_string()));
    }
    kept.insert(remote.to_string(), left);
    None
}

/// [`hold_list`], with a session new on brother `id` taking the place of
/// its tile that went a moment ago (a handoff), and a tile that goes
/// leaving its place for it. `home` is the cluster a tile stands in.
pub fn hold_placed(
    r: &mut Registry,
    id: &str,
    name: &str,
    list: &[Entry],
    now: SystemTime,
    places: &mut Places,
    home: impl Fn(&crate::Session) -> String,
) -> (bool, Vec<String>) {
    places.fresh(now);
    let listed: HashSet<String> = list.iter().map(|e| afar::local_id(id, &e.id)).collect();
    let before: Vec<(String, String, Left)> = r
        .all()
        .filter(|s| s.brother.as_ref().is_some_and(|b| b.id == id))
        .filter_map(|s| {
            let remote = afar::remote_id(&s.id, id)?.to_string();
            let left = Left {
                home: home(s),
                slot: None,
                created: s.created,
                at: now,
            };
            Some((s.id.clone(), remote, left))
        })
        .collect();
    let new: Vec<&Entry> = list
        .iter()
        .filter(|e| r.get(&afar::local_id(id, &e.id)).is_none())
        .collect();
    let (mut changed, gone) = hold_list(r, id, name, list, now);
    for e in new {
        let local = afar::local_id(id, &e.id);
        match afar::place_of(&places.left, &e.id, now).cloned() {
            Some(l) => {
                if let Some(s) = r.get_mut(&local) {
                    afar::take_place(s, &l);
                    changed = true;
                }
                places.left.remove(&e.id);
            }
            // Its tile elsewhere may go a moment later: the handoff's two
            // lists cross.
            None => {
                places.arrived.insert(e.id.clone(), (local, now));
            }
        }
    }
    for (local, remote, left) in before {
        if !listed.contains(&local) {
            changed |= went(
                r,
                &mut places.arrived,
                &mut places.left,
                &remote,
                &local,
                left,
            )
            .is_some();
        }
    }
    (changed, gone)
}

/// Makes `r` hold exactly the sessions brother `id`, shown as `name`,
/// lists. A tile that did not change is left as it stood, so it keeps its
/// age and its unread mark. Answers whether any tile changed, and the ids
/// here of the tiles that went.
pub fn hold_list(
    r: &mut Registry,
    id: &str,
    name: &str,
    list: &[Entry],
    now: SystemTime,
) -> (bool, Vec<String>) {
    let mut changed = false;
    let listed: HashSet<String> = list.iter().map(|e| afar::local_id(id, &e.id)).collect();
    for e in list {
        let local = afar::local_id(id, &e.id);
        let was = r.get(&local).cloned();
        let on = OnBrother {
            id: id.to_string(),
            name: name.to_string(),
            project: e.project.clone(),
            reach: true,
            home: None,
        };
        let s = afar::session(&on, e, was.as_ref(), now);
        if was.as_ref() != Some(&s) {
            r.add(s);
            changed = true;
        }
    }
    let gone: Vec<String> = r
        .all()
        .filter(|s| s.brother.as_ref().is_some_and(|b| b.id == id))
        .filter(|s| !listed.contains(&s.id))
        .map(|s| s.id.clone())
        .collect();
    for local in &gone {
        r.remove(local);
        changed = true;
    }
    (changed, gone)
}

/// Sets each brother's tiles to how it is reached now, from `lines`: by
/// brother key, None while it answers, else the line its tiles show
/// dimmed instead of their own. True when any tile changed.
pub fn set_reach(r: &mut Registry, lines: &HashMap<String, Option<String>>) -> bool {
    let ids: Vec<String> = r
        .all()
        .filter(|s| s.brother.is_some())
        .map(|s| s.id.clone())
        .collect();
    let mut changed = false;
    for id in ids {
        let Some(s) = r.get_mut(&id) else {
            continue;
        };
        let Some(b) = s.brother.as_mut() else {
            continue;
        };
        let Some(line) = lines.get(&b.id) else {
            continue;
        };
        let reach = line.is_none();
        if b.reach != reach {
            b.reach = reach;
            changed = true;
        }
        if let Some(line) = line {
            if &s.last_line != line {
                s.last_line = line.clone();
                changed = true;
            }
        }
    }
    changed
}

/// Drops every tile of brother `id`: it was cast out, and nothing of it is
/// shown again. Answers the ids here of the tiles that went.
pub fn drop_brother(r: &mut Registry, id: &str) -> Vec<String> {
    let gone: Vec<String> = r
        .all()
        .filter(|s| s.brother.as_ref().is_some_and(|b| b.id == id))
        .map(|s| s.id.clone())
        .collect();
    for local in &gone {
        r.remove(local);
    }
    gone
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Phase, Session};

    #[test]
    fn a_handed_off_tile_comes_back_where_it_stood() {
        let t0 = SystemTime::now();
        let mut r = Registry::default();
        let mut mine = Session::new("s1", "fix", "/code/app");
        mine.created = t0 - std::time::Duration::from_secs(3600);
        let created = mine.created;
        r.add(mine);
        let mut places = Places::default();
        places.leave(&mut r, "s1", "/code/app", t0);
        r.remove("s1");
        let home = |s: &Session| {
            s.brother
                .as_ref()
                .map_or_else(|| s.cwd.clone(), |b| b.cluster())
        };
        hold_placed(
            &mut r,
            "B",
            "Beta",
            &[entry("s1", Phase::Idle)],
            t0,
            &mut places,
            home,
        );
        let s = r.get(&afar::local_id("B", "s1")).unwrap();
        assert_eq!(s.brother.as_ref().unwrap().cluster(), "/code/app");
        assert_eq!(s.created, created);
        // Handed on from Beta to Gamma, whose list comes first: the new
        // tile waits, then takes the place once Beta's goes.
        hold_placed(
            &mut r,
            "G",
            "Gamma",
            &[entry("s1", Phase::Idle)],
            t0,
            &mut places,
            home,
        );
        hold_placed(&mut r, "B", "Beta", &[], t0, &mut places, home);
        let g = r.get(&afar::local_id("G", "s1")).unwrap();
        assert_eq!(g.brother.as_ref().unwrap().cluster(), "/code/app");
        assert_eq!(g.created, created);
    }

    fn entry(id: &str, phase: Phase) -> Entry {
        Entry {
            id: id.into(),
            name: format!("{id} name"),
            project: "app".into(),
            phase,
            line: "working".into(),
            paused: false,
            since: Some(1_000),
            agent: None,
            talked: true,
            worktree: false,
        }
    }

    #[test]
    fn a_list_is_held_exactly_and_local_tiles_are_left_alone() {
        let now = SystemTime::now();
        let mut r = Registry::default();
        r.add(Session::new("mine", "mine", "/code/app"));
        let (changed, gone) = hold_list(
            &mut r,
            "B",
            "Beta",
            &[entry("s1", Phase::Idle), entry("s2", Phase::Idle)],
            now,
        );
        assert!(changed);
        assert!(gone.is_empty());
        let s1 = r.get(&afar::local_id("B", "s1")).expect("s1 is held");
        assert_eq!(
            s1.brother.as_ref().unwrap().cluster(),
            afar::key("B", "app")
        );

        let (changed, _) = hold_list(
            &mut r,
            "B",
            "Beta",
            &[entry("s1", Phase::Idle), entry("s2", Phase::Idle)],
            now,
        );
        assert!(!changed, "the same list changes nothing");

        let (changed, gone) = hold_list(&mut r, "B", "Beta", &[entry("s1", Phase::Idle)], now);
        assert!(changed);
        assert_eq!(gone, vec![afar::local_id("B", "s2")]);
        assert!(r.get("mine").is_some(), "a local tile is never a brother's");
        assert_eq!(drop_brother(&mut r, "B"), vec![afar::local_id("B", "s1")]);
        assert!(r.get("mine").is_some());
    }

    #[test]
    fn an_unreached_brother_dims_its_tiles_with_why() {
        let now = SystemTime::now();
        let mut r = Registry::default();
        hold_list(&mut r, "B", "Beta", &[entry("s1", Phase::Idle)], now);
        let mut lines = HashMap::new();
        lines.insert("B".to_string(), Some("Beta asleep".to_string()));
        assert!(set_reach(&mut r, &lines));
        let s = r.get(&afar::local_id("B", "s1")).unwrap();
        assert!(!s.brother.as_ref().unwrap().reach);
        assert_eq!(s.last_line, "Beta asleep");
        assert!(!set_reach(&mut r, &lines), "nothing more changes");
        lines.insert("B".to_string(), None);
        assert!(set_reach(&mut r, &lines));
        assert!(
            r.get(&afar::local_id("B", "s1"))
                .unwrap()
                .brother
                .as_ref()
                .unwrap()
                .reach
        );
    }
}
