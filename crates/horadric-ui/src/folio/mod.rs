//! The Folio: a headless Claude session's conversation drawn as a chat, not
//! a terminal. Experimental, a session kind beside the terminal ones: a
//! tile in its project's cluster and a pane on the stage. See
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
//! Behind the conversation runes drift up through a glow in the project's
//! colour. They warm while Claude works and cool to rest when it stops,
//! and at rest, or hidden, nothing redraws.
//!
//! Here is the session: its registry by session id, its lifecycle and
//! what reaches the CLI. [`view`] is the window, its paint loop and input,
//! [`transcript`] lays out and draws the blocks, [`composer`] the input
//! box and its buttons, and [`backdrop`] the background.

mod backdrop;
mod cards;
mod composer;
mod controls;
mod editor;
mod flow;
mod picker;
mod rich;
mod select;
mod transcript;
mod view;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use horadric_core::folio::Folio;
use horadric_pty::headless::Options;
use horadric_pty::headless_host::{Event, Hosted};
use serde::{Deserialize, Serialize};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetParent, PostMessageW, RegisterClassW, HWND_MESSAGE,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_USER, WNDCLASSW,
};

use crate::app::{self, Input};
use crate::folio_look as look;

pub use view::{attach, char, key, place, register_class, wheel};
/// Posted to the pane a Folio is in when what its header says may have
/// changed.
pub const WM_FOLIO_REFRESH: u32 = WM_USER + 9;
/// Posted to the router when events wait in some Folio's inbox.
const WM_FOLIO_LINES: u32 = WM_USER + 10;

const ROUTER_CLASS: PCWSTR = w!("HoradricFolioRouter");

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
}

/// One Folio and the session behind it.
struct Chat {
    /// The project's key, whose colour it glows in.
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
    input: composer::Input,
    scroll: f32,
    /// Keeps the end in view as it streams, until scrolled away from.
    follow: bool,
    content_h: f32,
    view_h: f32,
    /// Blocks unfolded by a click, by index.
    open: HashSet<usize>,
    busy_was: bool,
    busy_since: Instant,
    /// What the pane's header was last told, so it is told again only
    /// when that changes.
    header: String,
    /// What the app was last told of it: working, asking, ended.
    told: Option<State>,
    /// The host has sent everything it kept and goes on live.
    live: bool,
    /// The session goes on in a terminal once claude.exe has ended.
    to_terminal: bool,
    /// Waits for the human to trust its folder, with what starts it then.
    /// Claude Code asks before it runs a folder's hooks and MCP servers,
    /// but not headless, so the Folio asks for it.
    pending: Option<(Tag, Options)>,
    /// The transcript's own state and what it keeps between frames.
    tr: transcript::Ui,
}

/// What the app hears of a Folio, for its tile's phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    pub busy: bool,
    /// An ask waits on the human.
    pub asking: bool,
    pub ended: bool,
    /// The host's replay is in, so this is where the session stands.
    pub live: bool,
    /// The last turn ended, its reply not yet followed by a prompt.
    pub finished: bool,
    /// How many background tasks run, once the CLI has said.
    pub background: Option<usize>,
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
            input: composer::Input::new(),
            scroll: 0.0,
            follow: true,
            content_h: 0.0,
            view_h: 0.0,
            open: HashSet::new(),
            busy_was: false,
            // Long ago, so a Folio opened at rest starts cold.
            busy_since: Instant::now() - look::COOL,
            header: String::new(),
            told: None,
            live: false,
            pending: None,
            to_terminal: false,
            tr: transcript::Ui::default(),
        }
    }

    fn note_busy(&mut self) {
        if self.folio.busy != self.busy_was {
            self.busy_was = self.folio.busy;
            self.busy_since = Instant::now();
        }
    }

    fn heat(&self) -> f32 {
        look::heat(self.folio.busy, self.busy_since.elapsed())
    }

    fn state(&self) -> State {
        State {
            busy: self.folio.busy,
            asking: !self.folio.pending_asks().is_empty(),
            ended: self.ended,
            live: self.live,
            finished: self.folio.turn_finished(),
            background: (self.folio.background_heard).then_some(self.folio.background.len()),
        }
    }

    fn detail(&self) -> String {
        let state = if self.to_terminal {
            "opening in a terminal"
        } else if self.error.is_some() || self.ended {
            "ended"
        } else if self.folio.busy {
            "working"
        } else {
            "chat, experimental"
        };
        match &self.folio.model {
            Some(m) => format!("{state} \u{b7} {m}"),
            None => state.to_string(),
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

/// The transcript of `conversation` held in `dir`, up to `limit` bytes
/// when given: the history a resumed Folio shows above what it streams.
fn history(dir: &Path, conversation: &str, limit: Option<u64>) -> Option<String> {
    let path = horadric_hooks::transcript::path(&dir.to_string_lossy(), conversation)?;
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(limit.unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Starts session `id`, a headless Claude in `dir` in a host of its own,
/// resuming the conversation `tag.resume` when it names one. `options`
/// carry the flags and the environment its hooks find it by. In a folder
/// Claude Code has not been told to trust, it waits for the human to say
/// so in its pane first.
pub fn open(id: &str, dir: &Path, mut tag: Tag, options: &Options) -> Result<(), String> {
    if CHATS.with(|c| c.borrow().contains_key(id)) {
        return Ok(());
    }
    let mut chat = Chat::new(&tag.key, dir);
    if let Some(conversation) = tag.resume.clone() {
        // `--resume` streams only the new turns: what came before is read
        // from the transcript, and how much there was goes in the tag.
        if let Some(text) = history(dir, &conversation, None) {
            tag.history = text.len() as u64;
            chat.folio.replay_transcript(&text);
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
    .map_err(|e| format!("Claude did not start: {e}"))?;
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
        let hosted = match Hosted::attach(&horadric_hooks::instance(), &id, events) {
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
        if let Some(conversation) = &tag.resume {
            if let Some(text) = history(&info.cwd, conversation, Some(tag.history)) {
                chat.folio.replay_transcript(&text);
            }
        }
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
    let gone = CHATS.with(|c| c.borrow_mut().remove(id));
    if let Some(h) = gone.as_ref().and_then(|c| c.hosted.as_ref()) {
        h.close();
    }
}

/// Ends session `id` once it is between turns, so it can go on in a
/// terminal: the app hears [`Input::FolioHandedOver`] once
/// claude.exe has ended. False while it works or an ask waits.
pub fn hand_over(id: &str) -> bool {
    with(id, |c| {
        if c.folio.busy || !c.folio.pending_asks().is_empty() || c.to_terminal {
            return false;
        }
        let Some(h) = &c.hosted else {
            return false;
        };
        c.to_terminal = true;
        h.close();
        true
    })
    .unwrap_or(false)
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

/// What the pane's header says beside its name.
pub fn detail(id: &str) -> String {
    with(id, |c| c.detail()).unwrap_or_default()
}

/// Whether claude.exe still runs for session `id`.
pub fn running(id: &str) -> bool {
    with(id, |c| {
        !c.ended && c.hosted.as_ref().is_some_and(Hosted::running)
    })
    .unwrap_or(false)
}

/// The Claude Code conversation session `id` holds, once its init said.
pub fn conversation(id: &str) -> Option<String> {
    with(id, |c| c.folio.session_id.clone()).flatten()
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
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Feeds every Folio the events its reader left, redraws the ones that
/// changed, and tells the app of any whose tile should change.
fn drain() {
    let mut changed: Vec<(HWND, bool)> = Vec::new();
    let mut tell: Vec<Input> = Vec::new();
    let busy = CHATS.with(|c| {
        let Ok(mut chats) = c.try_borrow_mut() else {
            return true;
        };
        for (id, chat) in chats.iter_mut() {
            // Before the drain, so an event after it posts again.
            chat.posted.store(false, Ordering::Release);
            let events: Vec<Event> = match chat.inbox.lock() {
                Ok(mut q) => q.drain(..).collect(),
                Err(_) => continue,
            };
            if events.is_empty() {
                continue;
            }
            let mut moved = false;
            for event in events {
                match event {
                    Event::Out(l) => moved |= chat.folio.feed(&l),
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
                    }
                }
            }
            chat.flush();
            composer::tend(id, chat);
            chat.note_busy();
            let now = chat.state();
            if chat.told.replace(now) != Some(now) {
                tell.push(Input::FolioState(id.clone(), now));
            }
            let header = chat.header_moved();
            if let (true, Some(v)) = (moved || header, chat.view) {
                changed.push((v, header));
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
        return "Claude ended before it started.".to_string();
    }
    let said: Vec<&str> = last.into_iter().rev().collect();
    format!("Claude ended before it started: {}", said.join(" "))
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
    if let Some((Some(v), header)) = with(id, |c| (c.view, c.header_moved())) {
        refresh(v, header);
    }
}

fn send_prompt(id: &str) {
    with(id, |c| composer::send(c, false));
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
                c.error = Some(format!("The answer did not reach Claude: {e}"));
            }
        }
    });
    redraw(id);
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

    #[test]
    fn start_failure_quotes_the_last_lines_of_stderr() {
        assert_eq!(start_failure(&[]), "Claude ended before it started.");
        let tail: Vec<String> = ["a", "", "b", "c", "d  "].map(String::from).to_vec();
        assert_eq!(
            start_failure(&tail),
            "Claude ended before it started: b c d"
        );
    }

    #[test]
    fn a_tag_reads_back_and_an_old_one_still_reads() {
        let tag = Tag {
            key: "c:/p".into(),
            name: "p".into(),
            resume: Some("abc".into()),
            history: 42,
        };
        let json = serde_json::to_string(&tag).unwrap();
        assert_eq!(serde_json::from_str::<Tag>(&json).unwrap(), tag);
        let bare: Tag = serde_json::from_str(r#"{"key":"k","name":"n"}"#).unwrap();
        assert_eq!((bare.resume, bare.history), (None, 0));
    }
}
