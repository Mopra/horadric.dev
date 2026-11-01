//! A brother's sessions held as tiles in this machine's registry, for a
//! client that keeps every brother's tile in that brother's own cluster
//! (the Mac). The Windows client also moves a handed off tile into the
//! place it left (`crates/horadric-ui/src/far.rs`); what the two share is
//! [`crate::afar`].

use std::collections::{HashMap, HashSet};
use std::time::SystemTime;

use crate::afar::{self, Entry, OnBrother};
use crate::Registry;

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
