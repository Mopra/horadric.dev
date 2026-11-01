//! The Folio: a headless Claude session's conversation drawn as a chat, not
//! a terminal. The session kind new Claude sessions open as, beside the
//! terminal ones: a tile in its project's cluster and a pane on the stage. See
//! docs/plan/headless.md.
//!
//! A Folio is a pane on the stage like a sanctuary: its console has no
//! program, and the pane keeps its header and glass. The conversation is a
//! child window of the pane laid over the glass, with its own render
//! target, since a pane's drawing is the terminal's. The pane keeps the
//! keyboard and hands keys here; the child takes the mouse.
//!
//! The session itself (the [`Folio`] model and the [`Hosted`] client of
//! the host that runs claude.exe) lives here by session id, not in the
//! window, so it runs on while the stage shows another project. The host
//! outlives the app: a reload drops the client, which only detaches, and
//! [`reattach`] finds the session again with everything it said replayed.
//! Events come on the client's reader thread and reach the UI thread
//! through a message only window.
//!
//! Behind the conversation runes drift up in the project's colour. They
//! warm while Claude works and cool to rest when it stops,
//! and at rest, or hidden, nothing redraws.
//!
//! Here is the session: its registry by session id, its lifecycle and
//! what reaches the CLI. [`view`] is the window, its paint loop and input,
//! [`transcript`] lays out and draws the blocks, [`composer`] the input
//! box and its buttons, and [`backdrop`] the background.

pub mod afar;
mod backdrop;
pub mod band;
mod banish;
mod cards;
mod comments;
mod composer;
mod controls;
mod editor;
mod flow;
mod lightbox;
mod mcp;
mod picker;
mod rich;
mod select;
mod sign_in;
mod slash;
mod summon;
mod transcript;
mod view;
mod watch;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use horadric_core::folio::backlog::{self, Kept};
use horadric_core::folio::compaction::{Act, Moment, Watch};
use horadric_core::folio::{Block, Folio, Notice};
use horadric_core::usage::Status;
use horadric_core::{HookEvent, Phase, WaitReason};
use horadric_pty::headless::Options;
use horadric_pty::headless_host::{Event, HostInfo, Hosted};
use serde::{Deserialize, Serialize};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetParent, KillTimer, PostMessageW, RegisterClassW, SetTimer,
    HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_TIMER, WM_USER, WNDCLASSW,
};

use crate::app::{self, Input};
use crate::folio_look as look;

pub use composer::{before_caret, dictated, dictation, handed};
pub use select::find;
pub use view::{
    attach, char, key, keyboard_moved, place, place_bar, refont, register_class, summon, wheel,
};
pub use watch::{is_watch, watch, watch_detail, watch_title};
/// Posted to the pane a Folio is in when what its header says may have
/// changed.
pub const WM_FOLIO_REFRESH: u32 = WM_USER + 9;
/// Posted to the router when events wait in some Folio's inbox.
const WM_FOLIO_LINES: u32 = WM_USER + 10;

const ROUTER_CLASS: PCWSTR = w!("HoradricFolioRouter");
/// The router's timer that writes changed drafts.
const SAVE_TIMER: usize = 1;
/// How long after the last change a draft is written.
const SAVE_AFTER_MS: u32 = 1500;

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

/// One Folio and the session behind it.
struct Chat {
    /// The project's key, whose colour its runes take.
    key: String,
    dir: PathBuf,
    folio: Folio,
    /// None when it did not start, or once it was let go.
    hosted: Option<Hosted>,
    /// Events from the reader thread.
    inbox: Arc<Mutex<VecDeque<Event>>>,
    /// A wake is queued for the router, so a chatty stream posts once.
    posted: Arc<AtomicBool>,
    ended: bool,
    error: Option<String>,
    view: Option<HWND>,
    /// The settings in the pane's header row, a window of their own on
    /// the plate beside the name, so the glass holds only the talk.
    bar: Option<HWND>,
    /// Where the bar sits: DIPs from its left to the glass's, for the
    /// menus it opens there, and the pane's place on the stage, for the
    /// plate's light to run on across it.
    bar_shift: f32,
    plate: (f32, f32),
    input: composer::Input,
    scroll: f32,
    /// Keeps the end in view as it streams, until scrolled away from.
    follow: bool,
    content_h: f32,
    view_h: f32,
    /// Blocks unfolded by a click, by index.
    open: HashSet<usize>,
    /// The runes move toward a heat above rest.
    busy_was: bool,
    /// The heat the runes move toward, see [`Chat::aim`], since when, and
    /// the heat they had then.
    aim: f32,
    busy_since: Instant,
    heat_from: f32,
    /// What the pane's header was last told, so it is told again only
    /// when that changes.
    header: String,
    /// What the app was last told of it: working, asking, ended.
    told: Option<State>,
    /// The host has sent everything it kept and goes on live.
    live: bool,
    /// The session goes on in a terminal once claude.exe has ended.
    to_terminal: bool,
    /// With `to_terminal`, it starts again as a Folio, its last turn
    /// rewound, rather than in a terminal.
    rewinding: bool,
    /// Its stream could not be read at all, so it goes on in a terminal
    /// whether or not a turn runs: see [`Folio::unreadable`].
    unreadable: bool,
    /// The line its conversation was resumed at by a rewind, see
    /// [`Tag::at`].
    rewound: Option<String>,
    /// The session starts again once claude.exe has ended, so it reads a
    /// new login.
    restarting: bool,
    /// Whether it comes to the stage as it starts again.
    restart_show: bool,
    /// Its claude.exe ends for an account switch, which starts it again
    /// once every session has stopped: what was typed waits.
    winding: bool,
    /// What it says once its history is in, see [`sign_in::greeting`].
    greeting: Option<String>,
    /// Waits for the human to trust its folder, with what starts it then.
    /// Claude Code asks before it runs a folder's hooks and MCP servers,
    /// but not headless, so the Folio asks for it.
    pending: Option<(Tag, Options)>,
    /// A resumed conversation's history, read on a thread of its own:
    /// filled once read, None when there is none to wait for.
    loading: Option<Arc<Mutex<Option<Folio>>>>,
    /// The transcript's own state and what it keeps between frames.
    tr: transcript::Ui,
    /// A hireling's conversation, read only, rather than a session.
    watch: Option<watch::Watch>,
    /// The first prompt, sent once the session is live and its history
    /// read: a quest, Warriv's events, an errand's first step.
    first: Option<String>,
    /// The quest and shared files above the composer, as the app last said.
    band: band::Band,
    /// It holds a quest, as the app last said: only a quest is compacted
    /// by [`tend_compaction`].
    quest: bool,
    compaction: Watch,
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
            limits: Default::default(),
        })
    }
}

impl Chat {
    fn new(key: &str, dir: &Path) -> Chat {
        Chat {
            key: key.to_string(),
            dir: dir.to_path_buf(),
            folio: Folio::new(),
            hosted: None,
            inbox: Arc::default(),
            posted: Arc::default(),
            ended: false,
            error: None,
            view: None,
            bar: None,
            bar_shift: 0.0,
            plate: (0.0, 1.0),
            input: composer::Input::new(),
            scroll: 0.0,
            follow: true,
            content_h: 0.0,
            view_h: 0.0,
            open: HashSet::new(),
            busy_was: false,
            aim: 0.0,
            busy_since: Instant::now(),
            heat_from: 0.0,
            header: String::new(),
            told: None,
            live: false,
            band: band::Band::default(),
            pending: None,
            loading: None,
            to_terminal: false,
            rewinding: false,
            unreadable: false,
            rewound: None,
            restarting: false,
            restart_show: false,
            winding: false,
            greeting: None,
            tr: transcript::Ui::default(),
            watch: None,
            first: None,
            quest: false,
            compaction: Watch::default(),
        }
    }

    /// Its claude can be let go without losing a turn or an ask.
    fn between_turns(&self) -> bool {
        let unsure = self.loading.is_some() || !self.live;
        let leaving = self.to_terminal || self.restarting;
        !unsure && !leaving && !self.folio.busy && !self.asking()
    }

    /// An ask, question, plan or MCP server's question waits on the human.
    fn asking(&self) -> bool {
        !self.folio.pending_asks().is_empty() || !self.folio.pending_elicitations().is_empty()
    }

    /// Claude works and waits on no one. An ask that waits is at rest for
    /// the runes, so nothing redraws while the human reads it.
    fn working(&self) -> bool {
        self.folio.busy && !self.asking()
    }

    /// The heat the runes move toward: a turn's full burn while Claude
    /// works, a calmer one while only its background agents do, so the
    /// human sees work going on that is not Claude's own.
    fn aim(&self) -> f32 {
        if self.working() {
            1.0
        } else if !self.ended && self.folio.agents_at_work() > 0 {
            look::AGENTS_HEAT
        } else {
            0.0
        }
    }

    fn note_busy(&mut self) {
        let aim = self.aim();
        if aim != self.aim {
            self.heat_from = self.heat();
            self.aim = aim;
            self.busy_was = aim > 0.0;
            self.busy_since = Instant::now();
        }
    }

    fn heat(&self) -> f32 {
        look::heat(self.heat_from, self.aim, self.busy_since.elapsed())
    }

    fn state(&self) -> State {
        State {
            busy: self.folio.busy,
            asking: self.asking(),
            ended: self.ended,
            gone: self.ended && !(self.to_terminal || self.restarting || self.winding),
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

    fn detail(&self) -> String {
        let agents = self.folio.agents_at_work();
        let state = if self.rewinding {
            "undoing the last turn".to_string()
        } else if self.to_terminal {
            "opening in a terminal".to_string()
        } else if self.error.is_some() || self.ended {
            "ended".to_string()
        } else if self.folio.busy {
            "working".to_string()
        } else if agents > 0 {
            format!(
                "{agents} agent{} working",
                if agents == 1 { "" } else { "s" }
            )
        } else {
            "chat".to_string()
        };
        match &self.folio.model {
            Some(m) => format!("{state} \u{b7} {m}"),
            None => state,
        }
    }

    /// Whether the header says something new since it was last told.
    fn header_moved(&mut self) -> bool {
        let now = self.detail();
        if now == self.header {
            return false;
        }
        self.header = now;
        true
    }

    /// Writes what the model queued for the CLI, such as the answer to a
    /// request it cannot serve.
    fn flush(&mut self) {
        // While a reattach replays, an answer to a request may still be
        // on its way back; the Folio drops what it hears was answered.
        if !self.live {
            return;
        }
        let lines = self.folio.take_outbox();
        let Some(h) = &self.hosted else {
            return;
        };
        for line in lines {
            if let Err(e) = h.send_line(&line) {
                eprintln!("horadric: folio could not answer the CLI: {e}");
            }
        }
    }
}

thread_local! {
    static CHATS: RefCell<HashMap<String, Chat>> = RefCell::new(HashMap::new());
    static ROUTER: Cell<isize> = const { Cell::new(0) };
}

fn with<T>(id: &str, f: impl FnOnce(&mut Chat) -> T) -> Option<T> {
    CHATS.with(|c| c.try_borrow_mut().ok()?.get_mut(id).map(f))
}

/// The reader thread's side: queues the event and wakes the router.
fn on_event(
    inbox: Arc<Mutex<VecDeque<Event>>>,
    posted: Arc<AtomicBool>,
) -> impl FnMut(Event) + Send + 'static {
    let router = router();
    move |event| {
        if let Ok(mut q) = inbox.lock() {
            q.push_back(event);
        }
        if !posted.swap(true, Ordering::AcqRel) {
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(router as *mut c_void)),
                    WM_FOLIO_LINES,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }
}

/// Where the transcript of `conversation` held in `dir` is, and how long
/// it is now.
fn history_file(dir: &Path, conversation: &str) -> Option<(PathBuf, u64)> {
    let path = horadric_hooks::transcript::path(&dir.to_string_lossy(), conversation)?;
    let len = std::fs::metadata(&path).ok()?.len();
    Some((path, len))
}

/// Reads the first `limit` bytes of the transcript at `path` into a Folio
/// on a thread of its own, since a long session's runs to many megabytes:
/// the history a resumed Folio shows above what it streams. Until it is
/// in, [`drain`] leaves the chat's events waiting, so the history stays
/// first.
fn load_history(chat: &mut Chat, path: PathBuf, limit: u64) {
    let slot: Arc<Mutex<Option<Folio>>> = Arc::default();
    chat.loading = Some(Arc::clone(&slot));
    let at = chat.rewound.clone();
    let router = router();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Ok(file) = std::fs::File::open(&path) {
            let _ = file.take(limit).read_to_end(&mut bytes);
        }
        let folio = Folio::from_transcript_at(&String::from_utf8_lossy(&bytes), at.as_deref());
        if let Ok(mut s) = slot.lock() {
            *s = Some(folio);
        }
        unsafe {
            let _ = PostMessageW(
                Some(HWND(router as *mut c_void)),
                WM_FOLIO_LINES,
                WPARAM(0),
                LPARAM(0),
            );
        }
    });
}

/// A reattached Folio's events. The backlog is read into a Folio right
/// here, on the reader thread: a long session's runs to tens of MB, and
/// reading several on the UI thread stalled the window at start. The
/// Folio goes in `slot`, which [`drain`] waits for, and what follows the
/// backlog goes on to `live` as it comes.
fn catch_up(
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
fn read_backlog(
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

/// Starts session `id`, a headless Claude in `dir` in a host of its own,
/// resuming the conversation `tag.resume` when it names one. `options`
/// carry the flags and the environment its hooks find it by. In a folder
/// Claude Code has not been told to trust, it waits for the human to say
/// so in its pane first. `first` goes in as its first prompt once it can
/// take one, the folder trusted and any history read.
pub fn open_with(
    id: &str,
    dir: &Path,
    mut tag: Tag,
    options: &Options,
    first: Option<String>,
) -> Result<(), String> {
    if CHATS.with(|c| c.borrow().contains_key(id)) {
        return Ok(());
    }
    let mut chat = Chat::new(&tag.key, dir);
    chat.rewound = tag.at.clone();
    chat.first = first;
    if let Some(conversation) = tag.resume.clone() {
        // `--resume` streams only the new turns: what came before is read
        // from the transcript, and how much there was goes in the tag.
        if let Some((path, len)) = history_file(dir, &conversation) {
            tag.history = len;
            load_history(&mut chat, path, len);
        }
    }
    if let Some(said) = sign_in::greeting(id) {
        match chat.loading {
            Some(_) => chat.greeting = Some(said),
            None => chat.folio.note(&said),
        }
    }
    if !crate::accounts::trusted(dir) {
        chat.pending = Some((tag, options.clone()));
        CHATS.with(|c| c.borrow_mut().insert(id.to_string(), chat));
        return Ok(());
    }
    start_host(id, &mut chat, &tag, options)?;
    CHATS.with(|c| c.borrow_mut().insert(id.to_string(), chat));
    Ok(())
}

/// Starts the host that runs `chat`'s claude.exe.
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

/// The human trusts the folder of session `id`, which waits for it: the
/// trust is recorded where Claude Code keeps its own, and the session
/// starts.
fn trust_folder(id: &str) {
    with(id, |c| {
        let Some((tag, options)) = c.pending.take() else {
            return;
        };
        if let Err(e) = crate::accounts::trust(&c.dir) {
            c.error = Some(format!("Could not record that you trust this folder: {e}"));
            c.pending = Some((tag, options));
            return;
        }
        if let Err(e) = start_host(id, c, &tag, &options) {
            c.error = Some(e);
            c.ended = true;
        }
    });
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
        let mut chat = Chat::new(&tag.key, &info.cwd);
        chat.inbox = inbox;
        chat.posted = posted;
        chat.rewound = tag.at.clone();
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

/// Ends session `id` for good: its claude.exe gets its stdin closed and
/// a moment to end itself, then its host ends it.
pub fn close(id: &str) {
    let mut gone = CHATS.with(|c| c.borrow_mut().remove(id));
    if let Some(c) = gone.as_mut() {
        // Handed over, it may come back as a Folio: what was typed waits.
        if c.to_terminal || c.restarting || c.winding {
            c.input.fold_queue();
        } else {
            c.input.forget();
        }
    }
    if let Some(h) = gone.as_ref().and_then(|c| c.hosted.as_ref()) {
        h.close();
    }
}

/// Ends session `id` once it is between turns, so it can go on in a
/// terminal: the app hears [`Input::FolioHandedOver`] once
/// claude.exe has ended. False while it works or an ask waits.
pub fn hand_over(id: &str) -> bool {
    let_go(id, false)
}

/// Ends session `id` once it is between turns, as [`hand_over`] does, for
/// the app to start it again with its last turn rewound: it hears
/// [`Input::FolioHandedOver`] too.
pub fn rewind(id: &str) -> bool {
    let_go(id, true)
}

fn let_go(id: &str, rewinding: bool) -> bool {
    with(id, |c| {
        if !c.between_turns() {
            return false;
        }
        let Some(h) = &c.hosted else {
            return false;
        };
        c.to_terminal = true;
        c.rewinding = rewinding;
        h.close();
        true
    })
    .unwrap_or(false)
}

/// Ends session `id`'s claude.exe for an account switch, keeping its tile
/// and what was typed; the app starts it again on the new login. At rest
/// only, unless `now`. True once it is on its way out or has ended.
pub fn wind_down(id: &str, now: bool) -> bool {
    with(id, |c| {
        if c.ended || c.hosted.is_none() {
            return true;
        }
        if c.to_terminal || c.restarting || c.pending.is_some() {
            return false;
        }
        if !now && !c.between_turns() {
            return false;
        }
        c.winding = true;
        if let Some(h) = &c.hosted {
            h.close();
        }
        true
    })
    .unwrap_or(false)
}

/// Where an undo of session `id`'s last turn counts back from: its
/// conversation, and the line it was rewound to when no prompt came
/// since. None while it works, an ask waits, or it has no conversation.
pub fn rewind_from(id: &str) -> Option<(String, Option<String>)> {
    with(id, |c| {
        if !c.between_turns() {
            return None;
        }
        let conversation = c.folio.session_id.clone()?;
        let prompted = (c.folio.blocks.get(c.folio.history..).into_iter().flatten())
            .any(|b| matches!(b, horadric_core::folio::Block::Prompt(_)));
        Some((conversation, c.rewound.clone().filter(|_| !prompted)))
    })
    .flatten()
}

/// Asks session `id`'s claude to put its files back as they were when
/// the prompt `uuid` was sent: the app hears
/// [`Input::FolioFilesRewound`]. False unless it rests.
pub fn rewind_files(id: &str, uuid: &str) -> bool {
    with(id, |c| {
        if !c.between_turns() {
            return false;
        }
        let line = c.folio.rewind_files(uuid);
        c.hosted
            .as_ref()
            .is_some_and(|h| h.send_line(&line).is_ok())
    })
    .unwrap_or(false)
}

/// Has session `id` say `text` on its page once it starts again.
pub fn greet(id: &str, text: &str) {
    sign_in::greet(id, text);
}

/// Asks session `id`'s CLI to stop its background task `task`, such as a
/// subagent.
pub fn stop_task(id: &str, task: &str) {
    answer_with(id, |f| Some(f.stop_task(task)));
}

/// What the band above session `id`'s composer says, redrawn only when
/// that changes: the app sets it every second.
pub fn set_band(id: &str, b: band::Band) {
    if with(id, |c| (c.band != b).then(|| c.band = b))
        .flatten()
        .is_some()
    {
        redraw(id);
    }
}

/// The subagents session `id` runs in the background, by task id with a
/// label, once the CLI has listed them.
pub fn agents(id: &str) -> Vec<(String, String)> {
    with(id, |c| {
        (c.folio.background.iter())
            .filter(|t| t.is_agent())
            .map(|t| {
                let label = match (t.agent_type.as_str(), t.description.trim()) {
                    ("", d) => d.to_string(),
                    (a, "") => a.to_string(),
                    (a, d) => format!("{a}: {d}"),
                };
                (t.task_id.clone(), label)
            })
            .collect()
    })
    .unwrap_or_default()
}

/// Starts session `id`'s claude.exe again on its conversation, so it
/// reads a new login, saying `said` first. One that ended starts at once,
/// one at rest once it has ended; one at work or asking is left alone.
/// `show` brings it to the stage.
fn restart(id: &str, said: &str, show: bool) {
    // Some(true) starts it now, Some(false) once claude.exe has ended.
    let how = with(id, |c| {
        if c.to_terminal || c.restarting || c.pending.is_some() || c.loading.is_some() {
            return None;
        }
        if c.ended || c.hosted.is_none() {
            return Some(true);
        }
        if !c.live || c.folio.busy || !c.folio.pending_asks().is_empty() {
            return None;
        }
        let h = c.hosted.as_ref()?;
        c.restarting = true;
        c.restart_show = show;
        h.close();
        Some(false)
    })
    .flatten();
    let Some(now) = how else {
        return;
    };
    sign_in::greet(id, said);
    if now {
        app::push(Input::FolioRestart(id.to_string(), show));
    }
}

/// Lets go of every Folio, as the app reloads or quits keeping its
/// sessions: their hosts run on, and [`reattach`] finds them again.
pub fn detach_all() {
    let gone: Vec<Chat> = CHATS.with(|c| c.borrow_mut().drain().map(|(_, v)| v).collect());
    drop(gone);
}

/// Ends every Folio, as the app quits ending its sessions.
pub fn close_all() {
    let gone: Vec<Chat> = CHATS.with(|c| c.borrow_mut().drain().map(|(_, v)| v).collect());
    for h in gone.iter().filter_map(|c| c.hosted.as_ref()) {
        h.close();
    }
}

/// Whether claude.exe still runs for session `id`.
pub fn running(id: &str) -> bool {
    with(id, |c| {
        !c.ended && c.hosted.as_ref().is_some_and(Hosted::running)
    })
    .unwrap_or(false)
}

/// Whether session `id` takes what [`prompt`] sends now: up, its replay
/// and history in, and not on its way to a terminal.
pub fn ready(id: &str) -> bool {
    with(id, |c| {
        !c.ended && !c.to_terminal && c.live && c.loading.is_none() && c.hosted.is_some()
    })
    .unwrap_or(false)
}

/// Sends session `id` a line as though typed into it and Enter pressed:
/// `/model` and `/effort` switch it, anything else is a prompt. False when
/// it cannot take one now, see [`composer::prompt`].
pub fn prompt(id: &str, text: &str) -> bool {
    let went = with(id, |c| composer::prompt(c, text)).unwrap_or(false);
    if went {
        redraw(id);
    }
    went
}

/// Sends `text` to every Folio of the project `key` that can take it, as
/// though typed into each, so their answers come side by side on the
/// stage. Returns how many it went to.
pub fn broadcast(key: &str, text: &str) -> usize {
    let mut went = Vec::new();
    CHATS.with(|c| {
        let Ok(mut chats) = c.try_borrow_mut() else {
            return;
        };
        for (id, chat) in chats.iter_mut() {
            if chat.key == key && chat.watch.is_none() && composer::prompt(chat, text) {
                went.push(id.clone());
            }
        }
    });
    for id in &went {
        redraw(id);
    }
    went.len()
}

/// Says `text` above session `id`'s composer.
pub fn say(id: &str, text: &str) {
    with(id, |c| composer::notice(c, text));
    redraw(id);
}

/// Now, in Unix milliseconds.
fn unix_ms() -> u64 {
    (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH))
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// Stops session `id`'s turn, as Esc in its composer does. False when no
/// turn runs.
pub fn stop(id: &str) -> bool {
    with(id, composer::stop).unwrap_or(false)
}

/// Allows the oldest permission ask session `id` waits on, once, as Enter
/// does in the terminal. A question or a plan needs its own answer, so
/// they wait. False when no permission ask waits.
pub fn approve(id: &str) -> bool {
    let line = with(id, |c| {
        // While a reattach replays, the ask may already be answered.
        if !c.live {
            return None;
        }
        let ask = c.folio.pending_asks().first()?.to_string();
        let line = c.folio.answer(&ask, true)?;
        let h = c.hosted.as_ref()?;
        h.send_line(&line).ok()?;
        select::release_lapsed(c);
        Some(())
    });
    let went = line.flatten().is_some();
    if went {
        redraw(id);
    }
    went
}

/// The Claude Code conversation session `id` holds, once its init said.
pub fn conversation(id: &str) -> Option<String> {
    with(id, |c| c.folio.session_id.clone()).flatten()
}

/// Whether session `id` goes on in a terminal because its stream could
/// not be read.
pub fn unreadable(id: &str) -> bool {
    with(id, |c| c.unreadable).unwrap_or(false)
}

/// The window events are routed through to the UI thread, made on first
/// use.
fn router() -> isize {
    let have = ROUTER.with(Cell::get);
    if have != 0 {
        return have;
    }
    let hwnd = unsafe {
        let Ok(instance) = GetModuleHandleW(None) else {
            return 0;
        };
        let wc = WNDCLASSW {
            lpfnWndProc: Some(router_proc),
            hInstance: instance.into(),
            lpszClassName: ROUTER_CLASS,
            ..Default::default()
        };
        RegisterClassW(&wc);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            ROUTER_CLASS,
            PCWSTR::null(),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        )
    };
    let id = hwnd.map_or(0, |h| h.0 as isize);
    ROUTER.with(|r| r.set(id));
    id
}

unsafe extern "system" fn router_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_FOLIO_LINES {
        drain();
        return LRESULT(0);
    }
    if msg == sign_in::WM_FOLIO_LOGIN {
        sign_in::tend();
        slash::tend();
        mcp::settle();
        return LRESULT(0);
    }
    if msg == WM_TIMER && wparam.0 == watch::WATCH_TIMER {
        watch::tick(hwnd);
        return LRESULT(0);
    }
    if msg == WM_TIMER && wparam.0 == mcp::MCP_TIMER {
        mcp::tick(hwnd);
        return LRESULT(0);
    }
    if msg == WM_TIMER && wparam.0 == SAVE_TIMER {
        let _ = KillTimer(Some(hwnd), SAVE_TIMER);
        let saved = CHATS.with(|c| {
            let Ok(mut chats) = c.try_borrow_mut() else {
                return false;
            };
            for chat in chats.values_mut().filter(|c| c.input.dirty()) {
                chat.input.save();
            }
            true
        });
        if !saved {
            save_soon();
        }
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Writes changed drafts a moment after the last change. Each call puts
/// the moment off again, so typing writes once it pauses.
/// Has [`drain`] look at every Folio soon, lines or not.
fn wake() {
    let router = router();
    if router != 0 {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(router as *mut c_void)),
                WM_FOLIO_LINES,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

fn save_soon() {
    let router = router();
    if router != 0 {
        unsafe {
            SetTimer(
                Some(HWND(router as *mut c_void)),
                SAVE_TIMER,
                SAVE_AFTER_MS,
                None,
            );
        }
    }
}

/// Feeds every Folio the events its reader left, redraws the ones that
/// changed, and tells the app of any whose tile should change.
fn drain() {
    let mut changed: Vec<(HWND, bool)> = Vec::new();
    let mut tell: Vec<Input> = Vec::new();
    let mut banished: Vec<String> = Vec::new();
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
                // A compaction in the history counts: once per quest.
                if (chat.folio.blocks.iter())
                    .any(|b| matches!(b, Block::Notice(Notice::Compacted { .. })))
                {
                    chat.compaction.compacted = true;
                }
                chat.tr.state.from = None;
                chat.loading = None;
                if let Some(said) = chat.greeting.take() {
                    chat.folio.note(&said);
                }
                moved = true;
            }
            // Review turned on in a claude whose `initialize` went without
            // its hook: it starts again to register it.
            let hooked = chat.folio.review_hooked || !chat.folio.initialized;
            if chat.input.review && !hooked && chat.between_turns() {
                rehook.push(id.clone());
            }
            let events: Vec<Event> = match chat.inbox.lock() {
                Ok(mut q) => q.drain(..).collect(),
                Err(_) => continue,
            };
            if events.is_empty() && !moved {
                continue;
            }
            // Before the lines, which may ask whether an edit waits.
            chat.folio.review = chat.input.review;
            let (cleared, live) = (chat.folio.cleared, chat.live);
            for event in events {
                // A replay's lines are old, and when they came is lost.
                chat.folio.clock = chat.live.then(unix_ms);
                match event {
                    Event::Out(l) => {
                        chat.compaction.feed(&l);
                        moved |= chat.folio.feed(&l);
                    }
                    Event::Sent(l) => moved |= chat.folio.feed_sent(&l),
                    Event::Dropped(n) => {
                        eprintln!("horadric: folio {id} lost its {n} oldest lines");
                    }
                    Event::Live => chat.live = true,
                    Event::End(_) => {
                        chat.folio.end();
                        // Before any init, nothing ran: stderr says why.
                        if chat.folio.session_id.is_none() && chat.error.is_none() {
                            let tail = (chat.hosted.as_ref())
                                .map(Hosted::stderr_tail)
                                .unwrap_or_default();
                            chat.error = Some(start_failure(&tail));
                        }
                        chat.ended = true;
                        moved = true;
                        if chat.to_terminal {
                            tell.push(Input::FolioHandedOver(id.clone()));
                        }
                        if chat.restarting {
                            tell.push(Input::FolioRestart(id.clone(), chat.restart_show));
                        }
                    }
                }
            }
            if chat.folio.unreadable() && !chat.unreadable {
                // Its words cannot be shown, so it may as well be cut off
                // mid turn: the terminal resumes the conversation.
                chat.unreadable = true;
                chat.rewinding = false;
                chat.error = Some(
                    "This Claude's stream could not be read, so it goes on in a terminal."
                        .to_string(),
                );
                moved = true;
                match (&chat.hosted, chat.ended) {
                    (Some(h), false) => {
                        chat.to_terminal = true;
                        h.close();
                    }
                    _ => tell.push(Input::FolioHandedOver(id.clone())),
                }
            }
            // A replay's answer was acted on when it came.
            if let Some(how) = chat.folio.files_rewound.take().filter(|_| chat.live) {
                tell.push(Input::FolioFilesRewound(id.clone(), how));
            }
            // A replay's limits are old news, and the bars already had them.
            if let Some(limits) = chat.folio.limits.take().filter(|_| chat.live) {
                tell.push(Input::Limits(limits));
            }
            if chat.folio.cleared != cleared {
                chat.open.clear();
                chat.tr.forget();
                chat.scroll = 0.0;
                chat.follow = true;
                // A replay only catches up: the page burns for a clear
                // the human sees happen.
                if live {
                    banished.push(id.clone());
                }
            }
            chat.flush();
            select::release_lapsed(chat);
            composer::tend(id, chat);
            mcp::tend(chat);
            // Before the state goes out, so the tile never sees the turn it
            // stops end as Done, which the runner would nudge.
            moved |= tend_compaction(chat);
            chat.note_busy();
            if banished.last() == Some(id) {
                // The runes behind flare up as the page burns, and cool.
                chat.heat_from = chat.heat().max(1.0);
                chat.busy_since = Instant::now();
            }
            let now = chat.state();
            if chat.told.as_ref() != Some(&now) {
                chat.told = Some(now.clone());
                tell.push(Input::FolioState(id.clone(), now));
            }
            let header = chat.header_moved();
            if let (true, Some(v)) = (moved || header, chat.view) {
                changed.push((v, header));
            }
            if let (true, Some(b)) = (moved || header, chat.bar) {
                changed.push((b, false));
            }
        }
        false
    });
    if busy {
        // Painting holds the Folios. Come back once it is done.
        unsafe {
            let _ = PostMessageW(
                Some(HWND(ROUTER.with(Cell::get) as *mut c_void)),
                WM_FOLIO_LINES,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
    for id in rehook {
        restart(&id, "Each Edit and Write now waits for you.", false);
    }
    // Before the repaint, which draws over the page that burns.
    for id in banished {
        view::banish(&id);
    }
    for (v, header) in changed {
        refresh(v, header);
    }
    for input in tell {
        app::push(input);
    }
}

/// What the error line says when claude.exe ended before it started, with
/// the last of what it wrote to stderr.
fn start_failure(tail: &[String]) -> String {
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

/// Redraws the conversation, and tells its pane when its header changed.
fn refresh(view: HWND, header: bool) {
    unsafe {
        let _ = InvalidateRect(Some(view), None, false);
        if header {
            if let Ok(pane) = GetParent(view) {
                let _ = PostMessageW(Some(pane), WM_FOLIO_REFRESH, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn redraw(id: &str) {
    let Some((view, bar, header)) = with(id, |c| (c.view, c.bar, c.header_moved())) else {
        return;
    };
    if let Some(b) = bar {
        refresh(b, false);
    }
    if let Some(v) = view {
        refresh(v, header);
    }
}

fn answer(id: &str, request_id: &str, allow: bool) {
    answer_with(id, |f| f.answer(request_id, allow));
}

/// Answers the CLI with the line `make` has the model write, if any.
fn answer_with(id: &str, make: impl FnOnce(&mut Folio) -> Option<String>) {
    with(id, |c| {
        let Some(line) = make(&mut c.folio) else {
            return;
        };
        if let Some(h) = &c.hosted {
            if let Err(e) = h.send_line(&line) {
                c.error = Some(format!("The answer did not arrive: {e}"));
            }
        }
        select::release_lapsed(c);
    });
    redraw(id);
}

/// Steps a quest's compaction on, see [`Watch::step`]. True when it sent
/// something, so the page shows it.
fn tend_compaction(chat: &mut Chat) -> bool {
    let m = Moment {
        quest: chat.quest,
        live: chat.live
            && chat.loading.is_none()
            && chat.hosted.is_some()
            && !(chat.ended || chat.to_terminal || chat.restarting || chat.winding),
        busy: chat.folio.busy,
        asking: chat.asking(),
        agents: chat.folio.agents_at_work(),
    };
    let went = match chat.compaction.step(&m) {
        None => return false,
        Some(Act::Interrupt) => chat.hosted.as_ref().is_some_and(|h| h.interrupt().is_ok()),
        Some(Act::Send(text)) => composer::prompt(chat, text),
    };
    if !went {
        chat.compaction.give_up();
    }
    went
}

/// Whether session `id` holds a quest, which only the app knows.
pub fn set_quest(id: &str, quest: bool) {
    with(id, |c| c.quest = quest);
}

fn interrupt(id: &str) {
    with(id, |c| {
        if let Some(h) = &c.hosted {
            let _ = h.interrupt();
        }
    });
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
