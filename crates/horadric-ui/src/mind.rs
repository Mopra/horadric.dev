//! The mind firing: what each recall found, kept a little while so the
//! Arcane Sanctuary's mind lights it. The memory's thread tells it of a
//! recall ([`fire`]); the pane asks it what is lit at a moment ([`shine`]).
//!
//! A recall is drawn as a thought: its best hit flares first, then sparks
//! run from it along the synapses, one hop at a time, to each other hit,
//! which flares as a spark reaches it, and its neighbours flicker after.
//! Between recalls a few faint sparks wander the links, so the mind looks
//! awake.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

use crate::sky::Light;

/// How long a spark takes over one synapse, in seconds.
const HOP: f64 = 0.16;
/// How long a hit glows once reached, and its neighbours after it.
const FLARE: f64 = 5.0;
const ECHO: f64 = 2.0;
/// How strong a neighbour's flicker is, against its hit's.
const ECHO_PEAK: f32 = 0.35;
/// The most hops a spark walks; a hit further than this is leapt to.
const FARTHEST: usize = 12;
/// The most recalls of a project kept.
const MOST: usize = 8;
/// The thought's colour, and the fainter one of the wandering sparks.
const THOUGHT: u32 = 0x5FD3FF;
const IDLE: u32 = 0x35708A;
/// A wandering spark starts this often, in seconds, and takes this long.
const IDLE_EVERY: f64 = 0.35;
const IDLE_LIFE: f64 = 0.9;

struct Firing {
    ids: Vec<String>,
    at: f64,
}

fn firings() -> &'static Mutex<HashMap<String, VecDeque<Firing>>> {
    static ALL: OnceLock<Mutex<HashMap<String, VecDeque<Firing>>>> = OnceLock::new();
    ALL.get_or_init(Default::default)
}

/// A recall in the project `project` (the memory's key of it) found
/// `ids`, best first.
pub fn fire(project: &str, ids: &[String]) {
    let Ok(mut all) = firings().lock() else {
        return;
    };
    let kept = all.entry(project.to_string()).or_default();
    kept.push_back(Firing {
        ids: ids.to_vec(),
        at: crate::glows::now(),
    });
    while kept.len() > MOST {
        kept.pop_front();
    }
}

/// Whether a recall in `project` still fires at `now`.
pub fn firing(project: &str, now: f64) -> bool {
    firings()
        .lock()
        .ok()
        .and_then(|all| {
            let last = all.get(project)?.back()?;
            Some(now - last.at < FARTHEST as f64 * HOP + FLARE)
        })
        .unwrap_or(false)
}

/// What the recalls in `project` light at `now`, in a mind whose nodes
/// are found by `index` and linked as `next` says.
pub fn shine(
    project: &str,
    index: &HashMap<String, usize>,
    next: &[Vec<usize>],
    links: &[(usize, usize)],
    now: f64,
) -> Light {
    let mut light = Light::default();
    wander(links, now, &mut light);
    let Ok(all) = firings().lock() else {
        return light;
    };
    for f in all.get(project).into_iter().flatten() {
        let hits: Vec<usize> = f
            .ids
            .iter()
            .filter_map(|id| index.get(id).copied())
            .collect();
        thought(&hits, next, now - f.at, &mut light);
    }
    light
}

/// One recall's thought, `age` seconds in.
fn thought(hits: &[usize], next: &[Vec<usize>], age: f64, light: &mut Light) {
    let Some(&first) = hits.first() else {
        return;
    };
    if age < 0.0 {
        return;
    }
    let routes = routes(first, next);
    for (rank, &hit) in hits.iter().enumerate() {
        let path = route(&routes, first, hit);
        let reached = (path.len().saturating_sub(1)) as f64 * HOP;
        for (k, pair) in path.windows(2).enumerate() {
            let t = (age - k as f64 * HOP) / HOP;
            if (0.0..1.0).contains(&t) {
                light.sparks.push((pair[0], pair[1], THOUGHT, t as f32));
            }
        }
        let since = age - reached;
        if since < 0.0 {
            continue;
        }
        // The best hits shine brightest.
        let peak = 1.0 / (1.0 + rank as f32 * 0.25);
        let s = fade(since, FLARE) * peak;
        brighten(light, hit, s);
        let e = fade(since, ECHO) * peak * ECHO_PEAK;
        for &n in &next[hit] {
            brighten(light, n, e);
        }
    }
}

/// From full at 0 down to nothing at `life`, quickly at first.
fn fade(since: f64, life: f64) -> f32 {
    let left = (1.0 - since / life).max(0.0);
    (left * left) as f32
}

fn brighten(light: &mut Light, node: usize, strength: f32) {
    if strength <= 0.01 {
        return;
    }
    let lit = light.lit.entry(node).or_insert((THOUGHT, 0.0));
    lit.1 = lit.1.max(strength);
}

/// The step back toward `from` of every node within reach of it, by a
/// walk outward over the synapses.
fn routes(from: usize, next: &[Vec<usize>]) -> HashMap<usize, usize> {
    let mut back = HashMap::from([(from, from)]);
    let mut queue = VecDeque::from([(from, 0)]);
    while let Some((at, hops)) = queue.pop_front() {
        if hops == FARTHEST {
            continue;
        }
        for &n in &next[at] {
            if !back.contains_key(&n) {
                back.insert(n, at);
                queue.push_back((n, hops + 1));
            }
        }
    }
    back
}

/// The nodes from `from` to `to` over the synapses, or the leap straight
/// there when none reach it.
fn route(back: &HashMap<usize, usize>, from: usize, to: usize) -> Vec<usize> {
    if !back.contains_key(&to) {
        return vec![from, to];
    }
    let mut path = vec![to];
    let mut at = to;
    while at != from {
        at = back[&at];
        path.push(at);
    }
    path.reverse();
    path
}

/// A few faint sparks along links picked by the clock, the same at the
/// same moment so a redraw does not jump.
fn wander(links: &[(usize, usize)], now: f64, light: &mut Light) {
    if links.is_empty() {
        return;
    }
    let newest = (now / IDLE_EVERY).floor() as i64;
    let alive = (IDLE_LIFE / IDLE_EVERY).ceil() as i64;
    for slot in (newest - alive + 1)..=newest {
        let t = (now - slot as f64 * IDLE_EVERY) / IDLE_LIFE;
        if !(0.0..1.0).contains(&t) {
            continue;
        }
        let h = (slot as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 17;
        let (a, b) = links[(h % links.len() as u64) as usize];
        let (a, b) = if h & 1 == 0 { (a, b) } else { (b, a) };
        light.sparks.push((a, b, IDLE, t as f32));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A chain 0 - 1 - 2 - 3, and 4 alone.
    fn chain() -> Vec<Vec<usize>> {
        vec![vec![1], vec![0, 2], vec![1, 3], vec![2], vec![]]
    }

    #[test]
    fn a_spark_walks_the_synapses_hop_by_hop() {
        let next = chain();
        let mut light = Light::default();
        // Halfway over the second hop toward hit 3.
        thought(&[0, 3], &next, HOP * 1.5, &mut light);
        assert_eq!(light.sparks.len(), 1);
        let (a, b, _, t) = light.sparks[0];
        assert_eq!((a, b), (1, 2));
        assert!((t - 0.5).abs() < 1e-3);
        // The first hit already flares; the far one is not reached yet.
        assert!(light.lit.contains_key(&0));
        assert!(!light.lit.contains_key(&3));
    }

    #[test]
    fn a_hit_flares_when_reached_and_its_neighbours_flicker() {
        let next = chain();
        let mut light = Light::default();
        thought(&[0, 3], &next, HOP * 3.0 + 0.01, &mut light);
        let hit = light.lit[&3].1;
        let echo = light.lit[&2].1;
        assert!(hit > echo && echo > 0.0);
    }

    #[test]
    fn an_unreachable_hit_is_leapt_to() {
        let back = routes(0, &chain());
        assert_eq!(route(&back, 0, 4), vec![0, 4]);
        assert_eq!(route(&back, 0, 2), vec![0, 1, 2]);
    }

    #[test]
    fn the_wandering_sparks_are_the_same_at_the_same_moment() {
        let links = [(0, 1), (1, 2), (2, 3)];
        let (mut a, mut b) = (Light::default(), Light::default());
        wander(&links, 12.34, &mut a);
        wander(&links, 12.34, &mut b);
        assert_eq!(a, b);
        assert!(!a.sparks.is_empty());
    }
}
