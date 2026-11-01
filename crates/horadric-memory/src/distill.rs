//! Distilling: a note kept from passages of dialogue names them as its
//! sources, and is true from when the earliest of them was said.
//!
//! Transcripts are a record of what was said then, not of what is true
//! now; facts are notes. The kept text is never rewritten: a note is added
//! beside it, in the open, pointing at the passages it came from.
//!
//! The day's dialogue is handed to whoever distills it by `horadric
//! distill`, which an errand the human arms runs once a day: Warriv's
//! daily look back did it until the rounds went (2026-10-30), and a
//! session popping up on its own was what the human did not want.

use std::io;
use std::path::{Path, PathBuf};

use horadric_core::lore;

use crate::store::{Chunk, ChunkId, Kind, Month, Store};

/// How long before the last distilling a passage stored since may have
/// been said and still be read: a turn that ran past it, or a backlog
/// still being read, is read late, but a backfill of old transcripts is
/// no day's news.
pub const LATE: u64 = 24 * 60 * 60;

/// The most characters of dialogue handed over at once, about 40k
/// tokens: a busy day says more, and the newest is what is kept.
pub const MOST_CHARS: usize = 160_000;

/// What a conversation that is not the project's own talk holds:
/// Warriv's wakes, about the quest log, and the distilling itself, which
/// would otherwise distill its own reading.
const NOT_THE_PROJECT: [&str; 2] = ["You: Horadric woke you for this:", "horadric distill"];

/// The day's dialogue as it is handed over.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Day {
    /// Every passage, oldest first, each headed by its id.
    pub text: String,
    /// How many passages it holds.
    pub passages: usize,
    /// How many older ones did not fit.
    pub left_out: usize,
    /// How far the store was read: where the next distilling starts.
    pub end: Option<ChunkId>,
}

/// The dialogue of `project` not yet distilled, oldest first, without
/// the conversations that are not the project's own talk
/// ([`NOT_THE_PROJECT`]). That is what was stored after `after`, where
/// the last distilling stopped reading, said no more than [`LATE`]
/// before `since`; with no `after`, what was said at `since` or later.
/// Picked by where it was stored rather than when it was said, so a
/// passage stored after a distilling that it was said before is not
/// missed by both.
pub fn said_since<'a>(
    chunks: impl Iterator<Item = (ChunkId, &'a Chunk)>,
    project: &str,
    after: Option<ChunkId>,
    since: u64,
) -> Vec<(ChunkId, &'a Chunk)> {
    let new = |id: ChunkId, c: &Chunk| match after {
        Some(a) => id > a && c.time >= since.saturating_sub(LATE),
        None => c.time >= since,
    };
    let mut said: Vec<(ChunkId, &Chunk)> = chunks
        .filter(|(id, c)| c.kind == Kind::Dialogue && c.project == project && new(*id, c))
        .collect();
    let aside: Vec<String> = said
        .iter()
        .filter(|(_, c)| NOT_THE_PROJECT.iter().any(|m| c.text.contains(m)))
        .filter_map(|(_, c)| c.session.clone())
        .collect();
    said.retain(|(_, c)| c.session.as_ref().is_none_or(|s| !aside.contains(s)));
    said.sort_by_key(|(id, c)| (c.time, *id));
    said
}

/// `passages` written out, the newest that fit in `most` characters,
/// oldest first. Each opens with a line naming its id, its session and
/// how long ago it was said.
pub fn write(passages: &[(ChunkId, &Chunk)], now: u64, most: usize) -> Day {
    let mut kept: Vec<String> = Vec::new();
    let mut size = 0;
    for (id, c) in passages.iter().rev() {
        let mut head = format!("## {id} (");
        if let Some(s) = &c.session {
            let short: String = s.chars().take(8).collect();
            head.push_str(&format!("session {short}, "));
        }
        head.push_str(&format!("{} ago)", lore::age(now.saturating_sub(c.time))));
        let one = format!("{head}\n{}\n", c.text.trim_end());
        let len = one.chars().count();
        if size + len > most {
            break;
        }
        size += len;
        kept.push(one);
    }
    kept.reverse();
    Day {
        text: kept.join("\n"),
        passages: kept.len(),
        left_out: passages.len() - kept.len(),
        end: None,
    }
}

/// The dialogue of `project` from the store in `dir` not yet distilled,
/// as [`said_since`] picks it, with where the store ends for the next
/// time. `after` is where the last distilling stopped.
pub fn day(
    dir: &Path,
    project: &str,
    after: Option<ChunkId>,
    since: u64,
    now: u64,
) -> io::Result<Day> {
    // A chunk is kept in the month it is stored, so no earlier month holds
    // one stored after `after`, and none said since `since`.
    let from = after.map_or(Month::of(since), |a| a.month);
    let store = Store::text_since(dir, from)?;
    let said = said_since(store.chunks(), project, after, since);
    let end = store.chunks().map(|(id, _)| id).max().max(after);
    Ok(Day {
        end,
        ..write(&said, now, MOST_CHARS)
    })
}

/// Where a project's day is written in the memory folder `dir`, and
/// beside it how far the store was read: `.next` for the day handed
/// over, `.mark` once it was distilled.
pub fn day_file(dir: &Path, project: &str, ext: &str) -> PathBuf {
    dir.join("day")
        .join(lore::file_name(project))
        .with_extension(ext)
}

/// The day of `project` not yet distilled, written to its file for the
/// distilling session to read (a day is too long to print), with `.next`
/// keeping where the store ends. The first time, the last day.
pub fn hand_over(dir: &Path, project: &str, now: u64) -> io::Result<(Day, PathBuf)> {
    let after = std::fs::read_to_string(day_file(dir, project, "mark"))
        .ok()
        .and_then(|t| ChunkId::parse(t.trim()));
    let day = day(dir, project, after, now.saturating_sub(LATE), now)?;
    std::fs::create_dir_all(dir.join("day"))?;
    if let Some(end) = day.end {
        std::fs::write(day_file(dir, project, "next"), end.to_string())?;
    }
    let path = day_file(dir, project, "md");
    std::fs::write(&path, &day.text)?;
    Ok((day, path))
}

/// The day handed over was distilled, so the next starts where it
/// stopped reading. Says whether there was one to mark.
pub fn distilled(dir: &Path, project: &str) -> io::Result<bool> {
    let next = day_file(dir, project, "next");
    if !next.exists() {
        return Ok(false);
    }
    std::fs::rename(next, day_file(dir, project, "mark"))?;
    Ok(true)
}

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
    fn the_day_is_handed_over_once_and_leaves_out_warriv() {
        let dir = std::env::temp_dir().join(format!("horadric-day-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut store = Store::open(&dir).unwrap();
        store
            .add_all(
                &[
                    said("p", "s1", "You: why int8?", NOW - 600),
                    said("p", "w1", "You: Horadric woke you for this:\n- x", NOW - 500),
                    said("p", "w1", "Agent: noted", NOW - 400),
                    said("q", "s2", "You: other project", NOW - 300),
                    said("p", "s1", "Agent: a quarter of the size", NOW - 200),
                    said("p", "s3", "You: long ago", NOW - 3 * LATE),
                ],
                NOW,
            )
            .unwrap();
        let (day, path) = hand_over(&dir, "p", NOW).unwrap();
        assert_eq!(day.passages, 2);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("why int8") && text.contains("quarter"));
        assert!(!text.contains("noted") && !text.contains("other project"));
        assert!(text.find("why int8") < text.find("quarter"), "oldest first");
        assert!(distilled(&dir, "p").unwrap());
        assert!(!distilled(&dir, "p").unwrap(), "nothing handed over");
        store
            .add_all(&[said("p", "s1", "You: and later?", NOW + 60)], NOW + 60)
            .unwrap();
        let (day, _) = hand_over(&dir, "p", NOW + 120).unwrap();
        assert_eq!(day.passages, 1, "only what came after the mark");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_busy_day_keeps_the_newest() {
        let a = said("p", "s", &"x".repeat(50), NOW);
        let b = said("p", "s", "the newest", NOW);
        let id = |n| ChunkId {
            month: Month::of(NOW),
            n,
        };
        let day = write(&[(id(0), &a), (id(1), &b)], NOW, 60);
        assert_eq!((day.passages, day.left_out), (1, 1));
        assert!(day.text.contains("the newest"));
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
