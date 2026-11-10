//! Driving a session by its id, whatever kind it is. Quests, Warriv,
//! runewords, the usage window and a handoff once typed bytes into a
//! terminal; a Folio has none, so each act here says what it means and
//! each kind carries it out its own way.

use std::time::{Instant, SystemTime};

use horadric_core::runeword::{folio_keys, FolioKey};
use horadric_core::{HookEvent, SavedSession};

use super::App;

impl App {
    /// Whether session `id` runs here: a terminal whose program has not
    /// exited, or a Folio whose claude.exe runs.
    pub(super) fn runs_here(&self, id: &str) -> bool {
        match self.consoles.get(id) {
            Some(c) => c.exit_code().is_none(),
            None => self.folios.contains_key(id) && crate::folio::running(id),
        }
    }

    /// Sends `text` to session `id` as the human would type it and press
    /// Enter. A terminal gets the text now and its Enter a moment later, so
    /// the agent takes it as typed, not pasted; a Folio gets a prompt, or a
    /// switch for `/model`. False when nothing here can take it.
    pub(super) fn send_prompt(&mut self, id: &str, text: &str) -> bool {
        if self.folios.contains_key(id) {
            return crate::folio::prompt(id, text);
        }
        let Some(c) = self.consoles.get(id).filter(|c| c.exit_code().is_none()) else {
            return false;
        };
        c.write(text.as_bytes().to_vec());
        self.tasks.enters.push((id.to_string(), Instant::now()));
        true
    }

    /// Sends a command such as `/model opus` to session `id` at once, its
    /// Enter with it: the usage window's switches, typed only once the
    /// session is free, where waiting for the Enter would let another
    /// keystroke in between.
    pub(super) fn send_command(&mut self, id: &str, command: &str) -> bool {
        if self.folios.contains_key(id) {
            return crate::folio::prompt(id, command);
        }
        let Some(c) = self.consoles.get(id).filter(|c| c.exit_code().is_none()) else {
            return false;
        };
        c.write(format!("{command}\r").into_bytes());
        true
    }

    /// Stops session `id`'s turn: Esc to a terminal, as the human would, an
    /// interrupt request to a Folio. False when there is nothing to stop.
    pub(super) fn interrupt(&self, id: &str) -> bool {
        if self.folios.contains_key(id) {
            return crate::folio::stop(id);
        }
        let Some(c) = self.consoles.get(id) else {
            return false;
        };
        c.write(b"\x1b".to_vec());
        true
    }

    /// A runeword's `keys` step for Folio `id`, which has no prompt line to
    /// type into: the lines and stops the keys add up to, all at once,
    /// where a terminal takes the keys a moment apart. False when `id` is
    /// no Folio running here.
    pub(super) fn keys_to_folio(&mut self, id: &str, pieces: &[Vec<u8>]) -> bool {
        if !self.folios.contains_key(id) || !crate::folio::running(id) {
            return false;
        }
        for key in folio_keys(pieces) {
            match key {
                FolioKey::Line(line) => {
                    crate::folio::prompt(id, &line);
                }
                FolioKey::Stop => {
                    crate::folio::stop(id);
                }
                FolioKey::Approve => {
                    crate::folio::approve(id);
                }
            }
        }
        true
    }

    /// Ends session `id`'s agent but keeps its tile, paused, for a click to
    /// resume: a terminal's program is killed and its exit pauses it, and a
    /// Folio's claude.exe is closed and its tile paused here, since no exit
    /// comes back for one closed on purpose. Whether an agent was running
    /// and is stopped now.
    pub(super) fn halt(&mut self, id: &str) -> bool {
        if let Some(c) = self.consoles.get(id) {
            let running = c.exit_code().is_none();
            if running {
                c.kill();
            }
            return running;
        }
        if self.folios.remove(id).is_none() {
            return false;
        }
        crate::folio::close(id);
        let args = self.folio_args.remove(id).unwrap_or_default();
        if let Ok(mut r) = self.shared.registry.lock() {
            if let Some(s) = r.get(id) {
                let saved = SavedSession {
                    chat: true,
                    ..SavedSession::from_session(s, args, false)
                };
                self.paused.insert(id.to_string(), saved);
                r.apply(
                    id,
                    &HookEvent::synthetic(HookEvent::PAUSE),
                    SystemTime::now(),
                );
            }
        }
        self.reconcile(true);
        true
    }
}
