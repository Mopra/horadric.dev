//! The Mac app: the registry the hooks feed, the consoles, and the windows
//! that show them, all on the main thread.
//!
//! Everything that changes the app arrives as an [`Input`], queued and
//! handled one at a time, never inside another. A menu or a dialog runs a
//! loop of its own while the app is busy with the input that opened it;
//! what arrives meanwhile waits in the queue until that input is done.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use horadric_core::agent::Agent;
use horadric_core::{HookEvent, Phase, Registry, Route, SavedSession, SavedState};
use horadric_hooks::listener::{self, Command, Reload, Tagged};
use horadric_hooks::{transcript, TASKS_ENV};
use objc2::{AnyThread, MainThreadMarker, Message};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSScreen};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::console::{self, Console, Launch, Note};
use crate::keys::FontStep;
use crate::layout::{self, Hit, Metrics};
use crate::project::{folder_key, project_key, project_name};
use crate::{shell, store, theme};

use super::cluster::{main_screen_height, Cluster};
use super::look::Scene;
use super::menu::{self, Item, Status};
use super::stage::{self, Pane, Stage};
use super::{autostart, dialog, path, queue, snapshot, update};
use crate::quests::Runner as _;

#[path = "questing.rs"]
mod questing;

/// Between a tile column's windows, and round the edge of the screen.
const GAP: f32 = 8.0;
/// How long an ended session's tile stays before it goes.
const ENDED_LINGER: Duration = Duration::from_secs(4);
/// How many folders the start menu remembers.
const RECENT: usize = 10;

/// Everything that changes the app.
pub enum Input {
    /// A part of a cluster clicked, at this point on the screen.
    Cluster(String, Hit, NSPoint),
    /// A part of a cluster right clicked.
    ClusterMenu(String, Hit, NSPoint),
    /// A pane's cross clicked, for this session.
    PaneClose(String),
    /// A pane right clicked.
    PaneMenu(String, NSPoint),
    /// Another pane has the keyboard.
    Focused(String),
    /// A new plain terminal in the project with this key.
    Shell(String),
    Font(FontStep),
    /// A menu bar item, by its tag.
    Menu(usize),
    /// The registry changed.
    Events {
        phase: bool,
    },
    /// The listener has commands queued.
    Commands,
    Console(Note, usize),
    Tick,
    /// The relay link, a brother's ask or the sign in said something.
    Kin(super::brothers::Note),
    /// The brothers' server looks for what changed.
    KinTick,
    /// A Folio's stream moved its tile: busy, asking, ended.
    FolioState(String, crate::folio_session::State),
    /// A Folio asks for what only the app does: to go on in the same tile
    /// once its claude ended, to show a file, to end.
    Folio(String, super::folio::Ask),
    /// A files tile's scan came back, or its folder changed.
    Files(super::files::Note),
    /// A files tile's booked scan is due.
    FilesDue(String),
    /// The wheel over a files tile: points or lines, and which.
    FilesWheel(String, f64, bool),
    /// A control in the Settings window was used.
    Setting(super::settings::Pick),
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static QUEUE: RefCell<Vec<Input>> = const { RefCell::new(Vec::new()) };
}

/// Queues `input` and handles the queue when the app is free.
pub fn input(i: Input) {
    QUEUE.with(|q| q.borrow_mut().push(i));
    queue::post(drain);
}

fn drain() {
    loop {
        let Some(next) = QUEUE.with(|q| {
            let mut q = q.borrow_mut();
            (!q.is_empty()).then(|| q.remove(0))
        }) else {
            return;
        };
        let handled = APP.with(|a| match a.try_borrow_mut() {
            Ok(mut slot) => {
                if let Some(app) = slot.as_mut() {
                    app.handle(next);
                }
                true
            }
            Err(_) => {
                // Busy with the input that opened a menu or a dialog: this
                // one waits until that is done.
                QUEUE.with(|q| q.borrow_mut().insert(0, next));
                false
            }
        });
        if !handled {
            return;
        }
    }
}

/// Menu tags. Recent folders start at [`RECENT_TAG`].
const NEW_SESSION: usize = 1;
pub(crate) const NEW_SESSION_TAG: usize = NEW_SESSION;
const SHOW_STAGE: usize = 2;
pub(crate) const SHOW_STAGE_TAG: usize = SHOW_STAGE;
const FRONT: usize = 3;
pub(crate) const FRONT_TAG: usize = FRONT;
const LOGIN: usize = 4;
const CHECK_UPDATE: usize = 7;
const INSTALL_UPDATE: usize = 8;
const ABOUT: usize = 9;
const QUIT: usize = 10;
const NEW_TERMINAL: usize = 11;
const NEXT_WAITING: usize = 12;
/// Claude in a terminal, where New Session opens a Folio.
const NEW_CLI_SESSION: usize = 13;
const SETTINGS: usize = 14;
const USAGE: usize = 15;
const THEME_TAG: usize = 50;
const DISCORD_TAG: usize = 60;
const RECENT_TAG: usize = 100;

/// How a console is started.
enum Run {
    Agent(Agent),
    Shell,
}

pub struct App {
    mtm: MainThreadMarker,
    pub(super) registry: Arc<Mutex<Registry>>,
    pub(super) consoles: HashMap<String, Arc<Console>>,
    next_serial: usize,
    /// Sessions whose agent is not running, by id, as saved.
    pub(super) paused: HashMap<String, SavedSession>,
    pub(super) clusters: Vec<Cluster>,
    /// The order projects stand in the column, by key.
    order: Vec<String>,
    collapsed: HashSet<String>,
    stage: Stage,
    status: Status,
    requests: Arc<Mutex<Vec<Command>>>,
    pub(super) recent: Vec<String>,
    status_settings: Option<PathBuf>,
    font_size: f32,
    last_saved: Option<SavedState>,
    /// The sessions that were waiting at the last reconcile, to tell one
    /// that just started.
    waiting: HashSet<String>,
    /// Sessions ended by hand whose exit has not arrived yet. Their kill
    /// exits nonzero, which must not pause them.
    ending: HashSet<String>,
    quit: bool,
    frozen: bool,
    update: update::Updater,
    /// What the Mac keeps of its brothers, the server they ask included.
    pub(super) kin: super::brothers::Kin,
    /// Each Folio's own arguments, as a chat takes them, for its resume.
    folio_args: HashMap<String, Vec<String>>,
    /// Folios reattached whose replay has not said where they stand yet.
    resync: HashSet<String>,
    /// "Show on Discord", the client while it is on, and the run of work
    /// the profile counts up from.
    discord: horadric_core::saved::Discord,
    rich: Option<crate::discord::Discord>,
    run: horadric_core::presence::Run,
    /// Files shown read only on the stage, by pane id: the project each
    /// is shown with, its console, and when the file last changed.
    views: HashMap<String, View>,
    /// Each project's files tile.
    files: super::files::Files,
    /// The model, effort and permission mode every session starts with,
    /// unless its own arguments say otherwise.
    defaults: horadric_core::Defaults,
    /// The account's limits as last heard from a session's status line,
    /// or from a brother's.
    pub(super) usage: Arc<Mutex<Option<horadric_core::Usage>>>,
    /// The quest logs, their runner and Warriv.
    quests: questing::Quests,
}

/// A file shown read only on the stage, a pane with no program behind it
/// and no tile.
struct View {
    key: String,
    console: Arc<Console>,
    changed: Option<SystemTime>,
}

/// Runs the app until it quits.
pub fn run(port: u16, reload: bool) -> Result<(), String> {
    if std::net::TcpStream::connect_timeout(
        &([127, 0, 0, 1], port).into(),
        Duration::from_millis(200),
    )
    .is_ok()
    {
        return Err(format!("Horadric is already running (port {port} answers)"));
    }
    let mtm = MainThreadMarker::new().ok_or("the app must run on the main thread")?;
    // Before any thread starts: a Mac app from Finder has launchd's bare
    // PATH, which would never find `claude`.
    path::adopt_login_path();

    let ns_app = NSApplication::sharedApplication(mtm);
    ns_app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    // A build run from `target` has no bundle to take an icon from.
    set_dock_icon(&ns_app);
    super::delegate::install(mtm);

    store::trim_journal(unix_now());
    let saved = store::load();
    if let Some(t) = saved.theme.as_deref() {
        theme::set(theme::Theme::from_key(Some(t)));
    }
    theme::set_accents(&saved.accents);
    let mut registry = Registry::new();
    let now = SystemTime::now();
    for s in &saved.sessions {
        registry.add(s.to_session(now));
    }
    let registry = Arc::new(Mutex::new(registry));
    let requests: Arc<Mutex<Vec<Command>>> = Arc::new(Mutex::new(Vec::new()));
    let usage = Arc::new(Mutex::new(saved.usage.clone()));
    listen(
        port,
        Arc::clone(&registry),
        Arc::clone(&requests),
        Arc::clone(&usage),
    );

    let how = saved.carry(reload);
    let carry = if how.resumes() {
        saved.running_ids()
    } else {
        Vec::new()
    };
    let font_size = saved.font_size.unwrap_or(stage::FONT_DEFAULT);
    let stage = Stage::new(mtm, default_stage_frame(mtm, &saved), font_size);
    let status = Status::new(mtm);
    let status_settings = store::write_status_settings(&console::host_program());
    let mut app = App {
        mtm,
        registry,
        consoles: HashMap::new(),
        next_serial: 1,
        paused: saved
            .sessions
            .iter()
            .map(|s| (s.id.clone(), s.clone()))
            .collect(),
        clusters: Vec::new(),
        order: saved.columns.concat(),
        collapsed: saved
            .clusters
            .iter()
            .filter(|c| c.collapsed)
            .map(|c| c.key.clone())
            .collect(),
        stage,
        status,
        requests,
        recent: saved.recent.clone(),
        status_settings,
        font_size,
        last_saved: Some(saved.clone()),
        waiting: HashSet::new(),
        ending: HashSet::new(),
        quit: false,
        frozen: false,
        update: update::Updater::new(saved.update_told.clone()),
        kin: super::brothers::Kin::default(),
        folio_args: HashMap::new(),
        resync: HashSet::new(),
        discord: saved.discord,
        rich: None,
        run: Default::default(),
        views: HashMap::new(),
        quests: questing::Quests::new(&saved),
        files: super::files::Files::folded(
            (saved.clusters.iter())
                .filter(|c| c.files_collapsed)
                .map(|c| c.key.clone())
                .collect(),
        ),
        defaults: saved.defaults.clone(),
        usage,
    };
    app.attach_hosts();
    app.reattach_folios();
    for id in &carry {
        let heard = app
            .registry
            .lock()
            .map(|r| r.get(id).is_some_and(|s| s.phase != Phase::Paused))
            .unwrap_or(true);
        if !heard {
            if let Err(e) = app.resume(id, false) {
                eprintln!("horadric: cannot resume {id}: {e}");
            }
        }
    }
    app.reconcile();
    app.save();
    if let Some(crash) = crate::crashlog::take_last_crash() {
        let log = crate::crashlog::log_path()
            .map_or_else(|| "horadric.log".to_string(), |p| p.display().to_string());
        // Once the run loop turns, so the stage is up behind the alert
        // rather than held back by it.
        queue::after_main(Duration::from_secs(1), move || {
            dialog::error(
                "Horadric closed unexpectedly last time",
                &format!("{crash}\nThe log is in {log}"),
            );
        });
    }
    app.menus();
    if let Some(key) = saved.on_stage.as_deref().and_then(|id| app.project_of(id)) {
        app.show_project(&key, saved.on_stage.as_deref());
    } else if app.clusters.is_empty() {
        // Nothing to show yet: the stage says how to begin.
        app.stage.present();
    }
    APP.with(|a| *a.borrow_mut() = Some(app));
    // It waits for a sign in before it dials, so a Mac that never joins a
    // brotherhood never reaches the relay.
    super::brothers::start();
    tick_soon();
    snapshot::start();

    ns_app.activate();
    ns_app.run();
    crate::crashlog::clean_exit();
    APP.with(|a| {
        if let Some(mut app) = a.borrow_mut().take() {
            app.freeze();
        }
    });
    Ok(())
}

/// The rune stone in the Dock, red for a dev instance.
fn set_dock_icon(app: &NSApplication) {
    let Some(png) = crate::icon::png(512, horadric_hooks::dev()) else {
        return;
    };
    let data = objc2_foundation::NSData::with_bytes(png);
    if let Some(image) =
        objc2_app_kit::NSImage::initWithData(objc2_app_kit::NSImage::alloc(), &data)
    {
        unsafe { app.setApplicationIconImage(Some(&image)) };
    }
}

fn tick_soon() {
    queue::after_main(Duration::from_secs(1), || {
        input(Input::Tick);
        tick_soon();
    });
}

/// The listener on its thread, and the feeder that applies its events to
/// the registry and wakes the app.
fn listen(
    port: u16,
    registry: Arc<Mutex<Registry>>,
    requests: Arc<Mutex<Vec<Command>>>,
    usage: Arc<Mutex<Option<horadric_core::Usage>>>,
) {
    let (tx, rx) = mpsc::channel::<Tagged>();
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
    thread::spawn(move || {
        if let Err(e) = listener::serve(port, tx, Some(cmd_tx)) {
            eprintln!("horadric: listener stopped: {e}");
        }
    });
    thread::spawn(move || {
        for t in rx {
            // The Mac has no sanctuary to light yet.
            if t.event.hook_event_name == HookEvent::GLOW {
                continue;
            }
            // The limits are the account's, so every session's reading
            // counts, the truer of each kept, as on Windows.
            if let Some(limits) = t.limits.as_ref().filter(|l| !l.is_empty()) {
                let now = unix_now();
                if let Ok(mut u) = usage.lock() {
                    let merged = match u.as_ref() {
                        Some(held) => held.limits.merge(limits, now),
                        None => limits.clone(),
                    };
                    *u = Some(horadric_core::Usage {
                        limits: merged,
                        at: now,
                    });
                }
            }
            let target =
                registry
                    .lock()
                    .ok()
                    .and_then(|r| match r.route(&t.horadric_id, &t.event) {
                        Route::To(id) => Some(id),
                        Route::Stranger => None,
                    });
            let Some(target) = target else {
                continue;
            };
            let changed = registry
                .lock()
                .map(|mut r| r.apply(&target, &t.event, SystemTime::now()))
                .unwrap_or(false);
            queue::post(move || input(Input::Events { phase: changed }));
        }
    });
    thread::spawn(move || {
        for c in cmd_rx {
            if let Ok(mut q) = requests.lock() {
                q.push(c);
            }
            queue::post(|| input(Input::Commands));
        }
    });
}

/// The visible part of the main screen, as left, top, width and height
/// with the top measured down from the top of the screen.
fn work_area(mtm: MainThreadMarker) -> (f32, f32, f32, f32) {
    let screen_h = main_screen_height(mtm);
    let Some(screen) = NSScreen::screens(mtm).firstObject() else {
        return (0.0, 25.0, 1440.0, 875.0);
    };
    let v = screen.visibleFrame();
    let top = screen_h - (v.origin.y + v.size.height);
    (
        v.origin.x as f32,
        top as f32,
        v.size.width as f32,
        v.size.height as f32,
    )
}

/// Where the stage goes: where it was, or right of one column of tiles,
/// as tall as the screen allows.
fn default_stage_frame(mtm: MainThreadMarker, saved: &SavedState) -> NSRect {
    if let Some([x, y, w, h]) = saved.stage {
        if w > 200 && h > 200 {
            return NSRect::new(
                NSPoint::new(x as f64, y as f64),
                NSSize::new(w as f64, h as f64),
            );
        }
    }
    let (left, top, w, h) = work_area(mtm);
    let x = left + Metrics::default().width + 2.0 * GAP;
    let width = (w - (x - left) - GAP).max(600.0);
    let screen_h = main_screen_height(mtm) as f32;
    let bottom = screen_h - (top + h) + GAP;
    NSRect::new(
        NSPoint::new(x as f64, bottom as f64),
        NSSize::new(width as f64, (h - 2.0 * GAP) as f64),
    )
}

/// When the file at `path` last changed, None when it cannot be read.
fn file_changed(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl App {
    fn handle(&mut self, i: Input) {
        match i {
            Input::Cluster(key, hit, at) => self.cluster_click(&key, hit, at),
            Input::ClusterMenu(key, hit, at) => self.cluster_menu(&key, hit, at),
            Input::PaneClose(id) => self.close_pane(&id),
            Input::PaneMenu(id, at) => self.pane_menu(&id, at),
            Input::Focused(id) => self.identify(&id),
            Input::Shell(key) => self.open_shell(&key),
            Input::Font(step) => {
                self.font_size = self.stage.set_font(step);
                self.save();
            }
            Input::Menu(tag) => self.menu_pick(tag),
            Input::Events { phase } => {
                if phase {
                    self.reconcile();
                } else {
                    self.redraw_clusters();
                }
            }
            Input::Commands => self.commands(),
            Input::Console(Note::Output, serial) => self.output(serial),
            Input::Console(Note::Exit, serial) => self.exited(serial),
            Input::Tick => self.tick(),
            Input::Kin(note) => super::brothers::heard(self, note),
            Input::KinTick => super::brothers::tick(self),
            Input::FolioState(id, state) => self.folio_state(&id, state),
            Input::Folio(id, ask) => self.folio_ask(&id, ask),
            Input::Files(note) => {
                if self.files.heard(note) {
                    self.reconcile();
                }
            }
            Input::FilesDue(key) => self.files.due(&key),
            Input::Setting(pick) => self.setting(pick),
            Input::FilesWheel(key, dy, precise) => {
                if self.files.wheel(&key, dy, precise) {
                    self.reconcile();
                }
            }
        }
    }

    fn console_by_serial(&self, serial: usize) -> Option<&Arc<Console>> {
        self.consoles
            .values()
            .find(|c| c.serial == serial)
            .or_else(|| self.kin.far.console_by_serial(serial))
    }

    pub(super) fn project_of(&self, id: &str) -> Option<String> {
        let r = self.registry.lock().ok()?;
        r.get(id).map(project_key)
    }

    /// The folder new sessions in the project with this key start in: a
    /// session's own, which keeps the case the key lost, or the key.
    pub(super) fn project_dir(&self, key: &str) -> Option<PathBuf> {
        let from_session = self.registry.lock().ok().and_then(|r| {
            r.all()
                .filter(|s| project_key(s) == key)
                .map(|s| PathBuf::from(&s.cwd))
                .find(|p| p.is_dir())
        });
        from_session
            .or_else(|| {
                self.recent
                    .iter()
                    .find(|r| folder_key(r) == key)
                    .map(PathBuf::from)
            })
            .or_else(|| Some(PathBuf::from(key)).filter(|p| p.is_dir()))
    }

    // Sessions.

    fn unique_id(&self, base: &str) -> String {
        let taken = |id: &str| {
            self.consoles.contains_key(id)
                || self.paused.contains_key(id)
                || self.registry.lock().is_ok_and(|r| r.get(id).is_some())
        };
        loop {
            let id = horadric_core::session_id(base, SystemTime::now());
            if !taken(&id) {
                return id;
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn remember(&mut self, dir: &Path) {
        let d = dir.to_string_lossy().to_string();
        self.recent.retain(|r| folder_key(r) != folder_key(&d));
        self.recent.insert(0, d);
        self.recent.truncate(RECENT);
    }

    /// Starts an agent in a new session in `cwd` and shows it.
    fn start(&mut self, name: Option<String>, cwd: PathBuf, args: Vec<String>, agent: Agent) {
        if !cwd.is_dir() {
            dialog::error(
                "Horadric could not start a session",
                &format!("{} is not a folder", cwd.display()),
            );
            return;
        }
        let folder = cwd
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "session".into());
        let base = name.clone().unwrap_or_else(|| folder.clone());
        let id = self.unique_id(&base);
        // A bare Claude opens as a Folio, as on Windows; one with a name or
        // arguments of its own was asked for as a terminal.
        let chat = agent == Agent::Claude && name.is_none() && args.is_empty();
        let shown = name.unwrap_or(folder);
        let started = match chat {
            true => self.open_chat(&id, &shown, cwd, None, Vec::new(), false),
            false => self.launch(&id, &shown, cwd, args, Run::Agent(agent)),
        };
        if let Err(e) = started {
            dialog::error("Horadric could not start a session", &e);
            return;
        }
        if let Some(key) = self.project_of(&id) {
            self.show_project(&key, Some(&id));
        }
    }

    /// Starts Claude in a new session in `cwd` for a brother, without
    /// showing it here or asking anything: the brother hears why not.
    pub(super) fn start_afar(&mut self, cwd: PathBuf) -> Result<String, String> {
        let base = cwd
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "session".into());
        let id = self.unique_id(&base);
        self.launch(&id, &base, cwd, Vec::new(), Run::Agent(Agent::Claude))?;
        Ok(id)
    }

    /// Starts Claude in a terminal in a new session in `cwd`, for New CLI
    /// Session, where a bare start opens a Folio.
    fn start_cli(&mut self, cwd: PathBuf) {
        let base = cwd
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "session".into());
        let id = self.unique_id(&base);
        if let Err(e) = self.launch(&id, &base, cwd, Vec::new(), Run::Agent(Agent::Claude)) {
            dialog::error("Horadric could not start a session", &e);
            return;
        }
        if let Some(key) = self.project_of(&id) {
            self.show_project(&key, Some(&id));
        }
    }

    /// Starts session `id`, named `name`, as a Folio in `dir` on `own`,
    /// its own arguments as a chat takes them, carrying on `resume`. A
    /// tile that `existed` stays, paused, when it does not start.
    fn open_chat(
        &mut self,
        id: &str,
        name: &str,
        dir: PathBuf,
        resume: Option<String>,
        own: Vec<String>,
        existed: bool,
    ) -> Result<(), String> {
        if horadric_pty::headless::claude_exe().is_none() {
            return Err(
                "claude was not found. Install it, or check it is on the PATH your login shell sets."
                    .into(),
            );
        }
        // An ended chat of the same tile, kept for its last words, goes.
        super::folio::close(id);
        self.remember(&dir);
        self.register_folio(id, &dir, name, resume.as_deref());
        let key = self.project_of(id).unwrap_or_default();
        let mut all = self.defaults.flags(&own);
        all.extend(super::web::claude_args());
        all.extend(own.iter().cloned());
        let flags =
            horadric_core::folio::headless_args(&all, Some(horadric_core::folio::SUBAGENT_PROMPT));
        let options = horadric_pty::headless::Options {
            args: flags,
            env_set: vec![
                (horadric_hooks::SESSION_ENV.to_string(), id.to_string()),
                (
                    horadric_hooks::OWNER_ENV.to_string(),
                    horadric_hooks::port().to_string(),
                ),
                (TASKS_ENV.to_string(), String::new()),
                // The CLI keeps what each prompt's edits overwrote only when
                // told to off a terminal.
                (
                    "CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING".to_string(),
                    "1".to_string(),
                ),
            ]
            .into_iter()
            .chain(self.defaults.env())
            .collect(),
            ..Default::default()
        };
        let tag = crate::folio_session::Tag {
            key,
            name: name.to_string(),
            resume,
            history: 0,
            at: None,
        };
        if let Err(e) = super::folio::open(id, &dir, tag, &options) {
            if let Ok(mut r) = self.registry.lock() {
                if existed {
                    r.apply(
                        id,
                        &HookEvent::synthetic(HookEvent::PAUSE),
                        SystemTime::now(),
                    );
                } else {
                    r.remove(id);
                }
            }
            return Err(e);
        }
        let serial = self.next_serial;
        self.next_serial += 1;
        self.consoles.insert(
            id.to_string(),
            Console::folio(id.to_string(), serial, id.to_string()),
        );
        self.folio_args.insert(id.to_string(), own);
        self.reconcile();
        Ok(())
    }

    /// Gives a Folio its tile, as `launch` does a terminal session's.
    fn register_folio(&mut self, id: &str, dir: &Path, name: &str, resume: Option<&str>) {
        let register = HookEvent {
            cwd: dir.to_string_lossy().to_string(),
            name: Some(name.to_string()),
            ..HookEvent::synthetic(HookEvent::REGISTER)
        };
        if let Ok(mut r) = self.registry.lock() {
            r.apply(id, &register, SystemTime::now());
            if let Some(s) = r.get_mut(id) {
                s.shell = false;
                if let Some(conversation) = resume {
                    s.claude_session_id = Some(conversation.to_string());
                    s.prompted = true;
                }
            }
        }
    }

    /// Finds the Folios whose hosts ran on while no app did, after a
    /// reload, a crash or a Quit that kept them, and gives each its tile
    /// and pane again. Their phase follows as their replay comes in.
    fn reattach_folios(&mut self) {
        for found in super::folio::reattach() {
            let super::folio::Found { id, dir, tag } = found;
            // Saved as a paused chat too: the host it still has wins.
            if let Some(saved) = self.paused.remove(&id) {
                self.folio_args.insert(id.clone(), saved.args);
            }
            self.register_folio(&id, &dir, &tag.name, tag.resume.as_deref());
            self.resync.insert(id.clone());
            let serial = self.next_serial;
            self.next_serial += 1;
            self.consoles
                .insert(id.clone(), Console::folio(id.clone(), serial, id));
        }
    }

    /// A Folio's tile follows what its stream says, where its hooks say
    /// nothing: an ask, which headless never posts as a hook, and where a
    /// turn stands after a reattach, since hooks went nowhere meanwhile.
    fn folio_state(&mut self, id: &str, state: crate::folio_session::State) {
        let conversation = super::folio::conversation(id);
        {
            let Ok(mut r) = self.registry.lock() else {
                return;
            };
            let Some(session) = r.get_mut(id) else {
                return;
            };
            let now = SystemTime::now();
            if let Some(c) = conversation {
                session.follow(&c, now);
            }
            // Once no background task runs, the turn's end is not a wait
            // on one.
            if state.background == Some(0) {
                session.loot.outstanding.clear();
            }
            session.stream_agents = state.agents.clone();
            if let (Some(t), None) = (&state.title, &session.title) {
                session.title = Some(horadric_core::title::Title {
                    text: t.clone(),
                    custom: false,
                });
            }
            if let Some(status) = state.status() {
                session.status = Some(status);
            }
            let resync = state.live && self.resync.remove(id);
            if let Some(event) = state.event(&session.phase, session.waits_on.as_deref(), resync) {
                r.apply(id, &event, now);
            }
        }
        if state.gone {
            self.folio_gone(id);
        }
        self.reconcile();
    }

    /// What a Folio asks of the app.
    fn folio_ask(&mut self, id: &str, ask: super::folio::Ask) {
        use super::folio::{Ask, Leave};
        match ask {
            Ask::View(path) => {
                if let Some(key) = self.project_of(id) {
                    self.open_file(&key, path);
                }
            }
            Ask::Exit => self.end(id),
            // Its claude has ended and its host gone: the same tile goes
            // on, in a terminal or as a Folio started again.
            Ask::Left(left) => {
                let mut own = self.folio_args.remove(id).unwrap_or_default();
                super::folio::close(id);
                self.consoles.remove(id);
                let started = match left.leave {
                    Leave::Terminal => {
                        let args = Agent::Claude.carry_on(left.conversation.as_deref(), &own);
                        let went =
                            self.launch(id, &left.name, left.dir, args, Run::Agent(Agent::Claude));
                        // Its hooks name the conversation once it runs; the
                        // tile knows it meanwhile, to resume it.
                        if let (Some(c), Ok(mut r)) = (left.conversation, self.registry.lock()) {
                            if let Some(s) = r.get_mut(id) {
                                s.claude_session_id = Some(c);
                                s.prompted = true;
                            }
                        }
                        went
                    }
                    Leave::Again(again) => {
                        own.extend(again.own.iter().cloned());
                        let resume = again.resume.clone();
                        if resume.is_none() {
                            if let Ok(mut r) = self.registry.lock() {
                                if let Some(s) = r.get_mut(id) {
                                    s.claude_session_id = None;
                                    s.prompted = false;
                                }
                            }
                        }
                        super::folio::prepare(id, again);
                        self.open_chat(id, &left.name, left.dir, resume, own, true)
                    }
                };
                if let Err(e) = started {
                    dialog::error("Horadric could not start the session again", &e);
                    return;
                }
                if let Some(key) = self.project_of(id) {
                    self.show_project(&key, Some(id));
                }
            }
        }
    }

    /// A Folio whose claude ended on its own pauses, as a terminal that
    /// crashed does, so its tile resumes the conversation. The pane stays
    /// for what it last said to be read.
    fn folio_gone(&mut self, id: &str) {
        if self.paused.contains_key(id) {
            return;
        }
        let args = self.folio_args.get(id).cloned().unwrap_or_default();
        if let Ok(mut r) = self.registry.lock() {
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
    }

    fn open_shell(&mut self, key: &str) {
        let Some(dir) = self.project_dir(key) else {
            return;
        };
        let n = self
            .registry
            .lock()
            .map(|r| r.all().filter(|s| s.shell && project_key(s) == key).count())
            .unwrap_or(0);
        let id = self.unique_id("terminal");
        if let Err(e) = self.launch(&id, &shell::name(n), dir, Vec::new(), Run::Shell) {
            dialog::error("Horadric could not open a terminal", &e);
            return;
        }
        self.show_project(key, Some(&id));
    }

    /// Registers a session and starts its console.
    fn launch(
        &mut self,
        id: &str,
        name: &str,
        cwd: PathBuf,
        args: Vec<String>,
        run: Run,
    ) -> Result<(), String> {
        if self
            .consoles
            .get(id)
            .is_some_and(|c| c.exit_code().is_none())
        {
            return Err(format!("{id} is already running"));
        }
        let shell = matches!(run, Run::Shell);
        let program = match run {
            Run::Agent(agent) => console::agent_program(agent).ok_or_else(|| {
                format!(
                    "{} was not found. Install it, or check it is on the PATH your login shell sets.",
                    agent.program()
                )
            })?,
            Run::Shell => console::shell_program().ok_or("no shell found (set HORADRIC_SHELL)")?,
        };
        let fresh = self.quests.take_tree(id);
        if fresh.is_none() {
            self.remember(&cwd);
        }
        let register = HookEvent {
            cwd: cwd.to_string_lossy().to_string(),
            name: Some(name.to_string()),
            ..HookEvent::synthetic(HookEvent::REGISTER)
        };
        let mut own_tree = None;
        let was_known = self
            .registry
            .lock()
            .map(|mut r| {
                let known = r.get(id).is_some();
                r.apply(id, &register, SystemTime::now());
                if let Some(s) = r.get_mut(id) {
                    s.shell = shell;
                    if let Some((w, _)) = &fresh {
                        s.worktree = Some(w.clone());
                    }
                    own_tree = s.worktree.clone();
                }
                known
            })
            .unwrap_or(false);
        let extra = match shell {
            true => Vec::new(),
            false => self.claude_extra(id, &program, &args, &cwd),
        };
        let env = crate::quests::session_env(id, own_tree.as_ref(), &cwd);
        let serial = self.next_serial;
        self.next_serial += 1;
        let console = Console::spawn(
            Launch {
                id: id.to_string(),
                serial,
                program,
                args,
                extra,
                cwd,
                shell,
                env,
                setup: fresh.map(|(_, setup)| setup).unwrap_or_default(),
            },
            super::Notify,
        )
        .map_err(|e| {
            if let Ok(mut r) = self.registry.lock() {
                if was_known {
                    r.apply(
                        id,
                        &HookEvent::synthetic(HookEvent::PAUSE),
                        SystemTime::now(),
                    );
                } else if let Some(w) = r.remove(id).and_then(|s| s.worktree) {
                    crate::worktree::remove(w);
                }
            }
            format!("could not start the agent: {e}")
        })?;
        self.consoles.insert(id.to_string(), console);
        self.reconcile();
        Ok(())
    }

    /// Resumes a paused session in the same tile, with its conversation.
    pub(super) fn resume(&mut self, id: &str, show: bool) -> Result<(), String> {
        // Taken over on a brother: it runs there, and two must not run.
        if let Some(line) = super::keeping::fenced(id) {
            return Err(line);
        }
        let Some(saved) = self.paused.remove(id) else {
            return Ok(());
        };
        self.consoles.remove(id);
        let cwd = PathBuf::from(&saved.cwd);
        let result = if cwd.is_dir() && saved.chat {
            let conversation = (saved.claude_session_id.clone()).filter(|_| saved.prompted);
            let own = saved.args.clone();
            self.open_chat(id, &saved.name, cwd, conversation, own, true)
        } else if cwd.is_dir() {
            let run = if saved.shell {
                Run::Shell
            } else {
                Run::Agent(Agent::Claude)
            };
            let args = if saved.shell {
                Vec::new()
            } else {
                saved.launch_args()
            };
            self.launch(id, &saved.name, cwd, args, run)
        } else {
            Err(format!("{} no longer exists", cwd.display()))
        };
        if result.is_err() {
            self.paused.insert(id.to_string(), saved);
        } else if show {
            if let Some(key) = self.project_of(id) {
                self.show_project(&key, Some(id));
            }
        }
        result
    }

    /// Connects to every session host still running for this instance.
    fn attach_hosts(&mut self) {
        for id in console::running_hosts() {
            let Some(saved) = self.paused.get(&id).cloned() else {
                eprintln!("horadric: a session host runs for {id}, which is not a saved session");
                continue;
            };
            let serial = self.next_serial;
            let console = match Console::attach(
                &id,
                serial,
                saved.args.clone(),
                saved.shell,
                PathBuf::from(&saved.cwd),
                super::Notify,
            ) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("horadric: cannot attach to {id}: {e}");
                    continue;
                }
            };
            self.next_serial += 1;
            self.paused.remove(&id);
            let busy = saved
                .claude_session_id
                .as_deref()
                .filter(|_| !saved.shell)
                .and_then(|claude| transcript::mid_turn(&saved.cwd, claude))
                .unwrap_or(false);
            if let Ok(mut r) = self.registry.lock() {
                let now = SystemTime::now();
                r.apply(&id, &HookEvent::synthetic(HookEvent::REGISTER), now);
                if busy {
                    r.apply(&id, &HookEvent::synthetic("PreToolUse"), now);
                }
            }
            self.consoles.insert(id, console);
        }
    }

    fn output(&mut self, serial: usize) {
        if self.views.values().any(|v| v.console.serial == serial) {
            // A file's colours came in.
            self.stage.output(serial);
            return;
        }
        let Some(console) = self.console_by_serial(serial).cloned() else {
            return;
        };
        if !console.take_dirty() {
            return;
        }
        self.stage.output(serial);
        if console.shell {
            let title = console.title();
            if let Ok(mut r) = self.registry.lock() {
                if let Some(s) = r.get_mut(&console.id) {
                    s.touch(SystemTime::now());
                    s.last_line = title.unwrap_or_default();
                }
            }
        }
    }

    /// The program ended. A clean exit ends the session; anything else
    /// pauses it, so a click resumes it.
    fn exited(&mut self, serial: usize) {
        if self.frozen {
            return;
        }
        let Some(console) = self.console_by_serial(serial).cloned() else {
            return;
        };
        if console.shell || self.ending.remove(&console.id) {
            self.forget(&console.id);
            self.reconcile();
            return;
        }
        if let Ok(mut r) = self.registry.lock() {
            if console.exit_code() == Some(0) {
                if r.get(&console.id).is_some() {
                    r.apply(
                        &console.id,
                        &HookEvent::synthetic("SessionEnd"),
                        SystemTime::now(),
                    );
                }
            } else if let Some(s) = r.get(&console.id) {
                let saved = SavedSession::from_session(s, console.args.clone(), false);
                self.paused.insert(console.id.clone(), saved);
                r.apply(
                    &console.id,
                    &HookEvent::synthetic(HookEvent::PAUSE),
                    SystemTime::now(),
                );
            }
        }
        self.stage.output(serial);
        self.reconcile();
    }

    /// Takes a session off the tiles for good.
    pub(super) fn forget(&mut self, id: &str) {
        if let Some(c) = self.consoles.remove(id) {
            if c.folio.is_some() {
                super::folio::close(id);
                self.folio_args.remove(id);
            } else if c.exit_code().is_none() {
                c.kill();
            }
        }
        self.paused.remove(id);
        let own_tree = (self.registry.lock().ok())
            .and_then(|mut r| r.remove(id))
            .and_then(|s| Some((project_key(&s), s.worktree?)));
        if let Some((key, w)) = own_tree {
            self.remove_tree(key, w);
        }
    }

    /// Ends a session: its program is told to stop, and its tile goes.
    pub(super) fn end(&mut self, id: &str) {
        // A brother's session runs there: its tile menu asks it to end.
        if super::far::is_brothers(self, id) {
            return;
        }
        match self.consoles.get(id) {
            // A Folio's claude gets its stdin closed and ends itself, and
            // no exit comes back to a console: the tile goes now.
            Some(c) if c.folio.is_some() => {
                super::folio::close(id);
                self.consoles.remove(id);
                self.folio_args.remove(id);
                self.paused.remove(id);
                if let Ok(mut r) = self.registry.lock() {
                    r.apply(id, &HookEvent::synthetic("SessionEnd"), SystemTime::now());
                }
            }
            Some(c) if c.exit_code().is_none() => {
                // A clean end: the tile goes when the exit arrives.
                self.ending.insert(id.to_string());
                c.kill();
                if let Ok(mut r) = self.registry.lock() {
                    r.apply(id, &HookEvent::synthetic("SessionEnd"), SystemTime::now());
                }
                self.paused.remove(id);
            }
            _ => self.forget(id),
        }
        self.reconcile();
    }

    fn identify(&mut self, id: &str) {
        let changed = self
            .registry
            .lock()
            .ok()
            .and_then(|mut r| r.get_mut(id).map(|s| s.identify()))
            .unwrap_or(false);
        if changed {
            self.redraw_clusters();
        }
    }

    // Windows.

    /// Makes the clusters match the projects in the registry, places them
    /// and redraws them, and brings the stage up to date.
    pub(super) fn reconcile(&mut self) {
        let now = SystemTime::now();
        let sessions: Vec<horadric_core::Session> = self
            .registry
            .lock()
            .map(|mut r| {
                r.prune_ended(ENDED_LINGER, now);
                r.all().cloned().collect()
            })
            .unwrap_or_default();
        // Projects in the order they stand, new ones at the end.
        let mut keys: Vec<String> = Vec::new();
        for s in &sessions {
            let k = project_key(s);
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
        for k in &keys {
            if !self.order.contains(k) {
                self.order.push(k.clone());
            }
        }
        let present: HashSet<&String> = keys.iter().collect();
        let mut gone = Vec::new();
        self.clusters.retain(|c| {
            let keep = present.contains(&c.key);
            if !keep {
                gone.push(c.key.clone());
                c.close();
            }
            keep
        });
        for k in &keys {
            if !self.clusters.iter().any(|c| &c.key == k) {
                self.clusters.push(Cluster::new(self.mtm, k));
            }
        }
        let order = self.order.clone();
        self.clusters
            .sort_by_key(|c| order.iter().position(|k| *k == c.key).unwrap_or(usize::MAX));
        let stage_key = self.stage.key().filter(|_| self.stage.is_visible());
        let focused = self.stage.focused_id();
        let m = Metrics::default();
        let dirs: Vec<(String, PathBuf)> = (keys.iter())
            .filter_map(|k| Some((k.clone(), self.project_dir(k)?)))
            .collect();
        self.files.keep(&dirs);
        for c in &mut self.clusters {
            let mut mine: Vec<horadric_core::Session> = sessions
                .iter()
                .filter(|s| project_key(s) == c.key)
                .cloned()
                .collect();
            mine.sort_by_key(|s| s.created);
            let collapsed = self.collapsed.contains(&c.key);
            c.collapsed = collapsed;
            let shown = if collapsed { 0 } else { mine.len() };
            let body = self.files.body(&c.key, &m).filter(|_| !collapsed);
            let layout = layout::cluster(&m, shown, 0, collapsed, None, None, body);
            let files = (layout.files.as_ref()).and_then(|f| self.files.view(&c.key, f.rows.len()));
            let (hot, pressed) = c.hover();
            let on_stage = stage_key.as_deref() == Some(c.key.as_str());
            let selected = focused
                .as_deref()
                .filter(|_| on_stage)
                .and_then(|id| mine.iter().position(|s| s.id == id));
            c.set_scene(Scene {
                name: project_name(&c.key),
                accent: theme::accent(&c.key),
                sessions: if collapsed { Vec::new() } else { mine },
                layout,
                looks: Vec::new(),
                hot,
                pressed,
                collapsed,
                on_stage,
                selected,
                now,
                ambient: true,
                files,
            });
        }
        self.place_clusters();
        self.sync_stage(&sessions);
        self.attention(&sessions);
        self.sync_discord(&sessions);
        self.menus();
        self.save();
    }

    /// The Dock says how many sessions wait for the human, and bounces
    /// once when one starts to while they are in another app.
    fn attention(&mut self, sessions: &[horadric_core::Session]) {
        let waiting: HashSet<String> = sessions
            .iter()
            .filter(|s| s.phase.is_waiting())
            .map(|s| s.id.clone())
            .collect();
        let app = NSApplication::sharedApplication(self.mtm);
        let fresh = waiting.iter().any(|id| !self.waiting.contains(id));
        if fresh && !app.isActive() {
            app.requestUserAttention(
                objc2_app_kit::NSRequestUserAttentionType::InformationalRequest,
            );
        }
        let badge = match waiting.len() {
            0 => None,
            n => Some(objc2_foundation::NSString::from_str(&n.to_string())),
        };
        app.dockTile().setBadgeLabel(badge.as_deref());
        self.waiting = waiting;
    }

    /// Hands the Discord client what the profile should say now; it drops
    /// what did not change, and only ever takes it on a channel, so a hung
    /// Discord never holds the app.
    fn sync_discord(&mut self, sessions: &[horadric_core::Session]) {
        if self.discord.is_off() || self.frozen {
            // Dropped, not stopped: the client clears and closes on its
            // own thread.
            self.rich = None;
            self.run = Default::default();
            return;
        }
        let stage = self
            .stage
            .key()
            .filter(|_| self.stage.is_visible())
            .map(|k| project_name(&k));
        let named: Vec<(&horadric_core::Session, String)> = sessions
            .iter()
            .map(|s| (s, project_name(&project_key(s))))
            .collect();
        let seen: Vec<horadric_core::presence::Seen> = named
            .iter()
            .map(|(session, project)| horadric_core::presence::Seen { session, project })
            .collect();
        let start = self.run.update(&seen, SystemTime::now());
        let activity =
            horadric_core::presence::presence(&seen, stage.as_deref(), self.discord, start);
        self.rich
            .get_or_insert_with(crate::discord::Discord::start)
            .set(activity);
    }

    fn redraw_clusters(&mut self) {
        // Phases did not change, but lines and ages did: the scenes are
        // rebuilt from the registry as a reconcile does.
        self.reconcile();
    }

    /// Stands the clusters in columns down the left of the screen.
    fn place_clusters(&self) {
        let (left, top, _, h) = work_area(self.mtm);
        let w = Metrics::default().width;
        let (mut x, mut y) = (left + GAP, top + GAP);
        for c in &self.clusters {
            if y > top + GAP && y + c.height > top + h - GAP {
                x += w + GAP;
                y = top + GAP;
            }
            c.place(x, y);
            y += c.height + GAP;
        }
    }

    /// The panes the stage shows for the project with this key.
    fn panes_of(&self, key: &str, sessions: &[horadric_core::Session]) -> Vec<Pane> {
        let accent = theme::accent(key);
        let mut mine: Vec<&horadric_core::Session> =
            sessions.iter().filter(|s| project_key(s) == key).collect();
        mine.sort_by_key(|s| s.created);
        let mut panes: Vec<Pane> = mine
            .into_iter()
            .filter_map(|s| {
                let c = self
                    .consoles
                    .get(&s.id)
                    .or_else(|| self.kin.far.console(&s.id))?;
                let mut pane =
                    Pane::new(s.id.clone(), s.label().to_string(), Arc::clone(c), accent);
                pane.phase =
                    (theme::edge_strength(&s.phase) > 0.0).then(|| theme::phase_color(&s.phase));
                Some(pane)
            })
            .collect();
        // The files open in the project, after its sessions, by name.
        let mut views: Vec<(&String, &View)> =
            self.views.iter().filter(|(_, v)| v.key == key).collect();
        views.sort_by(|a, b| a.0.cmp(b.0));
        for (id, v) in views {
            let name = (v.console.path())
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_else(|| id.clone());
            panes.push(Pane::new(id.clone(), name, Arc::clone(&v.console), accent));
        }
        // The project's browser, last.
        if let Some(c) = super::web::console_of(key) {
            panes.push(Pane::new(
                super::web::pane_id(key),
                "Browser".into(),
                c,
                accent,
            ));
        }
        panes
    }

    /// Shows the file at `path` read only on the stage, with the project
    /// `key`, or brings it forward when it shows already.
    fn open_file(&mut self, key: &str, path: PathBuf) {
        let id = format!("view:{}", path.display());
        if !self.views.contains_key(&id) {
            let serial = self.next_serial;
            self.next_serial += 1;
            let detail = (path.parent())
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let changed = file_changed(&path);
            let console = Console::view(id.clone(), serial, path, detail, super::Notify);
            self.views.insert(
                id.clone(),
                View {
                    key: key.to_string(),
                    console,
                    changed,
                },
            );
        }
        self.show_project(key, Some(&id));
    }

    /// Asks for a file in the project `key` and shows it.
    fn pick_file(&mut self, key: &str) {
        let start = self.project_dir(key);
        if let Some(path) = dialog::pick_file(self.mtm, start.as_deref()) {
            self.open_file(key, path);
        }
    }

    /// Reads again each file shown whose file changed since.
    fn reload_views(&mut self) {
        for v in self.views.values_mut() {
            let Some(path) = v.console.path() else {
                continue;
            };
            let now = file_changed(&path);
            if now != v.changed {
                v.changed = now;
                v.console.reload(super::Notify);
                self.stage.output(v.console.serial);
            }
        }
    }

    /// Keeps the stage's panes the project's consoles, and its colours
    /// the phases.
    fn sync_stage(&mut self, sessions: &[horadric_core::Session]) {
        let Some(key) = self.stage.key() else {
            return;
        };
        let want: Vec<String> = self
            .panes_of(&key, sessions)
            .iter()
            .map(|p| p.id.clone())
            .collect();
        let shown_all = want.iter().all(|id| self.stage.shows(id));
        let same_count = {
            let mut n = 0;
            for s in sessions {
                let console =
                    self.consoles.contains_key(&s.id) || self.kin.far.console(&s.id).is_some();
                if project_key(s) == key && console && self.stage.shows(&s.id) {
                    n += 1;
                }
            }
            n += (self.views.iter())
                .filter(|(id, v)| v.key == key && self.stage.shows(id))
                .count();
            n += usize::from(
                super::web::is_open(&key) && self.stage.shows(&super::web::pane_id(&key)),
            );
            n == want.len()
        };
        if !(shown_all && same_count) {
            let panes = self.panes_of(&key, sessions);
            self.stage.show(&key, &project_name(&key), panes, None);
            return;
        }
        let by_id: HashMap<&str, &horadric_core::Session> =
            sessions.iter().map(|s| (s.id.as_str(), s)).collect();
        self.stage.refresh(|p| {
            if let Some(s) = by_id.get(p.id.as_str()) {
                p.name = s.label().to_string();
                p.phase =
                    (theme::edge_strength(&s.phase) > 0.0).then(|| theme::phase_color(&s.phase));
            }
        });
    }

    /// Shows the project with this key on the stage and brings it forward.
    pub(super) fn show_project(&mut self, key: &str, focus: Option<&str>) {
        let sessions: Vec<horadric_core::Session> = self
            .registry
            .lock()
            .map(|r| r.all().cloned().collect())
            .unwrap_or_default();
        let panes = self.panes_of(key, &sessions);
        self.stage.show(key, &project_name(key), panes, focus);
        self.stage.present();
        let app = NSApplication::sharedApplication(self.mtm);
        app.activate();
        if let Some(id) = focus
            .map(str::to_string)
            .or_else(|| self.stage.focused_id())
        {
            self.identify(&id);
        }
        for c in &self.clusters {
            c.raise();
        }
        self.reconcile();
    }

    fn cluster_click(&mut self, key: &str, hit: Hit, at: NSPoint) {
        let ids = self.ids_in(key);
        match hit {
            Hit::Tile(i) => {
                let Some(id) = ids.get(i).cloned() else {
                    return;
                };
                super::far::reveal(self, &id);
                if self.paused.contains_key(&id) {
                    if let Err(e) = self.resume(&id, true) {
                        dialog::error("Horadric could not resume the session", &e);
                    }
                } else {
                    self.show_project(key, Some(&id));
                }
            }
            Hit::Add => {
                if super::far::start(self, key) {
                    return;
                }
                if let Some(dir) = self.project_dir(key) {
                    self.start(None, dir, Vec::new(), Agent::Claude);
                }
            }
            Hit::Shell => self.open_shell(key),
            Hit::Header => {
                if !self.collapsed.remove(key) {
                    self.collapsed.insert(key.to_string());
                }
                self.reconcile();
            }
            Hit::New => self.pick_and_start(),
            Hit::FilesHeader | Hit::File(_) => match self.files.click(key, hit) {
                super::files::Click::Redraw => self.reconcile(),
                super::files::Click::Open(path) => self.open_file(key, path),
                super::files::Click::Nothing => {}
            },
            _ => {
                let _ = at;
            }
        }
    }

    /// The session ids a cluster shows, in its order.
    fn ids_in(&self, key: &str) -> Vec<String> {
        let mut mine: Vec<(SystemTime, String)> = self
            .registry
            .lock()
            .map(|r| {
                r.all()
                    .filter(|s| project_key(s) == key)
                    .map(|s| (s.created, s.id.clone()))
                    .collect()
            })
            .unwrap_or_default();
        mine.sort();
        mine.into_iter().map(|(_, id)| id).collect()
    }

    fn cluster_menu(&mut self, key: &str, hit: Hit, at: NSPoint) {
        let ids = self.ids_in(key);
        if let Hit::Tile(i) = hit {
            if let Some(id) = ids.get(i).cloned() {
                self.tile_menu(&id, at);
            }
            return;
        }
        const NEW: usize = 1;
        const SHELL: usize = 2;
        const REVEAL: usize = 3;
        const END_ALL: usize = 4;
        const OPEN_FILE: usize = 5;
        const BROWSER: usize = 6;
        let items = [
            Item::pick("New session", NEW),
            Item::pick("New terminal", SHELL),
            Item::pick("Open file\u{2026}", OPEN_FILE),
            Item::pick("Open browser", BROWSER),
            Item::pick("Show in Finder", REVEAL),
            Item::Sub("Quest log".into(), self.quest_items(key)),
            Item::Separator,
            Item::pick("End every session in this project", END_ALL),
        ];
        match menu::popup(self.mtm, &items, at) {
            Some(NEW) => {
                if let Some(dir) = self.project_dir(key) {
                    self.start(None, dir, Vec::new(), Agent::Claude);
                }
            }
            Some(SHELL) => self.open_shell(key),
            Some(t) if t >= questing::QUEST_TAG => self.quest_pick(key, t),
            Some(OPEN_FILE) => self.pick_file(key),
            Some(BROWSER) => super::web::open_for_user(self, key),
            Some(REVEAL) => {
                if let Some(dir) = self.project_dir(key) {
                    let _ = std::process::Command::new("/usr/bin/open").arg(dir).spawn();
                }
            }
            Some(END_ALL) => {
                let live = ids
                    .iter()
                    .filter(|id| {
                        self.consoles
                            .get(*id)
                            .is_some_and(|c| c.exit_code().is_none())
                    })
                    .count();
                let ok = live == 0
                    || dialog::confirm(
                        &format!("End {live} running session{}?", if live == 1 { "" } else { "s" }),
                        "Their agents are stopped. A conversation can still be resumed from Claude Code.",
                        "End them",
                    );
                if ok {
                    for id in ids {
                        self.end(&id);
                    }
                }
            }
            _ => {}
        }
    }

    fn tile_menu(&mut self, id: &str, at: NSPoint) {
        const SHOW: usize = 1;
        const RESUME: usize = 2;
        const RENAME: usize = 3;
        const END: usize = 4;
        let paused = self.paused.contains_key(id);
        let open = vec![
            if paused {
                Item::pick("Resume", RESUME)
            } else {
                Item::pick("Show", SHOW)
            },
            Item::pick("Rename\u{2026}", RENAME),
        ];
        let end = vec![Item::pick(
            if paused { "Forget" } else { "End session" },
            END,
        )];
        // The groups of the Windows tile menu in its order (calls to action,
        // open, what the session is doing, end), so a line the port gains
        // lands where it is on Windows. The port has no calls to action
        // yet; an empty group is left out.
        let calls: Vec<Item> = Vec::new();
        let doing: Vec<Item> = super::handing::menu(self, id);
        let mut items = Vec::new();
        for group in [calls, open, doing, end] {
            if group.is_empty() {
                continue;
            }
            if !items.is_empty() {
                items.push(Item::Separator);
            }
            items.extend(group);
        }
        match menu::popup(self.mtm, &items, at) {
            Some(SHOW) => {
                super::far::reveal(self, id);
                if let Some(key) = self.project_of(id) {
                    self.show_project(&key, Some(id));
                }
            }
            Some(RESUME) => {
                if let Err(e) = self.resume(id, true) {
                    dialog::error("Horadric could not resume the session", &e);
                }
            }
            Some(RENAME) => {
                let now = self
                    .registry
                    .lock()
                    .ok()
                    .and_then(|r| r.get(id).map(|s| s.label().to_string()))
                    .unwrap_or_default();
                if let Some(name) =
                    dialog::ask("Rename session", "A name for its tile and pane.", &now)
                {
                    if !super::far::rename(self, id, name.trim()) {
                        if let Ok(mut r) = self.registry.lock() {
                            if let Some(s) = r.get_mut(id) {
                                s.rename(Some(name.trim()).filter(|n| !n.is_empty()));
                            }
                        }
                        self.reconcile();
                    }
                }
            }
            Some(END) => {
                if !super::far::stop(self, id) {
                    self.end(id);
                }
            }
            Some(tag) => {
                super::handing::picked(self, id, tag);
            }
            None => {}
        }
    }

    fn pane_menu(&mut self, id: &str, at: NSPoint) {
        // A browser pane's own bar has what it can do.
        if super::web::owns(id) {
            return;
        }
        let Some(view) = self.views.get(id) else {
            self.tile_menu(id, at);
            return;
        };
        const REVEAL: usize = 1;
        const OPEN: usize = 2;
        const CLOSE: usize = 3;
        let path = view.console.path();
        let key = view.key.clone();
        let items = [
            Item::pick("Show in Finder", REVEAL),
            Item::pick("Open Another File\u{2026}", OPEN),
            Item::Separator,
            Item::pick("Close", CLOSE),
        ];
        match menu::popup(self.mtm, &items, at) {
            Some(REVEAL) => {
                if let Some(p) = path {
                    let _ = std::process::Command::new("/usr/bin/open")
                        .arg("-R")
                        .arg(p)
                        .spawn();
                }
            }
            Some(OPEN) => self.pick_file(&key),
            Some(CLOSE) => self.close_pane(id),
            _ => {}
        }
    }

    fn close_pane(&mut self, id: &str) {
        if super::far::close(self, id) || super::web::close_pane(self, id) {
            return;
        }
        if self.views.remove(id).is_some() {
            self.reconcile();
            return;
        }
        let live = self
            .consoles
            .get(id)
            .is_some_and(|c| c.exit_code().is_none());
        let shell = self.consoles.get(id).is_some_and(|c| c.shell);
        if live && !shell {
            let ok = dialog::confirm(
                "End this session?",
                "Its agent is stopped. Paused sessions can be resumed from their tile.",
                "End session",
            );
            if !ok {
                return;
            }
        }
        self.end(id);
    }

    /// Shows the session that has waited longest, then the next one each
    /// time, as Ctrl+Alt+Space does on Windows.
    fn next_waiting(&mut self) {
        let shown = self.stage.focused_id();
        let mut waiting: Vec<(SystemTime, String)> = self
            .registry
            .lock()
            .map(|r| {
                r.all()
                    .filter(|s| s.phase.is_waiting())
                    .map(|s| (s.since, s.id.clone()))
                    .collect()
            })
            .unwrap_or_default();
        waiting.sort();
        // Past the one shown now, round to the first.
        let at = shown
            .and_then(|id| waiting.iter().position(|(_, w)| *w == id))
            .map_or(0, |i| (i + 1) % waiting.len().max(1));
        let Some((_, id)) = waiting.get(at).cloned() else {
            return;
        };
        if let Some(key) = self.project_of(&id) {
            self.show_project(&key, Some(&id));
        }
    }

    fn pick_and_start(&mut self) {
        let start = self.recent.first().map(PathBuf::from);
        if let Some(dir) = dialog::pick_folder(self.mtm, start.as_deref()) {
            self.start(None, dir, Vec::new(), Agent::Claude);
        }
    }

    // Menus.

    pub(super) fn menus(&self) {
        let waiting = self.waiting.len();
        let mut items = vec![
            Item::keyed("New Session\u{2026}", NEW_SESSION, "n"),
            if waiting > 0 {
                Item::pick(format!("Next Waiting Session ({waiting})"), NEXT_WAITING)
            } else {
                Item::pick("Next Waiting Session", NEXT_WAITING)
            },
            Item::pick("Show the Stage", SHOW_STAGE),
            Item::pick("Bring Tiles to Front", FRONT),
        ];
        if !self.recent.is_empty() {
            let recent: Vec<Item> = self
                .recent
                .iter()
                .enumerate()
                .map(|(i, r)| Item::pick(r.clone(), RECENT_TAG + i))
                .collect();
            items.push(Item::Sub("Recent Projects".into(), recent));
        }
        items.push(Item::Separator);
        items.push(super::brothers::menu(self));
        let themes: Vec<Item> = theme::Theme::ALL
            .iter()
            .enumerate()
            .map(|(i, t)| Item::checked(t.label(), THEME_TAG + i, theme::current() == *t))
            .collect();
        items.push(Item::Sub("Theme".into(), themes));
        let discord: Vec<Item> = horadric_core::saved::Discord::ALL
            .iter()
            .enumerate()
            .map(|(i, d)| Item::checked(d.label(), DISCORD_TAG + i, self.discord == *d))
            .collect();
        items.push(Item::Sub("Show on Discord".into(), discord));
        items.push(Item::checked("Open at Login", LOGIN, autostart::enabled()));
        items.push(Item::pick("Usage\u{2026}", USAGE));
        items.push(Item::pick("Settings\u{2026}", SETTINGS));
        match self.update.offer() {
            Some(v) => items.push(Item::pick(format!("Update to {v}\u{2026}"), INSTALL_UPDATE)),
            None => items.push(Item::pick("Check for Updates", CHECK_UPDATE)),
        }
        items.push(Item::Separator);
        items.push(Item::pick(
            format!("About Horadric {}", env!("CARGO_PKG_VERSION")),
            ABOUT,
        ));
        items.push(Item::pick("Quit Horadric", QUIT));
        self.status.set_menu(self.mtm, &items);

        let app_items = [
            Item::pick(
                format!("About Horadric {}", env!("CARGO_PKG_VERSION")),
                ABOUT,
            ),
            Item::pick("Check for Updates", CHECK_UPDATE),
            Item::Separator,
            Item::keyed("Settings\u{2026}", SETTINGS, ","),
            Item::Separator,
            Item::keyed("Quit Horadric", QUIT, "q"),
        ];
        let session_items = [
            Item::keyed("New Session\u{2026}", NEW_SESSION, "n"),
            Item::pick("New CLI Session\u{2026}", NEW_CLI_SESSION),
            Item::pick("New Terminal", NEW_TERMINAL),
            Item::keyed("Next Waiting Session", NEXT_WAITING, "j"),
            Item::Separator,
            Item::pick("Bring Tiles to Front", FRONT),
        ];
        menu::set_main(self.mtm, &app_items, &session_items);
    }

    /// What the usage window shows.
    fn usage_values(&self) -> super::usage::Values {
        super::usage::Values {
            usage: self.usage.lock().ok().and_then(|u| u.clone()),
            account: super::usage::account(),
            now: unix_now(),
        }
    }

    /// What the Settings window shows.
    fn settings_values(&self) -> super::settings::Values {
        super::settings::Values {
            theme: theme::current(),
            discord: self.discord,
            login: autostart::enabled(),
            defaults: self.defaults.clone(),
        }
    }

    /// Applies what a control in the Settings window picked. The theme,
    /// Discord and login go the way their menu items go, so both ways in
    /// do the same.
    fn setting(&mut self, pick: super::settings::Pick) {
        use super::settings::Pick;
        match pick {
            Pick::Theme(t) => {
                if let Some(i) = theme::Theme::ALL.iter().position(|x| *x == t) {
                    self.menu_pick(THEME_TAG + i);
                }
            }
            Pick::Discord(d) => {
                if let Some(i) = horadric_core::saved::Discord::ALL
                    .iter()
                    .position(|x| *x == d)
                {
                    self.menu_pick(DISCORD_TAG + i);
                }
            }
            Pick::Login(on) => {
                if on != autostart::enabled() {
                    self.menu_pick(LOGIN);
                }
            }
            Pick::Default(s, value) => {
                self.defaults.set(s, value);
                self.save();
            }
        }
        super::settings::refresh(&self.settings_values());
    }

    fn menu_pick(&mut self, tag: usize) {
        // Before the recent folders, whose tags run on from theirs.
        if super::brothers::picked(self, tag) {
            return;
        }
        match tag {
            NEW_SESSION => self.pick_and_start(),
            NEW_CLI_SESSION => {
                let start = self.recent.first().map(PathBuf::from);
                if let Some(dir) = dialog::pick_folder(self.mtm, start.as_deref()) {
                    self.start_cli(dir);
                }
            }
            NEW_TERMINAL => {
                if let Some(key) = self.stage.key() {
                    self.open_shell(&key);
                }
            }
            NEXT_WAITING => self.next_waiting(),
            SHOW_STAGE => {
                self.stage.present();
                let app = NSApplication::sharedApplication(self.mtm);
                app.activate();
            }
            FRONT => {
                for c in &self.clusters {
                    c.raise();
                }
            }
            LOGIN => {
                if autostart::enabled() {
                    autostart::disable();
                } else if let Ok(exe) = std::env::current_exe() {
                    if let Err(e) = autostart::enable_at(&exe) {
                        dialog::error("Horadric could not open at login", &e);
                    }
                }
                self.menus();
                super::settings::refresh(&self.settings_values());
            }
            t if (THEME_TAG..THEME_TAG + theme::Theme::ALL.len()).contains(&t) => {
                theme::set(theme::Theme::ALL[t - THEME_TAG]);
                self.reconcile();
                self.stage.refresh(|_| {});
                self.menus();
            }
            t if (DISCORD_TAG..DISCORD_TAG + horadric_core::saved::Discord::ALL.len())
                .contains(&t) =>
            {
                self.discord = horadric_core::saved::Discord::ALL[t - DISCORD_TAG];
                // Reconciles, which hands the client the new setting.
                self.reconcile();
            }
            SETTINGS => super::settings::show(self.mtm, &self.settings_values()),
            USAGE => super::usage::show(self.mtm, &self.usage_values()),
            CHECK_UPDATE => self.update.check(true),
            INSTALL_UPDATE => self.install_update(),
            ABOUT => dialog::about(),
            QUIT => self.ask_quit(),
            t if t >= RECENT_TAG => {
                if let Some(dir) = self.recent.get(t - RECENT_TAG).map(PathBuf::from) {
                    self.start(None, dir, Vec::new(), Agent::Claude);
                }
            }
            _ => {}
        }
    }

    fn install_update(&mut self) {
        let Some(m) = self.update.manifest() else {
            return;
        };
        if !dialog::confirm(
            &format!("Update to Horadric {}?", m.version),
            &m.notes,
            "Update now",
        ) {
            return;
        }
        // The download runs on a thread; the tick hands over once it is in.
        self.update.start_install();
    }

    // The listener's commands.

    fn commands(&mut self) {
        let taken: Vec<Command> = self
            .requests
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default();
        for c in taken {
            match c {
                Command::New(n) => {
                    let name = n.name.filter(|s| !s.is_empty());
                    self.start(name, PathBuf::from(n.cwd), n.args, Agent::Claude);
                }
                Command::Reload(r) => self.reload(r),
                // A session that just moved here is told so first. The
                // memory is not on the Mac yet: dropping any other Recall
                // call closes its reply, so the caller hears no answer at
                // once instead of waiting (it then answers by words).
                Command::Recall(call) => {
                    let _ = super::handing::recall(call);
                }
                Command::Browser(call) => super::web::call(self, call),
                Command::Tasks(t) => self.quest_command(&t),
                // Overlaps are not on the Mac yet; a Know call is dropped
                // as a Recall is.
                Command::Overlap(_) | Command::Know(_) => {}
            }
        }
    }

    fn reload(&mut self, r: Reload) {
        self.reload_into(Path::new(&r.exe));
    }

    /// Hands over to the build at `exe`: saves, starts its `swap`, quits.
    /// The sessions run on in their hosts and the new build attaches.
    fn reload_into(&mut self, exe: &Path) {
        self.freeze();
        let started = std::process::Command::new(exe)
            .args(["swap", "--pid", &std::process::id().to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match started {
            Ok(_) => terminate(self.mtm),
            Err(e) => {
                self.frozen = false;
                dialog::error(
                    "Horadric could not reload",
                    &format!("{}: {e}", exe.display()),
                );
            }
        }
    }

    // Time.

    fn tick(&mut self) {
        let mut ended = false;
        if let Ok(mut r) = self.registry.lock() {
            ended = r.all().any(|s| s.phase == Phase::Ended);
            let _ = &mut r;
        }
        if ended {
            self.reconcile();
        } else {
            // Ages and traces move with the clock.
            for c in &self.clusters {
                c.redraw();
            }
            self.save();
            // A run of work on the profile ends on time.
            if !self.discord.is_off() {
                let sessions: Vec<horadric_core::Session> = (self.registry.lock())
                    .map(|r| r.all().cloned().collect())
                    .unwrap_or_default();
                self.sync_discord(&sessions);
            }
        }
        self.reload_views();
        self.tick_quests();
        super::usage::refresh(&self.usage_values());
        self.update.tick();
        super::brothers::refresh(self);
        if let Some(checked) = self.update.take_checked() {
            // An update found is offered from the menu too, whatever the
            // answer to the question below.
            self.menus();
            match checked {
                Ok(true) => self.install_update(),
                Ok(false) => dialog::info(
                    "Horadric is up to date",
                    &format!("{} is the newest release.", env!("CARGO_PKG_VERSION")),
                ),
                Err(e) => dialog::error("Horadric could not check for updates", &e),
            }
        }
        if let Some(ready) = self.update.take_ready() {
            match ready {
                Ok(exe) => self.reload_into(&exe),
                Err(e) => dialog::error("Horadric could not update", &e),
            }
        }
        if self.update.take_news() {
            self.menus();
            if let Some(v) = self.update.offer() {
                dialog::notify(
                    &format!("Horadric {v} is out"),
                    "Update from the menu bar icon.",
                );
            }
        }
    }

    // Saving and quitting.

    fn snapshot(&self) -> SavedState {
        let mut sessions = Vec::new();
        if let Ok(r) = self.registry.lock() {
            for s in r.all().filter(|s| s.background.is_none()) {
                if let Some(p) = self.paused.get(&s.id) {
                    sessions.push(SavedSession::from_session(s, p.args.clone(), false));
                } else if let Some(c) = self.consoles.get(&s.id) {
                    if c.exit_code().is_none() && s.phase != Phase::Ended {
                        sessions.push(match c.folio.is_some() {
                            // It resumes as a chat whatever new ones open as.
                            true => SavedSession {
                                chat: true,
                                ..SavedSession::from_session(
                                    s,
                                    self.folio_args.get(&s.id).cloned().unwrap_or_default(),
                                    true,
                                )
                            },
                            false => SavedSession::from_session(s, c.args.clone(), true),
                        });
                    }
                }
            }
        }
        let frame = self.stage.frame();
        let stage = [
            frame.origin.x as i32,
            frame.origin.y as i32,
            frame.size.width as i32,
            frame.size.height as i32,
        ];
        let mut saved = SavedState {
            clusters: self
                .clusters
                .iter()
                .map(|c| horadric_core::SavedCluster {
                    key: c.key.clone(),
                    collapsed: c.collapsed,
                    files_collapsed: self.files.folded.contains(&c.key),
                    tasks_collapsed: false,
                    tome_collapsed: false,
                })
                .collect(),
            columns: vec![self.order.clone()],
            recent: self.recent.clone(),
            stage: Some(stage),
            on_stage: self.stage.focused_id().filter(|_| self.stage.is_visible()),
            sessions,
            font_size: Some(self.font_size).filter(|&s| s != stage::FONT_DEFAULT),
            theme: theme::current().saved(),
            accents: theme::accents(),
            live: !self.quit,
            update_told: self.update.told(),
            discord: self.discord,
            defaults: self.defaults.clone(),
            usage: self.usage.lock().ok().and_then(|u| u.clone()),
            ..Default::default()
        };
        self.quests.keep(&mut saved);
        saved
    }

    fn save(&mut self) {
        if self.frozen {
            return;
        }
        let now = self.snapshot();
        if self.last_saved.as_ref() != Some(&now) {
            store::save(&now);
            self.last_saved = Some(now);
        }
    }

    /// Saves one last time and stops saving.
    fn freeze(&mut self) {
        self.save();
        self.frozen = true;
    }

    /// Quits now, the sessions running on in their hosts: the Dock's
    /// Quit, and a logout, which must never wait on a question.
    pub(crate) fn quit_now(&mut self) {
        self.quit = true;
        self.freeze();
        // TerminateNow ends the process without `run` returning.
        crate::crashlog::clean_exit();
    }

    fn ask_quit(&mut self) {
        let live: Vec<&Arc<Console>> = self
            .consoles
            .values()
            .filter(|c| c.exit_code().is_none())
            .collect();
        let stop = if live.is_empty() {
            false
        } else {
            match dialog::quit(live.len()) {
                Some(stop) => stop,
                None => return,
            }
        };
        self.quit = true;
        if stop {
            super::folio::close_all();
            for c in self.consoles.values() {
                if c.exit_code().is_none() {
                    c.kill();
                }
            }
        }
        self.freeze();
        if stop {
            // The kills go out on threads that end with this process.
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while std::time::Instant::now() < deadline
                && (self.consoles.values()).any(|c| c.folio.is_none() && c.exit_code().is_none())
            {
                thread::sleep(Duration::from_millis(20));
            }
        }
        terminate(self.mtm);
    }
}

/// Ends the app's run loop, so `run` returns and the process exits.
fn terminate(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    app.stop(None);
    // `stop` takes effect after the next event; post one so it is now.
    super::delegate::wake(mtm);
}

/// Lets the stage's snapshot hook reach the windows.
pub(crate) fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok()?.as_mut().map(f))
}

impl App {
    /// Every session's screen as text, by id, whether the stage shows it
    /// or not.
    pub(crate) fn texts(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .consoles
            .iter()
            .filter(|(_, c)| c.folio.is_none())
            .map(|(id, c)| (id.clone(), stage::screen_text(c)))
            .collect();
        out.extend(super::folio::texts());
        out
    }

    /// Every window's view, named for the snapshot files.
    pub(crate) fn views(&self) -> Vec<(String, objc2::rc::Retained<objc2_app_kit::NSView>)> {
        let mut out = Vec::new();
        for (i, c) in self.clusters.iter().enumerate() {
            out.push((format!("cluster-{i}"), c.view().retain()));
        }
        if self.stage.is_visible() {
            out.push(("stage".into(), self.stage.view().retain()));
        }
        // Drawn whether or not it is open: a window need not be on screen
        // to be drawn into a bitmap, and opening it would take the keys.
        let values = self.settings_values();
        out.push((
            "settings".into(),
            super::settings::content(self.mtm, &values),
        ));
        let values = self.usage_values();
        out.push(("usage".into(), super::usage::content(self.mtm, &values)));
        out
    }
}
