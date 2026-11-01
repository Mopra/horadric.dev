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
//! between them says what Claude does and holds the buttons an ask needs.
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

use horadric_core::folio::{AskState, Block, Folio, Priority, Reply};
use horadric_core::trust;
use horadric_pty::headless::Options;
use horadric_pty::headless_host::{Event, Hosted};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSBezelStyle, NSButton, NSEventModifierFlags, NSFont,
    NSFontAttributeName, NSFontWeightRegular, NSForegroundColorAttributeName, NSLineBreakMode,
    NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSResponder, NSScrollView,
    NSTextDelegate, NSTextField, NSTextView, NSTextViewDelegate, NSView,
};
use objc2_foundation::{
    NSAttributedString, NSDictionary, NSMutableAttributedString, NSObject, NSPoint, NSRange,
    NSRect, NSSize, NSString,
};
use serde_json::Value;

use crate::folio_session::{catch_up, history_file, start_failure, State, Tag};
use crate::folio_text::{self, Style};
use crate::layout::Rect;
use crate::theme::{self, Color};

use super::app::{self, Input};
use super::menu::{self, Item};
use super::paint::{cg_rect, ns_color, Painter};

/// The bar between the transcript and the composer.
const BAR_H: f64 = 30.0;
/// The composer, which scrolls past this many lines.
const COMPOSER_H: f64 = 76.0;
const PAD: f64 = 8.0;
const BUTTON_W: f64 = 112.0;
/// How many buttons the bar can show at once.
const BUTTONS: usize = 3;

/// What a bar button does now.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Act {
    Allow(String),
    Always(String),
    Deny(String),
    Approve(String),
    KeepPlanning(String),
    Answer(String),
    Stop,
    Trust,
}

impl Act {
    fn title(&self) -> &'static str {
        match self {
            Act::Allow(_) => "Allow",
            Act::Always(_) => "Always Allow",
            Act::Deny(_) => "Deny",
            Act::Approve(_) => "Approve Plan",
            Act::KeepPlanning(_) => "Keep Planning",
            Act::Answer(_) => "Answer\u{2026}",
            Act::Stop => "Stop",
            Act::Trust => "Trust Folder",
        }
    }
}

/// One Folio and the session behind it.
struct Chat {
    dir: PathBuf,
    folio: Folio,
    /// None when it did not start, or once it was let go.
    hosted: Option<Hosted>,
    /// Events from the reader thread.
    inbox: Arc<Mutex<VecDeque<Event>>>,
    /// A drain is queued, so a chatty stream posts once.
    posted: Arc<AtomicBool>,
    ended: bool,
    error: Option<String>,
    /// The host has sent everything it kept and goes on live.
    live: bool,
    /// A resumed conversation's history, read on a thread of its own:
    /// filled once read, None when there is none to wait for.
    loading: Option<Arc<Mutex<Option<Folio>>>>,
    /// Waits for the human to trust its folder, with what starts it then.
    /// Claude Code asks before it runs a folder's hooks and MCP servers,
    /// but not headless, so the Folio asks for it.
    pending: Option<(Tag, Options)>,
    /// What the app was last told of it.
    told: Option<State>,
    view: Option<Retained<FolioView>>,
}

impl Chat {
    fn new(dir: &Path) -> Chat {
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
        }
    }

    fn asking(&self) -> bool {
        !self.folio.pending_asks().is_empty() || !self.folio.pending_elicitations().is_empty()
    }

    /// It can take a prompt now: started, its replay and history in.
    fn ready(&self) -> bool {
        self.hosted.is_some() && self.live && self.loading.is_none() && !self.ended
    }

    fn state(&self) -> State {
        State {
            busy: self.folio.busy,
            asking: self.asking(),
            ended: self.ended,
            // The Mac has no terminal hand over or restart yet, so an end
            // it did not ask for is always one to resume from the tile.
            gone: self.ended,
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
        } else if self.error.is_some() || self.ended {
            "ended"
        } else if self.folio.busy {
            "working"
        } else {
            "chat"
        };
        match &self.folio.model {
            Some(m) => format!("{state} \u{b7} {m}"),
            None => state.to_string(),
        }
    }

    /// What the bar says, and the buttons it shows, oldest ask first.
    fn bar(&self) -> (String, Vec<Act>) {
        if self.pending.is_some() {
            return (
                "Claude Code has not been told to trust this folder. Its hooks and MCP servers run once you do."
                    .into(),
                vec![Act::Trust],
            );
        }
        if let Some(e) = &self.error {
            return (e.clone(), Vec::new());
        }
        if self.ended {
            return ("The session ended. Click its tile to resume it.".into(), Vec::new());
        }
        for b in &self.folio.blocks {
            match b {
                Block::Ask {
                    request_id,
                    tool,
                    state: AskState::Waiting,
                    ..
                } if !request_id.is_empty() => {
                    let id = request_id.clone();
                    return (
                        format!("Claude asks to use {tool}."),
                        vec![Act::Deny(id.clone()), Act::Always(id.clone()), Act::Allow(id)],
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
                _ => {}
            }
        }
        if !self.folio.pending_elicitations().is_empty() {
            return (
                "An MCP server asks something the Mac's Folio cannot show yet.".into(),
                vec![Act::Stop],
            );
        }
        if !self.live || self.loading.is_some() {
            return ("Reading the conversation\u{2026}".into(), Vec::new());
        }
        if self.folio.busy {
            let doing = self.folio.status.as_deref().unwrap_or("working");
            return (format!("Claude is {doing}\u{2026}"), vec![Act::Stop]);
        }
        (
            "Return sends, Shift+Return starts a new line, Esc stops Claude.".into(),
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

    fn send(&mut self, line: Option<String>) {
        let (Some(line), Some(h)) = (line, &self.hosted) else {
            return;
        };
        if let Err(e) = h.send_line(&line) {
            self.error = Some(format!("Claude did not take it: {e}"));
        }
    }
}

thread_local! {
    static CHATS: RefCell<HashMap<String, Chat>> = RefCell::new(HashMap::new());
}

fn with<T>(id: &str, f: impl FnOnce(&mut Chat) -> T) -> Option<T> {
    CHATS.with(|c| c.try_borrow_mut().ok()?.get_mut(id).map(f))
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
    std::thread::spawn(move || {
        use std::io::Read;
        let mut bytes = Vec::new();
        if let Ok(file) = std::fs::File::open(&path) {
            let _ = file.take(limit).read_to_end(&mut bytes);
        }
        let folio = Folio::from_transcript_at(&String::from_utf8_lossy(&bytes), None);
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
/// so in its pane first.
pub fn open(id: &str, dir: &Path, mut tag: Tag, options: &Options) -> Result<(), String> {
    if CHATS.with(|c| c.borrow().contains_key(id)) {
        return Ok(());
    }
    let mut chat = Chat::new(dir);
    if let Some(conversation) = tag.resume.clone() {
        // `--resume` streams only the new turns: what came before is read
        // from the transcript, and how much there was goes in the tag.
        if let Some((path, len)) = history_file(dir, &conversation) {
            tag.history = len;
            load_history(&mut chat, path, len);
        }
    }
    if !trusted(dir) {
        chat.pending = Some((tag, options.clone()));
        CHATS.with(|c| c.borrow_mut().insert(id.to_string(), chat));
        return Ok(());
    }
    start_host(id, &mut chat, &tag, options)?;
    CHATS.with(|c| c.borrow_mut().insert(id.to_string(), chat));
    Ok(())
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
        let mut chat = Chat::new(&info.cwd);
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
        c.send(Some(line));
        true
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
fn drain() {
    let mut tell: Vec<Input> = Vec::new();
    let mut changed: Vec<Retained<FolioView>> = Vec::new();
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
                moved = true;
            }
            let events: Vec<Event> = match chat.inbox.lock() {
                Ok(mut q) => q.drain(..).collect(),
                Err(_) => continue,
            };
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
                        if chat.folio.session_id.is_none() && chat.error.is_none() {
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
            if chat.folio.unreadable() && chat.error.is_none() {
                chat.error = Some(
                    "This Claude's stream could not be read. End the session and start it with New CLI Session."
                        .to_string(),
                );
                moved = true;
            }
            chat.flush();
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
    for v in changed {
        v.refresh();
    }
    for input in tell {
        app::input(input);
    }
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
        Act::Allow(r) => answer(id, |f| f.reply(&r, Reply::Allow)),
        Act::Always(r) => answer(id, |f| f.reply(&r, Reply::Always)),
        Act::Deny(r) => {
            let why = view.take_composer();
            answer(id, |f| f.reply(&r, Reply::Deny(&why)))
        }
        Act::Approve(r) => answer(id, |f| f.approve_plan(&r, None)),
        Act::KeepPlanning(r) => {
            let feedback = view.take_composer();
            answer(id, |f| f.keep_planning(&r, &feedback))
        }
        Act::Answer(r) => {
            let Some(picks) = pick_answers(id, &r, view) else {
                return;
            };
            answer(id, |f| f.answer_question(&r, &picks))
        }
        Act::Stop => stop(id),
        Act::Trust => trust_folder(id),
    }
    view.refresh();
}

fn answer(id: &str, make: impl FnOnce(&mut Folio) -> Option<String>) {
    with(id, |c| {
        let line = make(&mut c.folio);
        c.send(line);
    });
    super::queue::post(drain);
}

/// Stops the turn running now. The CLI ends it with a `result` and stays
/// up for the next prompt.
fn stop(id: &str) {
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

/// The options of each of a question ask's questions, picked from a menu
/// at the bar's button, one menu a question. None when one was dismissed.
fn pick_answers(id: &str, request_id: &str, view: &FolioView) -> Option<Vec<Vec<String>>> {
    let questions = with(id, |c| {
        c.folio.blocks.iter().find_map(|b| match b {
            Block::Question {
                request_id: r,
                questions,
                ..
            } if r == request_id => Some(questions.clone()),
            _ => None,
        })
    })
    .flatten()?;
    let mtm = MainThreadMarker::from(view);
    let at = view.bar_point();
    let mut picks = Vec::new();
    for q in &questions {
        let mut items = vec![Item::disabled(q.question.clone()), Item::Separator];
        for (i, o) in q.options.iter().enumerate() {
            items.push(Item::pick(o.label.clone(), i + 1));
        }
        let picked = menu::popup(mtm, &items, at)?;
        let label = q.options.get(picked.checked_sub(1)?)?.label.clone();
        picks.push(vec![label]);
    }
    Some(picks)
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
    composer: Retained<NSTextView>,
    status: Retained<NSTextField>,
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
            } else {
                false
            }
        }
    }
);

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

        let composer_scroll = NSTextView::scrollableTextView(mtm);
        let composer = text_view_of(&composer_scroll);
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
        p.status.setFrame(NSRect::new(
            NSPoint::new(PAD, bar_y + 7.0),
            NSSize::new((x - 2.0 * PAD).max(0.0), BAR_H - 12.0),
        ));
    }

    /// Shows the session as it stands now.
    fn refresh(&self) {
        let id = self.ivars().id.borrow().clone();
        let Some((paragraphs, (said, acts))) =
            with(&id, |c| (folio_text::paragraphs(&c.folio), c.bar()))
        else {
            return;
        };
        if let Some(p) = self.ivars().parts.borrow().as_ref() {
            p.status.setStringValue(&NSString::from_str(&said));
            for (i, b) in p.buttons.iter().enumerate() {
                match acts.get(i) {
                    Some(a) => {
                        b.setTitle(&NSString::from_str(a.title()));
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
        if text.trim().is_empty() {
            return;
        }
        let id = self.ivars().id.borrow().clone();
        if prompt(&id, text.trim_end()) {
            self.take_composer();
            self.refresh();
        }
    }

    fn composer_text(&self) -> String {
        (self.ivars().parts.borrow().as_ref())
            .map(|p| p.composer.string().to_string())
            .unwrap_or_default()
    }

    /// What the composer holds, which it no longer does.
    fn take_composer(&self) -> String {
        let text = self.composer_text();
        if let Some(p) = self.ivars().parts.borrow().as_ref() {
            p.composer.setString(&NSString::from_str(""));
        }
        text.trim().to_string()
    }

    /// Below the bar's buttons on the screen, where a menu they open goes.
    fn bar_point(&self) -> NSPoint {
        let b = self.bounds();
        let y = (b.size.height - COMPOSER_H - PAD).max(0.0);
        let in_window = self.convertPoint_toView(NSPoint::new(b.size.width - PAD - BUTTON_W, y), None);
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
        Style::Step | Style::Shell => (mono_font(), theme::text_dim()),
        Style::StepFailed => (mono_font(), theme::error()),
        Style::Ask => (body_font(), theme::waiting()),
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
    for para in paragraphs {
        let (font, color) = look(para.style);
        let style = NSMutableParagraphStyle::new();
        style.setParagraphSpacing(match para.style {
            Style::Step | Style::StepFailed => 2.0,
            _ => 10.0,
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
