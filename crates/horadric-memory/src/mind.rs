//! The mind: a project's memory as the Arcane Sanctuary draws it. Each
//! memory (a note, a quest's summary, a passage of dialogue, a piece of
//! code) is a node, and each is linked to the few nearest it in meaning,
//! so what is about the same thing pulls together in the layout the way
//! the files that call each other do. A recall lights the nodes it found.
//!
//! The store holds far more than a pane can draw, so the notes and quests
//! come first and the newest of the rest fill what is left.

use std::collections::{BTreeMap, HashMap};

use horadric_core::sanctuary::{Graph, Kind as NodeKind, Link, Node};

use crate::store::{Chunk, Kind, Store};

/// The most memories drawn: the layout and the nearest neighbours both
/// stay cheap at this size.
pub const MOST: usize = 1500;
/// The share of the drawing code may take, so a big code base does not
/// crowd the dialogue out.
const CODE_SHARE: f32 = 0.25;
/// How many of its nearest each memory is linked to.
const NEAREST: usize = 4;
/// The least cosine a link needs past a memory's first: below it two
/// passages only share a topic.
const LEAST: f32 = 0.35;
/// The least cosine even a memory's nearest needs to be linked at all.
const FLOOR: f32 = 0.2;

/// One memory as the mind takes it.
pub struct Memory<'a> {
    /// What a recall's hit names it by: `#12` for a note, `2026-10#5` for
    /// the rest.
    pub id: String,
    pub kind: Kind,
    pub time: u64,
    pub text: &'a str,
    pub vector: &'a [i8],
}

/// The memories of `project` that have a `tag` vector, the notes and
/// quests first, then the newest of the rest, at most `most`.
pub fn pick<'a>(store: &'a Store, tag: &str, project: &str, most: usize) -> Vec<Memory<'a>> {
    // A note kept again (edited, replaced) is one memory: its newest copy.
    let mut notes: BTreeMap<u64, Memory> = BTreeMap::new();
    let mut quests = Vec::new();
    let mut dialogue = Vec::new();
    let mut code = Vec::new();
    for (id, c) in store.chunks() {
        if c.project != project {
            continue;
        }
        let Some(vector) = store.vector(tag, id) else {
            continue;
        };
        let m = |id: String, c: &'a Chunk| Memory {
            id,
            kind: c.kind,
            time: c.time,
            text: &c.text,
            vector,
        };
        match (c.note, c.kind) {
            (Some(n), _) => {
                if notes.get(&n).is_none_or(|old| old.time <= c.time) {
                    notes.insert(n, m(format!("#{n}"), c));
                }
            }
            (None, Kind::Quest) => quests.push(m(id.to_string(), c)),
            (None, Kind::Code) => code.push(m(id.to_string(), c)),
            (None, _) => dialogue.push(m(id.to_string(), c)),
        }
    }
    let newest = |v: &mut Vec<Memory>| v.sort_by(|a, b| b.time.cmp(&a.time));
    newest(&mut quests);
    newest(&mut dialogue);
    newest(&mut code);
    let mut out: Vec<Memory> = notes.into_values().collect();
    newest(&mut out);
    out.truncate(most);
    out.extend(quests.into_iter().take(most - out.len()));
    let left = most - out.len();
    let code_most = ((left as f32 * CODE_SHARE) as usize).min(code.len());
    let talk = dialogue.len().min(left - code_most);
    // Code the dialogue did not need fills in after it.
    let code_most = code.len().min(left - talk);
    out.extend(dialogue.into_iter().take(talk));
    out.extend(code.into_iter().take(code_most));
    out
}

/// The mind's graph of `memories`: each linked to its nearest in meaning.
pub fn graph(memories: &[Memory]) -> Graph {
    let n = memories.len();
    // Each pair scored once: the cosine is the same both ways.
    let mut sims = vec![0f32; n * n];
    for i in 0..n {
        for j in i + 1..n {
            let c = cosine(memories[i].vector, memories[j].vector);
            sims[i * n + j] = c;
            sims[j * n + i] = c;
        }
    }
    let mut pairs: HashMap<(usize, usize), u32> = HashMap::new();
    let mut best: Vec<(usize, f32)> = Vec::with_capacity(n);
    for i in 0..n {
        best.clear();
        best.extend((0..n).filter(|&j| j != i).map(|j| (j, sims[i * n + j])));
        let k = NEAREST.min(best.len());
        if k == 0 {
            continue;
        }
        best.select_nth_unstable_by(k - 1, |a, b| b.1.total_cmp(&a.1));
        best[..k].sort_by(|a, b| b.1.total_cmp(&a.1));
        for (rank, &(j, cos)) in best[..k].iter().enumerate() {
            if cos < if rank == 0 { FLOOR } else { LEAST } {
                break;
            }
            pairs
                .entry((i.min(j), i.max(j)))
                .or_insert_with(|| weight(cos));
        }
    }
    let mut g = Graph {
        nodes: memories
            .iter()
            .map(|m| Node {
                id: m.id.clone(),
                kind: node_kind(m.kind),
                label: label(m.text),
                weight: 0,
            })
            .collect(),
        links: Vec::with_capacity(pairs.len()),
    };
    let mut pairs: Vec<_> = pairs.into_iter().collect();
    pairs.sort();
    for ((a, b), weight) in pairs {
        g.nodes[a].weight += weight;
        g.nodes[b].weight += weight;
        g.links.push(Link { a, b, weight });
    }
    g
}

/// The cosine of two kept vectors: they were unit length before they
/// were rounded to 127ths.
fn cosine(a: &[i8], b: &[i8]) -> f32 {
    let dot: i32 = a.iter().zip(b).map(|(&x, &y)| x as i32 * y as i32).sum();
    dot as f32 / (127.0 * 127.0)
}

/// A link's weight from its cosine: the closer, the shorter and stronger.
fn weight(cos: f32) -> u32 {
    1 + ((cos - FLOOR).max(0.0) * 20.0) as u32
}

fn node_kind(k: Kind) -> NodeKind {
    match k {
        Kind::Note | Kind::Other => NodeKind::Note,
        Kind::Quest => NodeKind::Quest,
        Kind::Dialogue => NodeKind::Dialogue,
        Kind::Code => NodeKind::Code,
    }
}

/// What the pane shows on hover: the first line with words in it.
fn label(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: &[f32]) -> Vec<i8> {
        let len = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| (x / len * 127.0).round() as i8).collect()
    }

    fn memory<'a>(id: &str, v: &'a [i8]) -> Memory<'a> {
        Memory {
            id: id.into(),
            kind: Kind::Dialogue,
            time: 0,
            text: "\n  first words\nmore",
            vector: v,
        }
    }

    #[test]
    fn memories_close_in_meaning_are_linked_and_far_ones_not() {
        let a = unit(&[1.0, 0.1, 0.0]);
        let b = unit(&[1.0, 0.0, 0.1]);
        let c = unit(&[0.0, 0.0, 1.0]);
        let g = graph(&[memory("a", &a), memory("b", &b), memory("c", &c)]);
        let linked = |x: usize, y: usize| g.links.iter().any(|l| (l.a, l.b) == (x, y));
        assert!(linked(0, 1));
        assert!(!linked(0, 2));
        assert_eq!(g.nodes[0].label, "first words");
        assert_eq!(
            g.nodes[0].weight,
            g.links
                .iter()
                .filter(|l| l.a == 0)
                .map(|l| l.weight)
                .sum::<u32>()
        );
    }

    #[test]
    fn a_pair_is_linked_once_and_the_closer_weighs_more() {
        let a = unit(&[1.0, 0.0]);
        let b = unit(&[1.0, 0.05]);
        let c = unit(&[1.0, 0.8]);
        let g = graph(&[memory("a", &a), memory("b", &b), memory("c", &c)]);
        let ab = g.links.iter().filter(|l| (l.a, l.b) == (0, 1)).count();
        assert_eq!(ab, 1);
        let w = |x, y| {
            g.links
                .iter()
                .find(|l| (l.a, l.b) == (x, y))
                .unwrap()
                .weight
        };
        assert!(w(0, 1) > w(0, 2));
    }
}
