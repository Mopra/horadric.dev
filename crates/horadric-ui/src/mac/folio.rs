//! The Folio on the Mac: a headless Claude session's conversation as a
//! chat in its stage pane, not a terminal. The session behind it is the
//! Windows Folio's: claude in a host of its own speaking stream-json
//! ([`Hosted`]), read into the shared [`Folio`] model, its tile told how
//! it stands through [`State`]. Only the face differs.
//!
//! Windows lays the conversation out as cards it draws itself. The Mac
//! shows it in AppKit's own text views instead: the transcript as styled
//! paragraphs ([`crate::folio_text`]) in a read only text view, which
//! scrolls, selects and copies by itself, and a composer below it that
//! is an editable one, with input methods, undo and paste for free. A bar
//! between them says what Claude does and holds the buttons an ask needs,
//! and a menu at its start holds the rest (review, undo, rewind, the
//! terminal, MCP, images). The `/` and `!` commands, logins and the
//! rewinds are in [`super::folio_commands`], the dialogs that stand in
//! for Windows's cards in [`super::folio_forms`].
//!
//! The session lives here by id, not in its view, so it runs on while the
//! stage shows another project. A reload drops the client, which only
//! detaches, and [`reattach`] finds the session again with everything it
//! said replayed. Events come on the client's reader thread and reach the
//! main thread through the main queue.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use horadric_core::folio::{
    prompt_line, AskState, Block, FileEdit, Folio, ImageData, McpWant, Priority, Reply, SignInState,
};
use horadric_core::trust;
use horadric_pty::headless::Options;
use horadric_pty::headless_host::{Event, Hosted};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSAutoresizingMaskOptions, NSBezelStyle, NSButton, NSEventModifierFlags, NSFont,
    NSFontAttributeName, NSFontWeightRegular, NSForegroundColorAttributeName, NSLineBreakMode,
    NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSResponder, NSScrollView, NSText,
    NSTextDelegate, NSTextField, NSTextView, NSTextViewDelegate, NSView,
};
use objc2_foundation::{
    NSAttributedString, NSDictionary, NSMutableAttributedString, NSObject, NSPoint, NSRange,
    NSRect, NSSize, NSString,
};
use serde_json::Value;

use crate::folio_review::Review;
use crate::folio_session::{catch_up, history_file, start_failure, State, Tag};
use crate::folio_text::{self, Style};
use crate::layout::Rect;
use crate::theme::{self, Color};

use super::app::{self, Input};
use super::folio_commands as commands;
use super::paint::{cg_rect, ns_color, Painter};

/// The bar between the transcript and the composer.
const BAR_H: f64 = 30.0;
/// The composer, which scrolls past this many lines.
const COMPOSER_H: f64 = 76.0;
const PAD: f64 = 8.0;
const BUTTON_W: f64 = 112.0;
/// The menu button at the bar's start.
const MORE_W: f64 = 34.0;
/// How many buttons the bar can show at once.
const BUTTONS: usize = 4;
/// How long a host is waited on to go before the session starts again,
/// since the new one would take the same pipe.
const HOST_GONE_WAIT: Duration = Duration::from_secs(10);

/// What a bar button does now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Act {
    Allow(String),
    Always(String),
    Deny(String),
    /// A line comment on the edit ask's diff.
    Comment(String),
    Approve(String),
    KeepPlanning(String),
    Answer(String),
    /// An MCP server's form or page.
    Elicit(String),
    Stop,
    Trust,
    LogIn,
    OpenPage,
    SendCode,
    CancelLogIn,
    ClearImages,
}

impl Act {
    fn title(&self, comments: bool) -> &'static str {
        match self {
            Act::Allow(_) => "Allow",
            Act::Always(_) => "Always Allow",
            Act::Deny(_) if comments => "Send Comments",
            Act::Deny(_) => "Deny",
            Act::Comment(_) => "Comment\u{2026}",
            Act::Approve(_) => "Approve Plan",
            Act::KeepPlanning(_) => "Keep Planning",
            Act::Answer(_) => "Answer\u{2026}",
            Act::Elicit(_) => "Answer\u{2026}",
            Act::Stop => "Stop",
            Act::Trust => "Trust Folder",
            Act::LogIn => "Log In",
            Act::OpenPage => "Open Page",
            Act::SendCode => "Send Code",
            Act::CancelLogIn => "Cancel",
            Act::ClearImages => "Remove Images",
        }
    }
}

/// Why a session's claude is ending, so the same tile goes on.
#[derive(Debug, Clone)]
pub enum Leave {
    /// It goes on in a terminal.
    Terminal,
    /// It starts again as a Folio.
    Again(Again),
}

/// How a Folio starts again: after an undo, a rewind, a login, review
/// turned on, or a folder added.
#[derive(Debug, Clone, Default)]
pub struct Again {
    /// The conversation it carries on; None starts a new one, as an undo
    /// of the first turn does.
    pub resume: Option<String>,
    /// Flags for this start only, as a rewind's.
    pub flags: Vec<String>,
    /// The transcript line it was rewound to.
    pub at: Option<String>,
    /// Arguments it keeps from now on, as `/add-dir`'s folder.
    pub own: Vec<String>,
    /// What its page says once it has started again.
    pub said: Option<String>,
    /// Review stays as it was.
    pub review: bool,
}

/// A Folio whose claude has ended for the tile to go on: where and what
/// it was, and how it goes on.
pub struct Left {
    pub dir: PathBuf,
    pub name: String,
    pub conversation: Option<String>,
    pub leave: Leave,
}

/// What a Folio asks of the app, which alone holds the tiles.
pub enum Ask {
    Left(Left),
    /// Show this file on the stage, with the session's project.
    View(PathBuf),
    /// End the session, as `/exit`.
    Exit,
}

/// One Folio and the session behind it.
pub(super) struct Chat {
    pub(super) dir: PathBuf,
    pub(super) folio: Folio,
    /// None when it did not start, or once it was let go.
    pub(super) hosted: Option<Hosted>,
    /// Events from the reader thread.
    inbox: Arc<Mutex<VecDeque<Event>>>,
    /// A drain is queued, so a chatty stream posts once.
    posted: Arc<AtomicBool>,
    pub(super) ended: bool,
    pub(super) error: Option<String>,
    /// The host has sent everything it kept and goes on live.
    pub(super) live: bool,
    /// A resumed conversation's history, read on a thread of its own:
    /// filled once read, None when there is none to wait for.
    loading: Option<Arc<Mutex<Option<Folio>>>>,
    /// Waits for the human to trust its folder, with what starts it then.
    /// Claude Code asks before it runs a folder's hooks and MCP servers,
    /// but not headless, so the Folio asks for it.
    pending: Option<(Tag, Options)>,
    /// What the app was last told of it.
    told: Option<State>,
    pub(super) view: Option<Retained<FolioView>>,
    pub(super) tag: Tag,
    /// Every Edit and Write waits for the human, as a diff to comment on.
    pub(super) review: bool,
    /// The line comments on each edit ask, by request id.
    pub(super) reviews: HashMap<String, Review>,
    /// Images pasted or attached, sent with the next prompt.
    pub(super) images: Vec<ImageData>,
    /// One line the bar says while Claude rests, as a command's answer.
    pub(super) notice: Option<String>,
    /// Its claude ends for the tile to go on, and how.
    pub(super) leaving: Option<Leave>,
    /// The app was told it left: it is not a session that died.
    left: bool,
    /// The transcript line it was resumed at after an undo, which a
    /// further undo counts back from.
    pub(super) rewound: Option<String>,
    /// Said once its history is in.
    greeting: Option<String>,
    /// A fork waiting for its files to go back first.
    pub(super) fork: Option<commands::Fork>,
    busy_was: bool,
    /// A status ask for its MCP servers is due in a moment.
    mcp_soon: bool,
}

impl Chat {
    fn new(dir: &Path, tag: Tag) -> Chat {
        Chat {
            dir: dir.to_path_buf(),
            folio: Folio::new(),
            hosted: None,
            inbox: Arc::default(),
            posted: Arc::default(),
            ended: false,
            error: None,
            live: false,
            loading: None,
            pending: None,
            told: None,
            view: None,
            tag,
            review: false,
            reviews: HashMap::new(),
            images: Vec::new(),
            notice: None,
            leaving: None,
            left: false,
            rewound: None,
            greeting: None,
            fork: None,
            busy_was: false,
            mcp_soon: false,
        }
    }

    fn asking(&self) -> bool {
        !self.folio.pending_asks().is_empty() || !self.folio.pending_elicitations().is_empty()
    }

    /// It can take a prompt now: started, its replay and history in.
    pub(super) fn ready(&self) -> bool {
        self.hosted.is_some()
            && self.live
            && self.loading.is_none()
            && !self.ended
            && self.leaving.is_none()
    }

    /// Up, and neither working nor asking: what ending it to start again
    /// needs, or a turn would be cut short.
    pub(super) fn between_turns(&self) -> bool {
        self.ready() && !self.folio.busy && !self.asking() && self.fork.is_none()
    }

    fn state(&self) -> State {
        // Ending to go on in the same tile, it is not gone.
        let going_on = self.leaving.is_some() || self.left;
        State {
            busy: self.folio.busy,
            asking: self.asking(),
            ended: self.ended,
            gone: self.ended && !going_on,
            live: self.live,
            finished: self.folio.turn_finished(),
            background: (self.folio.background_heard).then_some(self.folio.background.len()),
            agents: (self.folio.background_heard).then(|| {
                (self.folio.background.iter())
                    .filter(|t| t.is_agent())
                    .map(|t| (t.task_id.clone(), t.agent_type.clone()))
                    .collect()
            }),
            failed: (self.folio.failed.clone()).map(|kind| (kind, self.folio.last_reply())),
            ask: self.folio.first_ask(),
            model: self.folio.model.clone(),
            context: (self.folio.context.as_ref()).map(|c| (c.used, c.limit)),
            title: self.folio.title.clone(),
        }
    }

    /// What the pane's header says beside the name.
    fn detail(&self) -> String {
        let state = if self.pending.is_some() {
            "waiting for trust"
        } else if self.leaving.is_some() || self.left {
            "starting again"
        } else if self.error.is_some() || self.ended {
            "ended"
        } else if self.folio.busy {
            "working"
        } else {
            "chat"
        };
        let review = if self.review { " \u{b7} review" } else { "" };
        match &self.folio.model {
            Some(m) => format!("{state} \u{b7} {m}{review}"),
            None => format!("{state}{review}"),
        }
    }

    /// What the bar says, and the buttons it shows, oldest ask first.
    fn bar(&mut self) -> (String, Vec<Act>) {
        if self.pending.is_some() {
            return (
                "Claude Code has not been told to trust this folder. Its hooks and MCP servers run once you do."
                    .into(),
                vec![Act::Trust],
            );
        }
        if self.leaving.is_some() || self.left {
            return ("Claude is starting again\u{2026}".into(), Vec::new());
        }
        if let Some(e) = &self.error {
            return (e.clone(), Vec::new());
        }
        if self.fork.is_some() {
            return ("Putting the files back\u{2026}".into(), Vec::new());
        }
        for b in &self.folio.blocks {
            match b {
                Block::Ask {
                    request_id,
                    tool,
                    input,
                    state: AskState::Waiting,
                    ..
                } if !request_id.is_empty() => {
                    let id = request_id.clone();
                    if let Some(e) = FileEdit::from_input(tool, input) {
                        let n = self.reviews.get(&id).map_or(0, |r| r.comments.len());
                        let said = match n {
                            0 => format!(
                                "Claude asks to change {}. Comment on its rows by number, or allow it.",
                                e.file_name()
                            ),
                            1 => "One comment. Send it for Claude to revise the edit.".into(),
                            n => format!("{n} comments. Send them for Claude to revise the edit."),
                        };
                        return (
                            said,
                            vec![
                                Act::Comment(id.clone()),
                                Act::Deny(id.clone()),
                                Act::Always(id.clone()),
                                Act::Allow(id),
                            ],
                        );
                    }
                    return (
                        format!("Claude asks to use {tool}."),
                        vec![
                            Act::Deny(id.clone()),
                            Act::Always(id.clone()),
                            Act::Allow(id),
                        ],
                    );
                }
                Block::Plan {
                    request_id,
                    state: AskState::Waiting,
                    ..
                } if !request_id.is_empty() => {
                    let id = request_id.clone();
                    return (
                        "Claude has a plan. Write below what to change before Keep Planning."
                            .into(),
                        vec![Act::KeepPlanning(id.clone()), Act::Approve(id)],
                    );
                }
                Block::Question {
                    request_id,
                    state: AskState::Waiting,
                    ..
                } if !request_id.is_empty() => {
                    return (
                        "Claude asks you something.".into(),
                        vec![Act::Answer(request_id.clone())],
                    );
                }
                Block::Elicitation(e) if e.state == AskState::Waiting => {
                    return (
                        format!("{} asks you something.", e.server),
                        vec![Act::Elicit(e.request_id.clone())],
                    );
                }
                _ => {}
            }
        }
        if let Some(card) = self.folio.sign_in_card() {
            match &card.state {
                SignInState::Needed | SignInState::Failed(_) | SignInState::Cancelled => {
                    return ("Claude needs you to log in.".into(), vec![Act::LogIn]);
                }
                SignInState::Starting => {
                    return ("Starting the login\u{2026}".into(), vec![Act::CancelLogIn]);
                }
                SignInState::Waiting => {
                    let mut acts = vec![Act::CancelLogIn];
                    if card.url.is_some() {
                        acts.push(Act::OpenPage);
                    }
                    if card.asks_code {
                        acts.push(Act::SendCode);
                    }
                    let said = match card.asks_code {
                        true => "Log in in your browser, then paste the code below.",
                        false => "Log in in your browser.",
                    };
                    return (said.into(), acts);
                }
                SignInState::Checking => {
                    return ("Checking the code\u{2026}".into(), vec![Act::CancelLogIn]);
                }
                SignInState::Done(_) => {}
            }
        }
        if self.ended {
            return (
                "The session ended. Click its tile to resume it.".into(),
                Vec::new(),
            );
        }
        if !self.live || self.loading.is_some() {
            return ("Reading the conversation\u{2026}".into(), Vec::new());
        }
        if self.folio.busy {
            let doing = self.folio.status.as_deref().unwrap_or("working");
            return (format!("Claude is {doing}\u{2026}"), vec![Act::Stop]);
        }
        if !self.images.is_empty() {
            let n = self.images.len();
            let said = match n {
                1 => "1 image goes with the next prompt.".to_string(),
                n => format!("{n} images go with the next prompt."),
            };
            return (said, vec![Act::ClearImages]);
        }
        if let Some(n) = &self.notice {
            return (n.clone(), Vec::new());
        }
        (
            "Return sends, Shift+Return starts a new line, Tab lists the / commands.".into(),
            Vec::new(),
        )
    }

    /// Writes what the model queued for the CLI, such as the answer to a
    /// request it cannot serve. Not while a reattach replays: an answer
    /// may still be on its way back.
    fn flush(&mut self) {
        if !self.live {
            return;
        }
        let lines = self.folio.take_outbox();
        if let Some(h) = &self.hosted {
            for line in lines {
                if let Err(e) = h.send_line(&line) {
                    eprintln!("horadric: folio could not answer the CLI: {e}");
                }
            }
        }
    }

    /// Writes `line` to claude. False, with the error shown, when it did
    /// not go.
    pub(super) fn send(&mut self, line: Option<String>) -> bool {
        let (Some(line), Some(h)) = (line, &self.hosted) else {
            return false;
        };
        match h.send_line(&line) {
            Ok(()) => true,
            Err(e) => {
                self.error = Some(format!("Claude did not take it: {e}"));
                false
            }
        }
    }

    /// Sends `text` as a prompt with the images waiting, at once when
    /// Claude rests, else queued behind the turn running now. A `!`
    /// command's output goes with it.
    pub(super) fn deliver(&mut self, text: &str) -> bool {
        let text = format!("{}{text}", self.folio.shell_context());
        let images = std::mem::take(&mut self.images);
        self.notice = None;
        if self.folio.busy {
            let line = self.folio.queue_prompt(&text, &images, Priority::Next);
            return self.send(Some(line));
        }
        let line = match images.is_empty() {
            true => {
                self.folio.push_prompt(&text);
                prompt_line(&text)
            }
            false => self.folio.push_prompt_with_images(&text, &images),
        };
        let went = self.send(Some(line));
        if !went {
            // No turn started, so none will end with a result.
            self.folio.end();
        }
        went
    }

    /// Lets its claude end so the tile goes on as `leave` says. Only
    /// between turns, or a turn would be cut short; one that ended goes
    /// on at once. False when it cannot now.
    pub(super) fn let_go(&mut self, id: &str, leave: Leave) -> bool {
        if self.leaving.is_some() || self.left {
            return false;
        }
        let leave = match leave {
            Leave::Again(again) => Leave::Again(Again {
                review: self.review,
                ..again
            }),
            terminal => terminal,
        };
        if self.ended || self.hosted.is_none() {
            self.leaving = Some(leave);
            self.tell_left(id);
            return true;
        }
        if !self.between_turns() {
            return false;
        }
        self.leaving = Some(leave);
        if let Some(h) = &self.hosted {
            h.close();
        }
        true
    }

    /// Tells the app this Folio has left, once its host has gone, since
    /// the next claude would take the same pipe.
    fn tell_left(&mut self, id: &str) {
        let Some(leave) = self.leaving.take() else {
            return;
        };
        self.left = true;
        let left = Left {
            dir: self.dir.clone(),
            name: self.tag.name.clone(),
            conversation: self.folio.session_id.clone().or(self.tag.resume.clone()),
            leave,
        };
        let id = id.to_string();
        // A host is known by its session's id; one that never started
        // needs no waiting for.
        let wait = self.hosted.is_some();
        // The waiting is off the main thread, the telling back on it.
        std::thread::spawn(move || {
            let start = std::time::Instant::now();
            while wait
                && start.elapsed() < HOST_GONE_WAIT
                && Hosted::list(&horadric_hooks::instance()).contains(&id)
            {
                std::thread::sleep(Duration::from_millis(100));
            }
            super::queue::post(move || app::input(Input::Folio(id, Ask::Left(left))));
        });
    }
}

thread_local! {
    static CHATS: RefCell<HashMap<String, Chat>> = RefCell::new(HashMap::new());
    /// How each Folio that is starting again starts, by id, until it has.
    static AGAIN: RefCell<HashMap<String, Again>> = RefCell::new(HashMap::new());
}

pub(super) fn with<T>(id: &str, f: impl FnOnce(&mut Chat) -> T) -> Option<T> {
    CHATS.with(|c| c.try_borrow_mut().ok()?.get_mut(id).map(f))
}

/// Runs `f` on every Folio, by id. False when they are held elsewhere.
pub(super) fn each(mut f: impl FnMut(&str, &mut Chat)) -> bool {
    CHATS.with(|c| {
        let Ok(mut chats) = c.try_borrow_mut() else {
            return false;
        };
        for (id, chat) in chats.iter_mut() {
            f(id, chat);
        }
        true
    })
}

/// Shows session `id` as it stands now, when it is on the stage.
pub(super) fn refresh(id: &str) {
    if let Some(v) = with(id, |c| c.view.clone()).flatten() {
        v.refresh();
    }
}

/// The reader thread's side: queues the event and has the main thread
/// drain the queue.
fn on_event(
    inbox: Arc<Mutex<VecDeque<Event>>>,
    posted: Arc<AtomicBool>,
) -> impl FnMut(Event) + Send + 'static {
    move |event| {
        if let Ok(mut q) = inbox.lock() {
            q.push_back(event);
        }
        if !posted.swap(true, Ordering::AcqRel) {
            super::queue::post(drain);
        }
    }
}

/// Reads the first `limit` bytes of a resumed conversation's transcript
/// into a Folio on a thread of its own, since a long session's runs to
/// many megabytes. Until it is in, [`drain`] leaves the events waiting,
/// so the history stays first.
fn load_history(chat: &mut Chat, path: PathBuf, limit: u64) {
    let slot: Arc<Mutex<Option<Folio>>> = Arc::default();
    chat.loading = Some(Arc::clone(&slot));
    let posted = Arc::clone(&chat.posted);
    let at = chat.rewound.clone();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut bytes = Vec::new();
        if let Ok(file) = std::fs::File::open(&path) {
            let _ = file.take(limit).read_to_end(&mut bytes);
        }
        let folio = Folio::from_transcript_at(&String::from_utf8_lossy(&bytes), at.as_deref());
        if let Ok(mut s) = slot.lock() {
            *s = Some(folio);
        }
        posted.store(true, Ordering::Release);
        super::queue::post(drain);
    });
}

/// Starts session `id`, a headless Claude in `dir` in a host of its own,
/// resuming the conversation `tag.resume` when it names one. In a folder
/// Claude Code has not been told to trust, it waits for the human to say
/// so in its pane first. One starting again after [`Leave::Again`] takes
/// that start's flags and greeting here.
pub fn open(id: &str, dir: &Path, mut tag: Tag, options: &Options) -> Result<(), String> {
    if CHATS.with(|c| c.borrow().contains_key(id)) {
        return Ok(());
    }
    let mut options = options.clone();
    let again = AGAIN.with(|a| a.borrow_mut().remove(id));
    if let Some(a) = &again {
        options.args.extend(a.flags.iter().cloned());
        tag.at = a.at.clone();
    }
    let mut chat = Chat::new(dir, tag.clone());
    chat.review = again.as_ref().is_some_and(|a| a.review);
    chat.rewound = tag.at.clone();
    chat.greeting = again.and_then(|a| a.said);
    if let Some(conversation) = tag.resume.clone() {
        // `--resume` streams only the new turns: what came before is read
        // from the transcript, and how much there was goes in the tag.
        if let Some((path, len)) = history_file(dir, &conversation) {
            tag.history = len;
            chat.tag.history = len;
            load_history(&mut chat, path, len);
        }
    }
    if chat.loading.is_none() {
        if let Some(said) = chat.greeting.take() {
            chat.folio.note(&said);
        }
    }
    if !trusted(dir) {
        chat.pending = Some((tag, options));
        CHATS.with(|c| c.borrow_mut().insert(id.to_string(), chat));
        return Ok(());
    }
    start_host(id, &mut chat, &tag, &options)?;
    CHATS.with(|c| c.borrow_mut().insert(id.to_string(), chat));
    Ok(())
}

/// Session `id` starts again as `again` says, the next time it opens.
pub fn prepare(id: &str, again: Again) {
    AGAIN.with(|a| a.borrow_mut().insert(id.to_string(), again));
}

fn start_host(id: &str, chat: &mut Chat, tag: &Tag, options: &Options) -> Result<(), String> {
    let tag_json = serde_json::to_string(tag).map_err(|e| e.to_string())?;
    let events = on_event(Arc::clone(&chat.inbox), Arc::clone(&chat.posted));
    let hosted = Hosted::start(
        &crate::console::host_program(),
        &horadric_hooks::instance(),
        id,
        &chat.dir,
        tag.resume.as_deref(),
        options,
        &tag_json,
        events,
    )
    .map_err(|e| format!("The session did not start: {e}"))?;
    chat.hosted = Some(hosted);
    Ok(())
}

/// A session found running in its host after a reload, a crash or a Quit
/// that kept it.
pub struct Found {
    pub id: String,
    pub dir: PathBuf,
    pub tag: Tag,
}

/// Attaches to every Folio's host still running for this Horadric that
/// has no Folio here yet. Each replays all it said since it started.
pub fn reattach() -> Vec<Found> {
    let mut found = Vec::new();
    for id in Hosted::list(&horadric_hooks::instance()) {
        if CHATS.with(|c| c.borrow().contains_key(&id)) {
            continue;
        }
        let inbox: Arc<Mutex<VecDeque<Event>>> = Arc::default();
        let posted: Arc<AtomicBool> = Arc::default();
        let events = on_event(Arc::clone(&inbox), Arc::clone(&posted));
        let slot: Arc<Mutex<Option<Folio>>> = Arc::default();
        let read_into = Arc::clone(&slot);
        let hosted = match Hosted::attach_with(&horadric_hooks::instance(), &id, |info| {
            catch_up(info, read_into, events)
        }) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("horadric: cannot attach to the Folio {id}: {e}");
                continue;
            }
        };
        let info = hosted.info().clone();
        let tag: Tag = serde_json::from_str(&info.tag).unwrap_or_default();
        let mut chat = Chat::new(&info.cwd, tag.clone());
        chat.rewound = tag.at.clone();
        chat.inbox = inbox;
        chat.posted = posted;
        chat.loading = Some(slot);
        chat.hosted = Some(hosted);
        CHATS.with(|c| c.borrow_mut().insert(id.clone(), chat));
        found.push(Found {
            id,
            dir: info.cwd,
            tag,
        });
    }
    found
}

/// Ends session `id` for good: claude gets its stdin closed and a moment
/// to end itself, then its host ends it.
pub fn close(id: &str) {
    let gone = CHATS.with(|c| c.borrow_mut().remove(id));
    if let Some(c) = gone {
        if let Some(v) = &c.view {
            v.removeFromSuperview();
        }
        if let Some(h) = &c.hosted {
            h.close();
        }
    }
}

/// Ends every session, as a Quit that stops them does.
pub fn close_all() {
    let ids: Vec<String> = CHATS.with(|c| c.borrow().keys().cloned().collect());
    for id in ids {
        close(&id);
    }
}

/// The conversation session `id` carries on, as its stream named it.
pub fn conversation(id: &str) -> Option<String> {
    with(id, |c| c.folio.session_id.clone()).flatten()
}

/// What the pane's header says beside the name.
pub fn detail(id: &str) -> String {
    with(id, |c| c.detail()).unwrap_or_default()
}

/// Sends `text` as a prompt, queued behind the turn running now. False
/// when the session cannot take one yet.
pub fn prompt(id: &str, text: &str) -> bool {
    with(id, |c| {
        if !c.ready() {
            return false;
        }
        let line = c.folio.queue_prompt(text, &[], Priority::Next);
        c.send(Some(line))
    })
    .unwrap_or(false)
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Takes in what every Folio's reader thread queued, tells the app what
/// changed for each tile, and refreshes the views that show it.
pub(super) fn drain() {
    let mut tell: Vec<Input> = Vec::new();
    let mut changed: Vec<Retained<FolioView>> = Vec::new();
    let mut pages: Vec<String> = Vec::new();
    let mut forks: Vec<String> = Vec::new();
    let mut rehook: Vec<String> = Vec::new();
    let busy = CHATS.with(|c| {
        let Ok(mut chats) = c.try_borrow_mut() else {
            return true;
        };
        for (id, chat) in chats.iter_mut() {
            // Before the drain, so an event after it posts again.
            chat.posted.store(false, Ordering::Release);
            let mut moved = false;
            if let Some(slot) = &chat.loading {
                // The history goes first: what streams waits for it.
                let Some(history) = slot.lock().ok().and_then(|mut s| s.take()) else {
                    continue;
                };
                chat.folio = history;
                chat.loading = None;
                if let Some(said) = chat.greeting.take() {
                    chat.folio.note(&said);
                }
                moved = true;
            }
            let events: Vec<Event> = match chat.inbox.lock() {
                Ok(mut q) => q.drain(..).collect(),
                Err(_) => continue,
            };
            // Before the lines, which may ask whether an edit waits.
            chat.folio.review = chat.review;
            for event in events {
                // A replay's lines are old, and when they came is lost.
                chat.folio.clock = chat.live.then(unix_ms);
                match event {
                    Event::Out(l) => moved |= chat.folio.feed(&l),
                    Event::Sent(l) => moved |= chat.folio.feed_sent(&l),
                    Event::Dropped(n) => {
                        eprintln!("horadric: folio {id} lost its {n} oldest lines");
                    }
                    Event::Live => {
                        chat.live = true;
                        moved = true;
                    }
                    Event::End(_) => {
                        chat.folio.end();
                        // Before any init, nothing ran: stderr says why.
                        let asked = chat.leaving.is_some();
                        if !asked && chat.folio.session_id.is_none() && chat.error.is_none() {
                            let tail = (chat.hosted.as_ref())
                                .map(Hosted::stderr_tail)
                                .unwrap_or_default();
                            chat.error = Some(start_failure(&tail));
                        }
                        chat.ended = true;
                        moved = true;
                    }
                }
            }
            if chat.ended && chat.leaving.is_some() {
                chat.tell_left(id);
            }
            if chat.folio.unreadable() && chat.error.is_none() {
                chat.error = Some(
                    "This Claude's stream could not be read. Go on in a terminal from the menu at the bar's start."
                        .to_string(),
                );
                moved = true;
            }
            chat.flush();
            if chat.ready() {
                moved |= tend(id, chat, &mut pages, &mut forks, &mut rehook);
            }
            let now = chat.state();
            if chat.told.as_ref() != Some(&now) {
                chat.told = Some(now.clone());
                tell.push(Input::FolioState(id.clone(), now));
                moved = true;
            }
            if let (true, Some(v)) = (moved, &chat.view) {
                changed.push(v.clone());
            }
        }
        false
    });
    if busy {
        // A view holds the Folios. Come back once it is done.
        super::queue::post(drain);
    }
    for url in pages {
        commands::open_url(&url);
    }
    for id in forks {
        commands::fork_now(&id);
    }
    for id in rehook {
        commands::start_again(&id, "Review is on: each edit waits for you.");
    }
    for v in changed {
        v.refresh();
    }
    for input in tell {
        app::input(input);
    }
}

/// Keeps a live session's side up to date, as Windows's composer does:
/// asks for its commands once, at a turn's end the context and a title,
/// its MCP servers' status when they need it, and goes on with a fork
/// once its files are back. True when something shown changed.
fn tend(
    id: &str,
    chat: &mut Chat,
    pages: &mut Vec<String>,
    forks: &mut Vec<String>,
    rehook: &mut Vec<String>,
) -> bool {
    let mut moved = false;
    // Once per claude: a reattach's replay says whether it went.
    if !chat.folio.initialized {
        let line = chat.folio.initialize();
        chat.send(Some(line));
    }
    // Review turned on in a claude whose `initialize` went without its
    // hook: it starts again to register it.
    if chat.review && !chat.folio.review_hooked && chat.between_turns() {
        rehook.push(id.to_string());
    }
    let busy = chat.folio.busy;
    if chat.busy_was && !busy {
        let line = chat.folio.get_context_usage();
        chat.send(Some(line));
        // Headless, the CLI names no conversation by itself.
        let title = chat.folio.ask_title();
        chat.send(title);
    }
    chat.busy_was = busy;
    match chat.folio.mcp_wanted() {
        McpWant::Now => {
            let line = chat.folio.mcp_status();
            chat.send(Some(line));
        }
        McpWant::Soon if !chat.mcp_soon => {
            chat.mcp_soon = true;
            let id = id.to_string();
            super::queue::after_main(Duration::from_secs(2), move || {
                with(&id, |c| c.mcp_soon = false);
                with(&id, |c| {
                    if c.ready() && c.folio.mcp_wanted() != McpWant::No {
                        let line = c.folio.mcp_status();
                        c.send(Some(line));
                    }
                });
            });
        }
        _ => {}
    }
    pages.extend(chat.folio.take_mcp_pages());
    if let Some(how) = chat.folio.files_rewound.take() {
        if let Some(f) = chat.fork.as_mut() {
            f.files = Some(how);
            forks.push(id.to_string());
            moved = true;
        }
    }
    moved
}

/// Whether Claude Code would start in `dir` without asking to trust it.
fn trusted(dir: &Path) -> bool {
    let key = trust::project_key(&dir.to_string_lossy());
    config_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .is_some_and(|c| trust::accepted(&c, &key))
}

/// Records that the human trusts `dir`, where Claude Code keeps its own.
fn trust_dir(dir: &Path) -> std::io::Result<()> {
    let path = config_path()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no home folder"))?;
    let key = trust::project_key(&dir.to_string_lossy());
    let mut config = match std::fs::read(&path) {
        // A file being written is not ours to replace with less.
        Ok(b) => serde_json::from_slice(&b)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Null,
        Err(e) => return Err(e),
    };
    if !trust::trust(&mut config, &key) {
        return Ok(());
    }
    // Written beside and renamed over, so Claude Code never reads half.
    let tmp = path.with_extension("json.horadric-tmp");
    let mut text = serde_json::to_string_pretty(&config)?;
    text.push('\n');
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

/// `.claude.json`, in `CLAUDE_CONFIG_DIR` or the home folder.
fn config_path() -> Option<PathBuf> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|d| !d.is_empty())
        .or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(dir).join(".claude.json"))
}

/// What a button the human pressed in session `id`'s bar does.
fn act(id: &str, act: Act, view: &FolioView) {
    match act {
        Act::Allow(r) => answer(id, |c| c.folio.reply(&r, Reply::Allow)),
        Act::Always(r) => answer(id, |c| c.folio.reply(&r, Reply::Always)),
        Act::Deny(r) => {
            let why = view.take_composer();
            answer(id, |c| {
                let reason = (c.reviews.get(&r)).and_then(|review| {
                    let dir = c.dir.clone();
                    crate::folio_review::edit_reason(&c.folio.blocks, &r, review, &why, |p| {
                        Path::new(p)
                            .strip_prefix(&dir)
                            .ok()
                            .map(|rel| rel.display().to_string())
                    })
                });
                let reason = reason.unwrap_or(why);
                c.folio.reply(&r, Reply::Deny(&reason))
            })
        }
        Act::Comment(r) => commands::comment(id, &r),
        Act::Approve(r) => answer(id, |c| c.folio.approve_plan(&r, None)),
        Act::KeepPlanning(r) => {
            let feedback = view.take_composer();
            answer(id, |c| c.folio.keep_planning(&r, &feedback))
        }
        Act::Answer(r) => {
            let Some(picks) = super::folio_forms::pick_answers(id, &r, view) else {
                return;
            };
            answer(id, |c| c.folio.answer_question(&r, &picks))
        }
        Act::Elicit(r) => commands::elicit(id, &r),
        Act::Stop => stop(id),
        Act::Trust => trust_folder(id),
        Act::LogIn => commands::log_in(id),
        Act::OpenPage => commands::open_page(id),
        Act::SendCode => {
            let code = view.take_composer();
            commands::send_code(id, &code);
        }
        Act::CancelLogIn => commands::cancel_login(id),
        Act::ClearImages => {
            with(id, |c| c.images.clear());
        }
    }
    view.refresh();
}

pub(super) fn answer(id: &str, make: impl FnOnce(&mut Chat) -> Option<String>) {
    with(id, |c| {
        let line = make(c);
        c.send(line);
    });
    super::queue::post(drain);
}

/// Stops the turn running now. The CLI ends it with a `result` and stays
/// up for the next prompt.
pub(super) fn stop(id: &str) {
    with(id, |c| {
        if let Some(h) = &c.hosted {
            let _ = h.interrupt();
        }
    });
}

/// The human trusts the folder of session `id`, which waits for it: the
/// trust is recorded where Claude Code keeps its own, and it starts.
fn trust_folder(id: &str) {
    with(id, |c| {
        let Some((tag, options)) = c.pending.take() else {
            return;
        };
        if let Err(e) = trust_dir(&c.dir) {
            c.error = Some(format!("Could not record that you trust this folder: {e}"));
            c.pending = Some((tag, options));
            return;
        }
        if let Err(e) = start_host(id, c, &tag, &options) {
            c.error = Some(e);
            c.ended = true;
        }
    });
    super::queue::post(drain);
}

pub struct ViewIvars {
    id: RefCell<String>,
    parts: RefCell<Option<Parts>>,
    /// What each bar button does now, by its tag.
    acts: RefCell<Vec<Act>>,
    /// The paragraphs last shown, so an unchanged Folio is not set again
    /// and the selection survives a refresh.
    shown: RefCell<Vec<folio_text::Paragraph>>,
}

/// The views a Folio's view is made of.
struct Parts {
    transcript: Retained<NSScrollView>,
    text: Retained<NSTextView>,
    composer_scroll: Retained<NSScrollView>,
    composer: Retained<ComposerText>,
    status: Retained<NSTextField>,
    more: Retained<NSButton>,
    buttons: Vec<Retained<NSButton>>,
}

define_class!(
    // SAFETY: NSView has no subclassing requirements, and this view does
    // not implement Drop.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ViewIvars]
    pub struct FolioView;

    impl FolioView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            if let Some(p) = Painter::current() {
                let b = self.bounds();
                let all = Rect::new(0.0, 0.0, b.size.width as f32, b.size.height as f32);
                p.fill_rect(&all, theme::screen());
            }
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            let _: () = unsafe { msg_send![super(self), setFrameSize: size] };
            self.lay_out();
        }

        #[unsafe(method(act:))]
        fn act_pressed(&self, sender: &NSButton) {
            let tag = sender.tag() as usize;
            let picked = self.ivars().acts.borrow().get(tag).cloned();
            if let Some(a) = picked {
                let id = self.ivars().id.borrow().clone();
                act(&id, a, self);
            }
        }

        #[unsafe(method(more:))]
        fn more_pressed(&self, _sender: &NSButton) {
            let id = self.ivars().id.borrow().clone();
            commands::more(&id, self);
            self.refresh();
        }
    }

    unsafe impl NSObjectProtocol for FolioView {}

    unsafe impl NSTextDelegate for FolioView {}

    unsafe impl NSTextViewDelegate for FolioView {
        #[unsafe(method(textView:doCommandBySelector:))]
        unsafe fn text_view_do_command(&self, _view: &NSTextView, selector: Sel) -> bool {
            if selector == sel!(insertNewline:) {
                let mtm = MainThreadMarker::from(self);
                let flags = NSApplication::sharedApplication(mtm)
                    .currentEvent()
                    .map(|e| e.modifierFlags())
                    .unwrap_or_else(NSEventModifierFlags::empty);
                // Shift or Option with Return is a new line, as in Claude.
                // The method macro converts only the tail value to Bool, so
                // this returns no early.
                if flags.intersects(NSEventModifierFlags::Shift | NSEventModifierFlags::Option) {
                    false
                } else {
                    self.submit();
                    true
                }
            } else if selector == sel!(cancelOperation:) {
                stop(&self.ivars().id.borrow());
                true
            } else if selector == sel!(insertTab:) {
                self.complete()
            } else {
                false
            }
        }
    }
);

pub struct ComposerIvars {
    id: RefCell<String>,
}

define_class!(
    // SAFETY: NSTextView may be subclassed, and this one only adds a
    // method that falls back on its own; it does not implement Drop.
    #[unsafe(super(NSTextView, NSText, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ComposerIvars]
    pub struct ComposerText;

    impl ComposerText {
        /// An image on the pasteboard goes with the next prompt; anything
        /// else pastes as text.
        #[unsafe(method(paste:))]
        fn paste(&self, sender: Option<&AnyObject>) {
            let id = self.ivars().id.borrow().clone();
            if !commands::paste_image(&id) {
                let _: () = unsafe { msg_send![super(self), paste: sender] };
            }
        }
    }

    unsafe impl NSObjectProtocol for ComposerText {}
);

impl ComposerText {
    /// A composer for session `id` in a scroll view of its own, made as
    /// `scrollableTextView` makes one, since that cannot make a subclass.
    fn scrolled(
        mtm: MainThreadMarker,
        id: &str,
    ) -> (Retained<NSScrollView>, Retained<ComposerText>) {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(400.0, COMPOSER_H));
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
        scroll.setHasVerticalScroller(true);
        let size = scroll.contentSize();
        let this = Self::alloc(mtm).set_ivars(ComposerIvars {
            id: RefCell::new(id.to_string()),
        });
        let inner = NSRect::new(NSPoint::new(0.0, 0.0), size);
        let text: Retained<ComposerText> = unsafe { msg_send![super(this), initWithFrame: inner] };
        text.setMinSize(NSSize::new(0.0, size.height));
        text.setMaxSize(NSSize::new(f64::MAX, f64::MAX));
        text.setVerticallyResizable(true);
        text.setHorizontallyResizable(false);
        text.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        if let Some(container) = unsafe { text.textContainer() } {
            container.setContainerSize(NSSize::new(size.width, f64::MAX));
            container.setWidthTracksTextView(true);
        }
        scroll.setDocumentView(Some(&text));
        (scroll, text)
    }
}

impl FolioView {
    fn new(mtm: MainThreadMarker, id: &str) -> Retained<FolioView> {
        let this = Self::alloc(mtm).set_ivars(ViewIvars {
            id: RefCell::new(id.to_string()),
            parts: RefCell::new(None),
            acts: RefCell::new(Vec::new()),
            shown: RefCell::new(Vec::new()),
        });
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(400.0, 300.0));
        let view: Retained<FolioView> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.build(mtm);
        view
    }

    fn build(&self, mtm: MainThreadMarker) {
        let id = self.ivars().id.borrow().clone();
        let transcript = NSTextView::scrollableTextView(mtm);
        let text = text_view_of(&transcript);
        text.setEditable(false);
        text.setSelectable(true);
        text.setRichText(true);
        text.setDrawsBackground(false);
        text.setTextContainerInset(NSSize::new(PAD, PAD));
        transcript.setDrawsBackground(false);
        transcript.setAutohidesScrollers(true);
        self.addSubview(&transcript);

        let (composer_scroll, composer) = ComposerText::scrolled(mtm, &id);
        composer.setEditable(true);
        composer.setRichText(false);
        composer.setAllowsUndo(true);
        composer.setAutomaticQuoteSubstitutionEnabled(false);
        composer.setAutomaticDashSubstitutionEnabled(false);
        composer.setAutomaticTextReplacementEnabled(false);
        composer.setFont(Some(&body_font()));
        composer.setTextColor(Some(&ns_color(theme::text())));
        composer.setBackgroundColor(&ns_color(theme::surface()));
        composer.setInsertionPointColor(Some(&ns_color(theme::text())));
        composer.setTextContainerInset(NSSize::new(4.0, 6.0));
        composer.setDelegate(Some(ProtocolObject::from_ref(self)));
        composer_scroll.setAutohidesScrollers(true);
        self.addSubview(&composer_scroll);

        let status = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        status.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        status.setTextColor(Some(&ns_color(theme::text_dim())));
        self.addSubview(&status);

        let target: &AnyObject = self.as_ref();
        let more = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("\u{22ef}"),
                Some(target),
                Some(sel!(more:)),
                mtm,
            )
        };
        more.setBezelStyle(NSBezelStyle::Push);
        self.addSubview(&more);

        let buttons = (0..BUTTONS)
            .map(|i| {
                let b = unsafe {
                    NSButton::buttonWithTitle_target_action(
                        &NSString::from_str(""),
                        Some(target),
                        Some(sel!(act:)),
                        mtm,
                    )
                };
                b.setBezelStyle(NSBezelStyle::Push);
                b.setTag(i as isize);
                b.setHidden(true);
                self.addSubview(&b);
                b
            })
            .collect();
        *self.ivars().parts.borrow_mut() = Some(Parts {
            transcript,
            text,
            composer_scroll,
            composer,
            status,
            more,
            buttons,
        });
        self.lay_out();
    }

    /// The transcript at the top, the bar, the composer at the bottom.
    fn lay_out(&self) {
        let parts = self.ivars().parts.borrow();
        let Some(p) = parts.as_ref() else {
            return;
        };
        let b = self.bounds();
        let (w, h) = (b.size.width, b.size.height);
        let composer_y = (h - COMPOSER_H - PAD).max(0.0);
        let bar_y = (composer_y - BAR_H).max(0.0);
        p.transcript
            .setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, bar_y)));
        p.composer_scroll.setFrame(NSRect::new(
            NSPoint::new(PAD, composer_y),
            NSSize::new((w - 2.0 * PAD).max(0.0), COMPOSER_H),
        ));
        p.more.setFrame(NSRect::new(
            NSPoint::new(PAD - 2.0, bar_y + 2.0),
            NSSize::new(MORE_W, BAR_H - 4.0),
        ));
        let shown = self.ivars().acts.borrow().len().min(BUTTONS);
        let mut x = w - PAD;
        for (i, button) in p.buttons.iter().enumerate().rev() {
            if i >= shown {
                continue;
            }
            x -= BUTTON_W;
            button.setFrame(NSRect::new(
                NSPoint::new(x, bar_y + 2.0),
                NSSize::new(BUTTON_W - 4.0, BAR_H - 4.0),
            ));
        }
        let left = PAD + MORE_W + 4.0;
        p.status.setFrame(NSRect::new(
            NSPoint::new(left, bar_y + 7.0),
            NSSize::new((x - left - PAD).max(0.0), BAR_H - 12.0),
        ));
    }

    /// Shows the session as it stands now.
    pub(super) fn refresh(&self) {
        let id = self.ivars().id.borrow().clone();
        let Some((paragraphs, (said, acts), comments)) = with(&id, |c| {
            let bar = c.bar();
            let comments: Vec<bool> = (bar.1.iter())
                .map(|a| match a {
                    Act::Deny(r) => c.reviews.get(r).is_some_and(|r| !r.is_empty()),
                    _ => false,
                })
                .collect();
            let reviews = &c.reviews;
            let paragraphs = folio_text::paragraphs_reviewed(&c.folio, |r| reviews.get(r));
            (paragraphs, bar, comments)
        }) else {
            return;
        };
        if let Some(p) = self.ivars().parts.borrow().as_ref() {
            p.status.setStringValue(&NSString::from_str(&said));
            for (i, b) in p.buttons.iter().enumerate() {
                match acts.get(i) {
                    Some(a) => {
                        let with_comments = comments.get(i).copied().unwrap_or(false);
                        b.setTitle(&NSString::from_str(a.title(with_comments)));
                        b.setHidden(false);
                    }
                    None => b.setHidden(true),
                }
            }
            if *self.ivars().shown.borrow() != paragraphs {
                show_paragraphs(p, &paragraphs);
                *self.ivars().shown.borrow_mut() = paragraphs;
            }
        }
        *self.ivars().acts.borrow_mut() = acts;
        self.lay_out();
        self.setNeedsDisplay(true);
    }

    /// Sends what the composer holds, and empties it once it went.
    fn submit(&self) {
        let text = self.composer_text();
        let id = self.ivars().id.borrow().clone();
        let images = with(&id, |c| !c.images.is_empty()).unwrap_or(false);
        if text.trim().is_empty() && !images {
            return;
        }
        if commands::submit(&id, text.trim_end(), self) {
            self.take_composer();
        }
        self.refresh();
    }

    /// Tab: the `/` commands that start with what is typed, in a menu.
    /// False, so Tab types itself, when it is not a command's name.
    fn complete(&self) -> bool {
        let typed = self.composer_text();
        if !typed.starts_with('/') || typed.contains(char::is_whitespace) {
            return false;
        }
        let id = self.ivars().id.borrow().clone();
        if let Some(name) = commands::pick_command(&id, &typed, self) {
            self.put_composer(&format!("/{name} "));
        }
        true
    }

    fn composer_text(&self) -> String {
        (self.ivars().parts.borrow().as_ref())
            .map(|p| p.composer.string().to_string())
            .unwrap_or_default()
    }

    /// What the composer holds, which it no longer does.
    pub(super) fn take_composer(&self) -> String {
        let text = self.composer_text();
        self.put_composer("");
        text.trim().to_string()
    }

    /// Puts `text` in the composer for the human to go on with.
    pub(super) fn put_composer(&self, text: &str) {
        if let Some(p) = self.ivars().parts.borrow().as_ref() {
            p.composer.setString(&NSString::from_str(text));
        }
    }

    /// Below the bar's buttons on the screen, where a menu they open goes.
    pub(super) fn bar_point(&self) -> NSPoint {
        let b = self.bounds();
        let y = (b.size.height - COMPOSER_H - PAD).max(0.0);
        let in_window =
            self.convertPoint_toView(NSPoint::new(b.size.width - PAD - BUTTON_W, y), None);
        self.on_screen(in_window)
    }

    /// Below the menu button at the bar's start, on the screen.
    pub(super) fn more_point(&self) -> NSPoint {
        let b = self.bounds();
        let y = (b.size.height - COMPOSER_H - PAD).max(0.0);
        let in_window = self.convertPoint_toView(NSPoint::new(PAD, y), None);
        self.on_screen(in_window)
    }

    fn on_screen(&self, in_window: NSPoint) -> NSPoint {
        match self.window() {
            Some(w) => w.convertPointToScreen(in_window),
            None => in_window,
        }
    }

    fn focus_composer(&self) {
        let parts = self.ivars().parts.borrow();
        if let (Some(p), Some(w)) = (parts.as_ref(), self.window()) {
            w.makeFirstResponder(Some(&p.composer));
        }
    }
}

/// The text view a scrollable text view scrolls.
fn text_view_of(scroll: &NSScrollView) -> Retained<NSTextView> {
    scroll
        .documentView()
        .and_then(|v| v.downcast::<NSTextView>().ok())
        .expect("a scrollable text view holds a text view")
}

fn body_font() -> Retained<NSFont> {
    NSFont::systemFontOfSize_weight(13.0, unsafe { NSFontWeightRegular })
}

fn mono_font() -> Retained<NSFont> {
    unsafe { NSFont::monospacedSystemFontOfSize_weight(12.0, NSFontWeightRegular) }
}

/// The font and colour of each kind of paragraph.
fn look(style: Style) -> (Retained<NSFont>, Color) {
    match style {
        Style::Prompt => (NSFont::boldSystemFontOfSize(13.0), theme::text()),
        Style::Reply => (body_font(), theme::text()),
        Style::Thinking => (body_font(), theme::text_dim()),
        Style::Step | Style::Shell | Style::Kept => (mono_font(), theme::text_dim()),
        Style::StepFailed | Style::Removed => (mono_font(), theme::error()),
        Style::Added => (mono_font(), theme::done()),
        Style::Ask | Style::Comment => (body_font(), theme::waiting()),
        Style::Notice | Style::Footer => (NSFont::systemFontOfSize(11.5), theme::text_dim()),
        Style::Error => (body_font(), theme::error()),
    }
}

/// Puts `paragraphs` in the transcript, keeping it at the end when it was.
fn show_paragraphs(p: &Parts, paragraphs: &[folio_text::Paragraph]) {
    let clip = p.transcript.contentView().bounds();
    let doc = p.text.frame();
    let at_end = clip.origin.y + clip.size.height >= doc.size.height - 4.0;
    let all = NSMutableAttributedString::new();
    for (i, para) in paragraphs.iter().enumerate() {
        let (font, color) = look(para.style);
        let style = NSMutableParagraphStyle::new();
        let tight = |s: Style| {
            matches!(
                s,
                Style::Step | Style::StepFailed | Style::Added | Style::Removed | Style::Kept
            )
        };
        // A diff's rows and a run of steps sit together; the rest apart.
        let next_tight = paragraphs.get(i + 1).is_some_and(|n| tight(n.style));
        style.setParagraphSpacing(match tight(para.style) && next_tight {
            true => 0.0,
            false if tight(para.style) => 6.0,
            false => 10.0,
        });
        let color = ns_color(color);
        let keys: [&NSString; 3] = unsafe {
            [
                NSFontAttributeName,
                NSForegroundColorAttributeName,
                NSParagraphStyleAttributeName,
            ]
        };
        let values: [&AnyObject; 3] = [font.as_ref(), color.as_ref(), style.as_ref()];
        let attrs = NSDictionary::from_slices(&keys, &values);
        let piece = unsafe {
            NSAttributedString::new_with_attributes(
                &NSString::from_str(&format!("{}\n", para.text)),
                &attrs,
            )
        };
        all.appendAttributedString(&piece);
    }
    if let Some(storage) = unsafe { p.text.textStorage() } {
        storage.setAttributedString(&all);
    }
    if at_end {
        let len = p.text.string().length();
        p.text.scrollRangeToVisible(NSRange::new(len, 0));
    }
}

/// Lays the Folio views of the panes in `shown` over their glass in the
/// stage's view, by session id and rect, and takes away those of panes no
/// longer shown.
pub fn place(stage: &NSView, shown: &[(String, Rect)]) {
    let mtm = MainThreadMarker::from(stage);
    CHATS.with(|c| {
        let Ok(mut chats) = c.try_borrow_mut() else {
            return;
        };
        for (id, chat) in chats.iter_mut() {
            let Some(r) = shown.iter().find(|(s, _)| s == id).map(|(_, r)| *r) else {
                if let Some(v) = &chat.view {
                    v.removeFromSuperview();
                }
                continue;
            };
            let v = chat
                .view
                .get_or_insert_with(|| FolioView::new(mtm, id))
                .clone();
            let parent = unsafe { v.superview() };
            if !parent.is_some_and(|p| std::ptr::eq(&*p, stage)) {
                stage.addSubview(&v);
            }
            v.setFrame(cg_rect(&r));
        }
    });
    // Outside the borrow: a refresh reads the Folio.
    let views: Vec<Retained<FolioView>> = CHATS.with(|c| {
        c.borrow()
            .iter()
            .filter(|(id, _)| shown.iter().any(|(s, _)| s == *id))
            .filter_map(|(_, chat)| chat.view.clone())
            .collect()
    });
    for v in views {
        v.refresh();
    }
}

/// Gives session `id`'s composer the keyboard.
pub fn focus(id: &str) {
    if let Some(v) = with(id, |c| c.view.clone()).flatten() {
        v.focus_composer();
    }
}

/// Every Folio's conversation as text, by id, for the snapshots CI keeps.
pub fn texts() -> Vec<(String, String)> {
    CHATS.with(|c| {
        c.borrow()
            .iter()
            .map(|(id, chat)| {
                let text: Vec<String> = folio_text::paragraphs(&chat.folio)
                    .into_iter()
                    .map(|p| p.text)
                    .collect();
                (id.clone(), text.join("\n\n"))
            })
            .collect()
    })
}
