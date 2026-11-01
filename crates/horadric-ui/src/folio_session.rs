//! What a Folio is to the app, apart from how it is drawn: the tag its
//! host keeps, the state its tile hears, and reading a host's backlog
//! back into a [`Folio`]. Shared by the Windows Folio (`folio`) and the
//! Mac's (`mac::folio`).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use horadric_core::folio::backlog::{self, Kept};
use horadric_core::folio::Folio;
use horadric_core::usage::Status;
use horadric_core::{HookEvent, Phase, WaitReason};
use horadric_pty::headless_host::{Event, HostInfo};
use serde::{Deserialize, Serialize};

/// What a Folio's host keeps for the app, handed back on every attach, so
/// the app after a reload knows what the session was.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    /// The project the session is in, by key.
    pub key: String,
    /// What its tile is called.
    pub name: String,
    /// The conversation it carries on, when it was resumed.
    #[serde(default)]
    pub resume: Option<String>,
    /// How much of that conversation's transcript there was as it was
    /// resumed, in bytes. The host's backlog holds the rest, so a reattach
    /// reads the transcript up to here and no turn shows twice.
    #[serde(default)]
    pub history: u64,
    /// The line of that transcript it was resumed at, when an undo
    /// rewound it there: what comes after in the file was undone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

/// What the app hears of a Folio, for its tile's phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub busy: bool,
    /// An ask waits on the human.
    pub asking: bool,
    pub ended: bool,
    /// Ended, and not to go on in a terminal, start again or wait out an
    /// account switch: claude.exe crashed, was killed or lost its host.
    /// The tile pauses, so a click resumes the conversation.
    pub gone: bool,
    /// The host's replay is in, so this is where the session stands.
    pub live: bool,
    /// The last turn ended, its reply not yet followed by a prompt.
    pub finished: bool,
    /// How many background tasks run, once the CLI has said.
    pub background: Option<usize>,
    /// The agents among them, by id with their type, once the CLI has
    /// said: the tile's sparks before the mod's roster speaks, or with no
    /// mod at all. The mod runs headless and its roster wins.
    pub agents: Option<Vec<(String, String)>>,
    /// How the last turn failed, as a `StopFailure` names it, with what
    /// Claude said of it: the tile shows the error, and a usage limit
    /// switches account.
    pub failed: Option<(String, String)>,
    /// The tool and input of the oldest ask, so the tile and its
    /// notification say what is asked.
    pub ask: Option<(String, serde_json::Value)>,
    /// The model, and the context's tokens used of its window after the
    /// last turn: a headless CLI runs no status line to tell the tile.
    pub model: Option<String>,
    pub context: Option<(u64, u64)>,
    /// What the CLI called the conversation, for the tile until a hook
    /// reads the same from the transcript.
    pub title: Option<String>,
}

impl State {
    /// The hook event that brings a tile in `phase` to where this Folio
    /// stands, as a terminal's hooks would have, if any. `waits_on` is the
    /// tool the tile last heard asked about. `resync` is a reattached
    /// tile's first word once the replay is in.
    pub fn event(&self, phase: &Phase, waits_on: Option<&str>, resync: bool) -> Option<HookEvent> {
        let tool = self.ask.as_ref().map(|(t, _)| t.as_str());
        if self.ended {
            None
        } else if self.asking {
            // A second ask after the first is answered is news too.
            (!phase.asks() || waits_on != tool).then(|| HookEvent {
                tool_name: tool.map(str::to_string),
                tool_input: self.ask.as_ref().map(|(_, i)| i.clone()),
                ..HookEvent::synthetic("PermissionRequest")
            })
        } else if self.busy {
            (!phase.mid_turn() || phase.is_waiting()).then(|| HookEvent::synthetic("PreToolUse"))
        } else if let Some((kind, said)) = &self.failed {
            let now = Phase::Waiting(WaitReason::Error(kind.clone()));
            (*phase != now).then(|| HookEvent {
                error_type: Some(kind.clone()),
                error_message: (!said.is_empty()).then(|| said.clone()),
                ..HookEvent::synthetic("StopFailure")
            })
        } else if resync && self.finished {
            // A turn that ended while no app watched is Done, as it would
            // have become.
            (*phase == Phase::Idle).then(|| HookEvent::synthetic("Stop"))
        } else if *phase == Phase::Working || phase.asks() {
            Some(HookEvent::synthetic("Stop"))
        } else if phase.is_waiting() {
            // An error this process never had, such as the one a login
            // or a new account just cleared: nothing to read as Done.
            Some(HookEvent::synthetic("Interrupt"))
        } else {
            None
        }
    }

    /// What the tile shows of the model and its context, as a terminal's
    /// status line says it. None until the CLI has said either.
    pub fn status(&self) -> Option<Status> {
        let context = (self.context)
            .filter(|&(_, limit)| limit > 0)
            .map(|(used, limit)| (used as f32 * 100.0 / limit as f32).min(100.0));
        (self.model.is_some() || context.is_some()).then(|| Status {
            model: self.model.clone(),
            context,
            ..Default::default()
        })
    }
}

/// Where the transcript of `conversation` held in `dir` is, and how long
/// it is now.
pub fn history_file(dir: &Path, conversation: &str) -> Option<(PathBuf, u64)> {
    let path = horadric_hooks::transcript::path(&dir.to_string_lossy(), conversation)?;
    let len = std::fs::metadata(&path).ok()?.len();
    Some((path, len))
}

/// A reattached Folio's events. The backlog is read into a Folio right
/// here, on the reader thread: a long session's runs to tens of MB, and
/// reading several on the UI thread stalled the window at start. The
/// Folio goes in `slot`, which [`drain`] waits for, and what follows the
/// backlog goes on to `live` as it comes.
pub fn catch_up(
    info: &HostInfo,
    slot: Arc<Mutex<Option<Folio>>>,
    mut live: impl FnMut(Event) + Send + 'static,
) -> impl FnMut(Event) + Send + 'static {
    let tag: Tag = serde_json::from_str(&info.tag).unwrap_or_default();
    let dir = info.cwd.clone();
    let conversation = info.conversation.clone();
    let mut kept: Option<Vec<Kept>> = Some(Vec::new());
    let mut dropped = false;
    move |event| {
        let Some(lines) = kept.as_mut() else {
            return live(event);
        };
        match event {
            Event::Out(l) => lines.push(Kept::Out(l)),
            Event::Sent(l) => lines.push(Kept::Sent(l)),
            Event::Dropped(n) => {
                eprintln!("horadric: a Folio's host lost its {n} oldest lines");
                dropped = true;
            }
            Event::Live | Event::End(_) => {
                let lines = kept.take().unwrap_or_default();
                let folio = read_backlog(&dir, &tag, conversation.as_deref(), &lines, dropped);
                if let Ok(mut s) = slot.lock() {
                    *s = Some(folio);
                }
                live(event);
            }
        }
    }
}

/// The Folio a host's backlog `lines` show, with the transcript before
/// them: up to where a resumed session began, or, when the backlog lost
/// its start (`dropped`), the whole of it, of the conversation the host
/// names or else the backlog does.
pub fn read_backlog(
    dir: &Path,
    tag: &Tag,
    conversation: Option<&str>,
    lines: &[Kept],
    dropped: bool,
) -> Folio {
    let read = |path: &Path, limit: u64| {
        let mut bytes = Vec::new();
        if let Ok(file) = std::fs::File::open(path) {
            let _ = file.take(limit).read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let conversation = (conversation.map(str::to_string))
        .or_else(|| backlog::conversation_in(lines))
        .or_else(|| tag.resume.clone());
    // After a `/clear` in the backlog what shows is all in it anyway.
    if dropped && !backlog::cleared_in(lines) {
        if let Some((path, len)) = (conversation.as_deref()).and_then(|c| history_file(dir, c)) {
            // An undo's point holds until the conversation goes on past it.
            let at = tag.at.as_deref().filter(|_| len <= tag.history);
            match Folio::from_transcript_and_backlog(&read(&path, len), at, lines) {
                Some(f) => return f,
                None => {
                    eprintln!("horadric: a Folio's backlog does not line up with its transcript")
                }
            }
        }
    }
    let mut folio = match tag.resume.as_deref().and_then(|c| history_file(dir, c)) {
        Some((path, _)) => Folio::from_transcript_at(&read(&path, tag.history), tag.at.as_deref()),
        None => Folio::new(),
    };
    folio.feed_kept(lines);
    if folio.session_id.is_none() {
        folio.session_id = conversation;
    }
    folio
}

/// What the error line says when claude.exe ended before it started, with
/// the last of what it wrote to stderr.
pub fn start_failure(tail: &[String]) -> String {
    let last: Vec<&str> = (tail.iter().rev())
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .take(3)
        .collect();
    if last.is_empty() {
        return "The session ended before it started.".to_string();
    }
    let said: Vec<&str> = last.into_iter().rev().collect();
    format!("The session ended before it started: {}", said.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_rest() -> State {
        State {
            busy: false,
            asking: false,
            ended: false,
            gone: false,
            live: true,
            finished: true,
            background: None,
            agents: None,
            failed: None,
            ask: None,
            model: None,
            context: None,
            title: None,
        }
    }

    fn name(e: Option<HookEvent>) -> Option<String> {
        e.map(|e| e.hook_event_name)
    }

    #[test]
    fn a_failed_turn_reaches_the_tile_as_its_error() {
        let failed = State {
            failed: Some(("rate_limit".into(), "You've hit your limit".into())),
            ..at_rest()
        };
        let e = failed.event(&Phase::Working, None, false).unwrap();
        assert_eq!(e.hook_event_name, "StopFailure");
        assert_eq!(e.error_type.as_deref(), Some("rate_limit"));
        assert_eq!(e.error_message.as_deref(), Some("You've hit your limit"));
        let limited = Phase::Waiting(WaitReason::Error("rate_limit".into()));
        // Said once, and never worn down to Done while it stands.
        assert_eq!(name(failed.event(&limited, None, false)), None);
        assert_eq!(
            name(at_rest().event(&Phase::Working, None, false)).as_deref(),
            Some("Stop")
        );
        // Gone with a new process, it is not read as a finished turn.
        let fresh = State {
            finished: false,
            ..at_rest()
        };
        assert_eq!(
            name(fresh.event(&limited, None, false)).as_deref(),
            Some("Interrupt")
        );
        let busy = State {
            busy: true,
            ..failed.clone()
        };
        assert_eq!(
            name(busy.event(&limited, None, false)).as_deref(),
            Some("PreToolUse")
        );
    }

    #[test]
    fn an_ask_names_its_tool_and_a_new_one_is_said_again() {
        let input = serde_json::json!({"command": "ls"});
        let asking = State {
            asking: true,
            finished: false,
            ask: Some(("Bash".into(), input.clone())),
            ..at_rest()
        };
        let e = asking.event(&Phase::Working, None, false).unwrap();
        assert_eq!(e.hook_event_name, "PermissionRequest");
        assert_eq!(e.tool_name.as_deref(), Some("Bash"));
        assert_eq!(e.tool_input, Some(input));
        let perm = Phase::Waiting(WaitReason::Permission);
        assert_eq!(name(asking.event(&perm, Some("Bash"), false)), None);
        assert!(asking
            .event(&perm, Some("AskUserQuestion"), false)
            .is_some());
    }

    #[test]
    fn the_context_meter_reads_the_last_turns_usage() {
        assert_eq!(at_rest().status(), None);
        let s = State {
            model: Some("claude-opus-5-5".into()),
            context: Some((50_000, 200_000)),
            ..at_rest()
        };
        let st = s.status().unwrap();
        assert_eq!(st.context, Some(25.0));
        assert_eq!(st.model.as_deref(), Some("claude-opus-5-5"));
        let unmeasured = State {
            context: Some((5, 0)),
            ..at_rest()
        };
        assert_eq!(unmeasured.status(), None);
    }

    #[test]
    fn start_failure_quotes_the_last_lines_of_stderr() {
        assert_eq!(start_failure(&[]), "The session ended before it started.");
        let tail: Vec<String> = ["a", "", "b", "c", "d  "].map(String::from).to_vec();
        assert_eq!(
            start_failure(&tail),
            "The session ended before it started: b c d"
        );
    }

    #[test]
    fn a_tag_reads_back_and_an_old_one_still_reads() {
        let tag = Tag {
            key: "c:/p".into(),
            name: "p".into(),
            resume: Some("abc".into()),
            history: 42,
            at: None,
        };
        let json = serde_json::to_string(&tag).unwrap();
        assert_eq!(serde_json::from_str::<Tag>(&json).unwrap(), tag);
        let bare: Tag = serde_json::from_str(r#"{"key":"k","name":"n"}"#).unwrap();
        assert_eq!((bare.resume, bare.history), (None, 0));
    }
}
