//! A hireling's conversation, read only: the transcript a subagent writes
//! as it works, drawn as a Folio in a pane beside its session. See
//! docs/plan/hirelings.md.
//!
//! There is no process behind it: the router's timer looks at the file
//! now and then, and when it grew, reads it whole again. A subagent's
//! transcript is small next to a session's, so that is cheaper than it
//! sounds, and it keeps the blocks exactly as a replay makes them. The
//! composer is not drawn: nothing typed here could reach it.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use horadric_core::folio::Folio;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use super::{redraw, router, with, Chat, CHATS};

/// The router's timer that looks at the files of the hirelings shown.
pub(super) const WATCH_TIMER: usize = 2;
const WATCH_MS: u32 = 1000;
/// A transcript untouched this long, with no word from its meta file,
/// is taken as finished.
const STILL: Duration = Duration::from_secs(30);

pub(super) struct Watch {
    path: PathBuf,
    len: u64,
    /// What the pane's header calls it.
    pub(super) title: String,
    pub(super) working: bool,
}

/// Shows the transcript at `path` as the read only Folio `id`, in the
/// colour of project `key`, replacing what `id` showed before.
pub fn watch(id: &str, key: &str, dir: &Path, path: &Path, title: &str) {
    let mut chat = Chat::new(key, dir);
    chat.live = true;
    chat.watch = Some(Watch {
        path: path.to_path_buf(),
        len: u64::MAX,
        title: title.to_string(),
        working: false,
    });
    let view = with(id, |c| c.view.take()).flatten();
    chat.view = view;
    CHATS.with(|c| c.borrow_mut().insert(id.to_string(), chat));
    look(id);
    let router = router();
    if router != 0 {
        unsafe {
            SetTimer(
                Some(HWND(router as *mut std::ffi::c_void)),
                WATCH_TIMER,
                WATCH_MS,
                None,
            );
        }
    }
}

/// Whether Folio `id` is a hireling's, read only.
pub fn is_watch(id: &str) -> bool {
    with(id, |c| c.watch.is_some()).unwrap_or(false)
}

/// What the pane's header says after its name.
pub fn watch_detail(id: &str) -> String {
    with(id, |c| {
        let w = c.watch.as_ref()?;
        let state = if w.working { "working" } else { "done" };
        Some(format!("{state} \u{b7} read only \u{b7} Esc to close"))
    })
    .flatten()
    .unwrap_or_default()
}

/// What the pane's header calls hireling pane `id`.
pub fn watch_title(id: &str) -> Option<String> {
    with(id, |c| c.watch.as_ref().map(|w| w.title.clone())).flatten()
}

/// The router's timer: every hireling shown reads its file again if it
/// grew. The timer stops once none is left.
pub(super) fn tick(router: HWND) {
    let ids: Vec<String> = CHATS.with(|c| {
        c.try_borrow()
            .map(|chats| {
                chats
                    .iter()
                    .filter(|(_, c)| c.watch.is_some())
                    .map(|(id, _)| id.clone())
                    .collect()
            })
            .unwrap_or_default()
    });
    if ids.is_empty() {
        unsafe {
            let _ = KillTimer(Some(router), WATCH_TIMER);
        }
    }
    for id in ids {
        look(&id);
    }
}

/// Reads hireling `id`'s transcript again when it changed, keeping where
/// the human scrolled to and what they unfolded.
fn look(id: &str) {
    let Some(path) = with(id, |c| c.watch.as_ref().map(|w| w.path.clone())).flatten() else {
        return;
    };
    let meta = std::fs::metadata(&path).ok();
    let len = meta.as_ref().map_or(0, |m| m.len());
    let touched = meta.and_then(|m| m.modified().ok());
    let working = match finished(&path) {
        Some(done) => !done,
        None => {
            touched.is_some_and(|t| SystemTime::now().duration_since(t).is_ok_and(|d| d < STILL))
        }
    };
    let grew = with(id, |c| c.watch.as_ref().is_some_and(|w| w.len != len)).unwrap_or(false);
    let text = if grew {
        std::fs::read(&path)
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    } else {
        None
    };
    let moved = with(id, |c| {
        let Some(w) = c.watch.as_mut() else {
            return false;
        };
        let was = w.working;
        w.working = working;
        w.len = len;
        if let Some(text) = &text {
            let mut f = Folio::from_sidechain(text);
            f.busy = working;
            c.folio = f;
        } else {
            c.folio.busy = working;
        }
        c.note_busy();
        text.is_some() || was != working
    })
    .unwrap_or(false);
    if moved {
        redraw(id);
    }
}

/// What the meta file beside a subagent's transcript says of whether it
/// finished, when there is one.
fn finished(transcript: &Path) -> Option<bool> {
    let meta = transcript.with_extension("meta.json");
    let text = std::fs::read_to_string(meta).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v["finished"].as_bool()
}
