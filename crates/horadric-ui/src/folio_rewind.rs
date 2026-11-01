//! Where a Folio's conversation is rewound to, shared by both faces: the
//! turn an undo drops, read from the end of its transcript, and the flags
//! that start claude again there. Windows keeps the rest in
//! `app::folio_tile`, the Mac in `mac::folio`.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use horadric_core::folio::{rewind_point, Rewind};

/// How much of a transcript's end is read for the turn to undo, before
/// the whole of it: a long session's runs to many megabytes, and its last
/// prompt is near the end.
const TAIL: u64 = 4 << 20;

/// The flags that resume a conversation at `r`, as a new one when it
/// forks. A first turn undone leaves nothing to resume, so it has none.
pub fn flags(r: &Rewind, fork: bool) -> Vec<String> {
    match (&r.keep, fork) {
        (Some(keep), false) => vec![
            "--resume-session-at".into(),
            keep.clone(),
            "--resume-drops-turn".into(),
            r.drop.clone(),
        ],
        // Without a dropped turn named, the CLI cuts the conversation at
        // `keep` however many turns follow.
        (Some(keep), true) => vec![
            "--fork-session".into(),
            "--resume-session-at".into(),
            keep.clone(),
        ],
        (None, _) => Vec::new(),
    }
}

/// What the page says once a fork from before `prompt` has started
/// again, with how putting back its files went, if it did.
pub fn fork_line(prompt: &str, files: Option<&Result<(), String>>) -> String {
    let mut said: String = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if said.chars().count() > 60 {
        said = said.chars().take(59).collect::<String>() + "\u{2026}";
    }
    let files = match files {
        Some(Ok(())) => "Its files went back with it.".to_string(),
        Some(Err(e)) => format!("Its files were left as they are: {e}"),
        None => "Its files were left as they are.".to_string(),
    };
    format!(
        "Rewound to before \u{201c}{said}\u{201d}. The conversation as it was is kept in History. {files}"
    )
}

/// The end of the file at `path`, from the first whole line after
/// `TAIL` bytes from its end, or all of it with `whole`.
pub fn read_transcript(path: &Path, whole: bool) -> Option<(String, bool)> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let from = if whole { 0 } else { len.saturating_sub(TAIL) };
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if from == 0 {
        return Some((text, true));
    }
    let start = text.find('\n').map_or(text.len(), |i| i + 1);
    Some((text[start..].to_string(), false))
}

/// The rewind of the last turn of `conversation`, counting back from
/// `head` when it was rewound there before.
pub fn find_rewind(path: &Path, head: Option<&str>) -> Option<Rewind> {
    let (tail, whole) = read_transcript(path, false)?;
    if let Some(r) = rewind_point(&tail, head) {
        return Some(r);
    }
    if whole {
        return None;
    }
    rewind_point(&read_transcript(path, true)?.0, head)
}
