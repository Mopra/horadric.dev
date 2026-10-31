//! What a Folio's tile does that a terminal's does through its keys:
//! undoing the last turn rewinds the conversation as well as the files,
//! and a Folio that has to stop for a brother pauses as a chat.
//!
//! claude cannot rewind a conversation it is running, so an undo lets it
//! end ([`crate::folio::rewind`]) and starts it again resumed at the line
//! before the turn's prompt (`--resume-session-at`, `--resume-drops-turn`,
//! hidden flags of the CLI that keep the undone turn in the transcript
//! but out of the conversation). The new host waits for the old one to
//! have gone, since both would take the same pipe.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::SystemTime;

use horadric_core::folio::{rewind_point, Rewind};
use horadric_core::saved::SavedSession;
use horadric_core::HookEvent;

use super::App;
use crate::toast::Kind;

/// How much of a transcript's end is read for the turn to undo, before
/// the whole of it: a long session's runs to many megabytes, and its last
/// prompt is near the end.
const TAIL: u64 = 4 << 20;

/// A Folio being rewound, from the undo until it starts again.
struct Pending {
    conversation: String,
    rewind: Rewind,
    /// Its claude has ended, so it starts once its host is gone too.
    ended: bool,
}

thread_local! {
    static REWINDS: RefCell<HashMap<String, Pending>> = RefCell::new(HashMap::new());
}

/// The rewind Folio `id` starts with, taken once it is due: what
/// [`App::start_folio`] adds to its flags and its tag.
pub(super) fn rewound(id: &str) -> Option<Rewind> {
    REWINDS.with(|r| {
        let mut r = r.borrow_mut();
        r.get(id).filter(|p| p.ended)?;
        r.remove(id).map(|p| p.rewind)
    })
}

/// The flags that resume a conversation at `r`. A first turn undone
/// leaves nothing to resume, so it has none.
pub(super) fn flags(r: &Rewind) -> Vec<String> {
    match &r.keep {
        Some(keep) => vec![
            "--resume-session-at".into(),
            keep.clone(),
            "--resume-drops-turn".into(),
            r.drop.clone(),
        ],
        None => Vec::new(),
    }
}

/// The end of the file at `path`, from the first whole line after
/// `TAIL` bytes from its end, or all of it with `whole`.
fn read_transcript(path: &Path, whole: bool) -> Option<(String, bool)> {
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
fn find_rewind(path: &Path, head: Option<&str>) -> Option<Rewind> {
    let (tail, whole) = read_transcript(path, false)?;
    if let Some(r) = rewind_point(&tail, head) {
        return Some(r);
    }
    if whole {
        return None;
    }
    rewind_point(&read_transcript(path, true)?.0, head)
}

impl App {
    /// Starts rewinding Folio `id`'s conversation to before its last turn,
    /// and says whether it did, so its files are put back with it.
    pub(super) fn folio_rewind(&self, id: &str) -> bool {
        let Some((conversation, head)) = crate::folio::rewind_from(id) else {
            self.toasts.show(
                Kind::Info,
                "Not undone",
                "It works or asks something now. Undo once it rests.",
            );
            return false;
        };
        let path = self
            .dir_of(id)
            .and_then(|d| horadric_hooks::transcript::path(&d.to_string_lossy(), &conversation));
        let Some(rewind) = path.and_then(|p| find_rewind(&p, head.as_deref())) else {
            self.toasts.show(
                Kind::Failed,
                "Not undone",
                "No turn to undo was found in the conversation.",
            );
            return false;
        };
        if !crate::folio::rewind(id) {
            return false;
        }
        REWINDS.with(|r| {
            r.borrow_mut().insert(
                id.to_string(),
                Pending {
                    conversation,
                    rewind,
                    ended: false,
                },
            )
        });
        true
    }

    /// Folio `id`'s claude ended: true when it was for a rewind, which it
    /// then starts with, at once or at a tick once its host has gone.
    pub(super) fn folio_rewind_ended(&mut self, id: &str) -> bool {
        let pending = REWINDS.with(|r| r.borrow_mut().get_mut(id).map(|p| p.ended = true));
        if pending.is_none() {
            return false;
        }
        crate::folio::close(id);
        self.folios.remove(id);
        self.folio_rewind_start(id);
        true
    }

    /// Starts every rewound Folio whose old host has gone.
    pub(super) fn folio_rewinds_tick(&mut self) {
        let due: Vec<String> = REWINDS.with(|r| {
            (r.borrow().iter())
                .filter(|(_, p)| p.ended)
                .map(|(id, _)| id.clone())
                .collect()
        });
        for id in due {
            self.folio_rewind_start(&id);
        }
    }

    fn folio_rewind_start(&mut self, id: &str) {
        let hosts = horadric_pty::headless_host::Hosted::list(&horadric_hooks::instance());
        if hosts.iter().any(|h| h == id) {
            return;
        }
        let Some((conversation, fresh)) = REWINDS.with(|r| {
            (r.borrow().get(id)).map(|p| (p.conversation.clone(), p.rewind.keep.is_none()))
        }) else {
            return;
        };
        let Some(dir) = self.dir_of(id) else {
            REWINDS.with(|r| r.borrow_mut().remove(id));
            return;
        };
        // The first turn undone leaves no conversation: a new one starts.
        if fresh {
            if let Ok(mut r) = self.shared.registry.lock() {
                if let Some(s) = r.get_mut(id) {
                    s.claude_session_id = None;
                    s.prompted = false;
                }
            }
        }
        let args = self.folio_args.get(id).cloned().unwrap_or_default();
        let resume = (!fresh).then_some(conversation);
        if let Err(e) = self.start_folio(Some(id.to_string()), dir, resume, &args, true) {
            REWINDS.with(|r| r.borrow_mut().remove(id));
            eprintln!("horadric: {id} did not start again after its undo: {e}");
            self.pause_as_chat(id, args);
        }
    }

    /// Ends Folio `id`'s claude and keeps its tile paused, to resume as a
    /// chat, as a terminal killed for a handoff or a takeover is.
    pub(super) fn pause_folio(&mut self, id: &str) {
        if self.folios.remove(id).is_none() {
            return;
        }
        crate::folio::close(id);
        let args = self.folio_args.remove(id).unwrap_or_default();
        self.pause_as_chat(id, args);
        self.sync_stage();
    }

    fn pause_as_chat(&mut self, id: &str, args: Vec<String>) {
        if let Ok(mut r) = self.shared.registry.lock() {
            if let Some(s) = r.get(id) {
                let saved = SavedSession {
                    chat: true,
                    ..SavedSession::from_session(s, args, false)
                };
                self.paused.insert(id.to_string(), saved);
                r.apply(id, &HookEvent::synthetic(HookEvent::PAUSE), SystemTime::now());
            }
        }
        self.reconcile(true);
    }
}
