//! Distilling: a note kept from passages of dialogue names them as its
//! sources, and is true from when the earliest of them was said.
//!
//! Transcripts are a record of what was said then, not of what is true
//! now; facts are notes. The kept text is never rewritten: a note is added
//! beside it, in the open, pointing at the passages it came from.

use std::path::Path;

use crate::store::{ChunkId, Store};

/// When the earliest of the passages `ids` was said, from the store in
/// `dir`, so a note distilled from them is true from then. An id that
/// names no passage is refused: a note must not point at nothing.
pub fn earliest(dir: &Path, ids: &[String]) -> Result<Option<u64>, String> {
    let parsed = ids
        .iter()
        .map(|s| {
            ChunkId::parse(s.trim())
                .ok_or_else(|| format!("{s} is no passage id; they read like 2026-10#12"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let Some(from) = parsed.iter().map(|id| id.month).min() else {
        return Ok(None);
    };
    let store = Store::text_since(dir, from).map_err(|e| e.to_string())?;
    let mut first: Option<u64> = None;
    for id in parsed {
        let c = store
            .chunk(id)
            .ok_or_else(|| format!("there is no passage {id} in the memory"))?;
        first = Some(first.map_or(c.time, |t| t.min(c.time)));
    }
    Ok(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Chunk, Kind};
    use std::fs;

    /// 2026-10-16 12:00 UTC.
    const NOW: u64 = 1_792_152_000;

    fn said(project: &str, session: &str, text: &str, time: u64) -> Chunk {
        Chunk {
            time,
            project: project.into(),
            kind: Kind::Dialogue,
            session: Some(session.into()),
            quest: None,
            commit: None,
            files: Vec::new(),
            note: None,
            text: text.into(),
        }
    }

    #[test]
    fn the_sources_are_read_from_the_store() {
        let dir = std::env::temp_dir().join(format!("horadric-distill-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut store = Store::open(&dir).unwrap();
        let ids = store
            .add_all(
                &[
                    said("p", "s1", "You: we keep int8", NOW - 600),
                    said("p", "s1", "Agent: kept", NOW - 300),
                ],
                NOW,
            )
            .unwrap();
        let names: Vec<String> = ids.iter().map(ChunkId::to_string).collect();
        assert_eq!(earliest(&dir, &names), Ok(Some(NOW - 600)));
        assert_eq!(earliest(&dir, &[]), Ok(None));
        assert!(earliest(&dir, &["2026-10#99".into()])
            .unwrap_err()
            .contains("no passage 2026-10#99"));
        assert!(earliest(&dir, &["twelve".into()]).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
