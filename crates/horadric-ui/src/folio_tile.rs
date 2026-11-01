//! A Folio's Undo last turn, which rewinds the conversation as well as
//! the files a terminal's puts back, and its Edit and Rewind on any
//! earlier prompt, which fork it there.
//!
//! claude cannot rewind a conversation it is running, so an undo lets it
//! end ([`crate::folio::rewind`]) and starts it again resumed at the line
//! before the turn's prompt (`--resume-session-at`, `--resume-drops-turn`,
//! hidden flags of the CLI that keep the undone turn in the transcript
//! but out of the conversation). The new host waits for the old one to
//! have gone, since both would take the same pipe.
//!
//! A fork goes the same way with `--fork-session` and no dropped turn:
//! the conversation carries on as a new one holding what came before the
//! prompt, and the old one stays whole in History. Its files go back
//! first, by the CLI's own `rewind_files` while it still runs, which can
//! reach any prompt it kept checkpoints for (see `App::open_chat`).

use std::cell::RefCell;
use std::collections::HashMap;

use horadric_core::folio::{prompt_point, Rewind};

use super::App;
use crate::folio_rewind::{find_rewind, fork_line, read_transcript};
use crate::toast::Kind;

/// A Folio being rewound, from the undo until it starts again.
struct Pending {
    conversation: String,
    rewind: Rewind,
    /// Its claude has ended, so it starts once its host is gone too.
    ended: bool,
    /// A fork at an earlier prompt rather than an undo.
    fork: Option<Fork>,
}

struct Fork {
    /// The prompt forked before, for the line that says so.
    prompt: String,
    /// What it goes on with: an edited prompt.
    resend: Option<String>,
    /// How putting back the files went, once it has.
    files: Option<Result<(), String>>,
}

thread_local! {
    static REWINDS: RefCell<HashMap<String, Pending>> = RefCell::new(HashMap::new());
}

/// The rewind Folio `id` starts with, taken once it is due, and whether
/// it forks: what [`App::start_folio`] adds to its flags and its tag.
pub(super) fn rewound(id: &str) -> Option<(Rewind, bool)> {
    REWINDS.with(|r| {
        let mut r = r.borrow_mut();
        r.get(id).filter(|p| p.ended)?;
        r.remove(id).map(|p| (p.rewind, p.fork.is_some()))
    })
}

pub(super) use crate::folio_rewind::flags;

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
                    fork: None,
                },
            )
        });
        true
    }

    /// Forks Folio `id` just before the `back`th prompt from the end,
    /// which reads `text`: its files go back first, then it ends and
    /// starts again as a new conversation from there, going on with
    /// `resend` when given.
    pub(super) fn folio_fork(&mut self, id: &str, back: usize, text: &str, resend: Option<String>) {
        if REWINDS.with(|r| r.borrow().contains_key(id)) {
            return;
        }
        let Some((conversation, head)) = crate::folio::rewind_from(id) else {
            self.toasts.show(
                Kind::Info,
                "Not rewound",
                "It works or asks something now. Rewind once it rests.",
            );
            return;
        };
        let path = self
            .dir_of(id)
            .and_then(|d| horadric_hooks::transcript::path(&d.to_string_lossy(), &conversation));
        let found = path
            .and_then(|p| read_transcript(&p, true))
            .and_then(|(jsonl, _)| prompt_point(&jsonl, head.as_deref(), back, text));
        let Some(rewind) = found else {
            self.toasts.show(
                Kind::Failed,
                "Not rewound",
                "That prompt was not found in the conversation Claude keeps.",
            );
            return;
        };
        let drop = rewind.drop.clone();
        REWINDS.with(|r| {
            r.borrow_mut().insert(
                id.to_string(),
                Pending {
                    conversation,
                    rewind,
                    ended: false,
                    fork: Some(Fork {
                        prompt: text.to_string(),
                        resend,
                        files: None,
                    }),
                },
            )
        });
        if !crate::folio::rewind_files(id, &drop) {
            REWINDS.with(|r| r.borrow_mut().remove(id));
            self.toasts.show(
                Kind::Info,
                "Not rewound",
                "It works or asks something now. Rewind once it rests.",
            );
        }
    }

    /// Folio `id`'s CLI put its files back for a fork, or said why not:
    /// either way the conversation forks now. Files it never kept
    /// checkpoints for, as a session started before they were on, stay.
    pub(super) fn folio_files_rewound(&mut self, id: &str, how: Result<(), String>) {
        let waiting = REWINDS.with(|r| {
            let mut r = r.borrow_mut();
            let f = r.get_mut(id)?.fork.as_mut().filter(|f| f.files.is_none())?;
            f.files = Some(how.clone());
            Some(())
        });
        if waiting.is_none() {
            return;
        }
        if !crate::folio::rewind(id) {
            REWINDS.with(|r| r.borrow_mut().remove(id));
            let said = match how {
                Ok(()) => {
                    "Its files went back, but it started working before the conversation could."
                }
                Err(_) => "It started working before the conversation could go back.",
            };
            self.toasts.show(Kind::Failed, "Not rewound", said);
        }
    }

    /// Folio `id`'s claude ended: true when it was for a rewind, which it
    /// then starts with, at once or at a tick once its host has gone.
    pub(super) fn folio_rewind_ended(&mut self, id: &str) -> bool {
        let pending = REWINDS.with(|r| r.borrow_mut().get_mut(id).map(|p| p.ended = true));
        if pending.is_none() {
            return false;
        }
        // Its pane stays meanwhile, and a start that fails pauses it.
        crate::folio::close(id);
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
        let fork = REWINDS.with(|r| {
            let mut r = r.borrow_mut();
            let f = r.get_mut(id)?.fork.as_mut()?;
            Some((fork_line(&f.prompt, f.files.as_ref()), f.resend.take()))
        });
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
        if let Some((said, resend)) = fork {
            crate::folio::greet(id, &said);
            if let Some(prompt) = resend {
                self.tasks.prompts.insert(id.to_string(), prompt);
            }
        }
        if let Err(e) = self.start_folio(Some(id.to_string()), dir, resume, &args, true) {
            REWINDS.with(|r| r.borrow_mut().remove(id));
            eprintln!("horadric: {id} did not start again after its undo: {e}");
            self.halt(id);
        }
    }
}
