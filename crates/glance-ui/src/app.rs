//! The UI thread: owns the cluster windows and the terminal windows, the
//! consoles behind them, and keeps the set of windows in sync with the set
//! of sessions.
//!
//! Sessions share one terminal window, the stage, which shows one project
//! at a time: each of its sessions is a pane in a grid. A click on a tile
//! switches the stage to that tile's project and gives its session the
//! keyboard; the sessions of other projects keep running unseen. A global
//! hotkey brings up the session that has waited on you longest.
//!
//! Everything that happens off the UI thread arrives as a message to one
//! hidden window: hook events from the feeder thread, new session requests
//! from the listener, output and exit from the consoles, clicks queued by the
//! window procedures, and the tray icon. A window, not the thread, because
//! thread messages are dropped while Windows runs a modal loop, and resizing
//! a terminal by its edge is a modal loop. A hidden top level window rather
//! than a message-only one, because only top level windows hear Explorer's
//! `TaskbarCreated` broadcast and the shutdown messages. A one second timer
//! on the same window moves the age lines and saves the state.
//!
//! A browser window a session opens is placed beside the stage when it
//! first appears, and follows its project: switching the stage to another
//! project minimises the browsers of the rest and brings back that one's.
//!
//! What Glance owned is saved to disk as it changes. On the next start the
//! sessions come back as paused tiles, and clicking one resumes its
//! conversation with `claude --resume`.
//!
//! `glance reload` hands the app over to a new build. Once no session is mid
//! turn this one saves, starts the new build's `swap`, and quits. The new
//! app starts with `reload` set and resumes the sessions that were running,
//! so an update costs a few seconds instead of a click on every tile.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::net::TcpStream;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use glance_core::usage::has_flag;
use glance_core::{
    session_id, HookEvent, Phase, Registry, SavedCluster, SavedPanel, SavedSession, SavedState,
    Session, Setting, Usage,
};
use glance_hooks::listener::{self, Command, Reload, Tagged};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONULL};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, VK_SPACE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostMessageW, PostQuitMessage,
    RegisterClassW, RegisterWindowMessageW, SetTimer, SystemParametersInfoW, TranslateMessage, MSG,
    SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WM_APP, WM_HOTKEY, WM_LBUTTONUP,
    WM_QUERYENDSESSION, WM_QUIT, WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::console::{self, Console, Launch};
use crate::glyphs::Font;
use crate::layout::{self, Metrics};
use crate::render::Gpu;
use crate::terminal::{self, Place, TerminalWindow};
use crate::tray::{self, Choice, Item, Tray};
use crate::usage::{self, UsageWindow};
use crate::window::{self, project_key, project_name, Cluster, Shared};
use crate::{autostart, browsers, inbox, picker, recent, shell, snapping, store};

/// A hook event changed the registry. `wparam` is 1 when a phase changed.
const WM_GLANCE_EVENT: u32 = WM_APP + 1;
/// A window procedure queued input with [`push`].
const WM_GLANCE_INPUT: u32 = WM_APP + 2;
/// A console has new output. `wparam` is its serial.
pub(crate) const WM_GLANCE_OUTPUT: u32 = WM_APP + 3;
/// A console's process exited. `wparam` is its serial.
pub(crate) const WM_GLANCE_EXIT: u32 = WM_APP + 4;
/// `glance new` asked for a session, or `glance reload` for a new build.
const WM_GLANCE_NEW: u32 = WM_APP + 5;
/// The tray icon was clicked. `lparam` is the mouse message.
const WM_GLANCE_TRAY: u32 = WM_APP + 6;
/// Show the folder picker, starting where the app's `pick_from` says.
const WM_GLANCE_PICK: u32 = WM_APP + 7;
/// Show the menu for the tile the app's `menu_for` names.
const WM_GLANCE_TILE_MENU: u32 = WM_APP + 8;
/// Show the menu for the project the app's `project_menu_for` names.
const WM_GLANCE_PROJECT_MENU: u32 = WM_APP + 9;
/// A top level window appeared somewhere. `wparam` is its handle.
const WM_GLANCE_WINDOW_SHOWN: u32 = WM_APP + 10;
/// A browser window a session opened is gone. `wparam` is its handle.
const WM_GLANCE_WINDOW_GONE: u32 = WM_APP + 11;
/// Show the menu for the setting the app's `setting_menu_for` names.
const WM_GLANCE_SETTING_MENU: u32 = WM_APP + 12;

const APP_CLASS: PCWSTR = w!("GlanceApp");
/// Runs a console program without giving it a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const HOTKEY_NEXT: i32 = 1;
const ENDED_LINGER: Duration = Duration::from_secs(20);
/// Starts the id of a file view, which is no session.
const VIEW: &str = "view:";
pub(crate) const MARGIN_DIP: i32 = 12;
pub(crate) const GAP_DIP: i32 = 12;
/// Narrower than this beside the stage, a new browser window stays where
/// it opened rather than squeeze in.
const BROWSER_MIN_DIP: i32 = 360;

/// Something a window procedure saw that the app has to act on. Window
/// procedures only get `&self`, and the app owns the windows, so they queue
/// it here and the app applies it right after.
pub(crate) enum Input {
    /// Header clicked: collapse or expand the cluster with this window.
    Toggle(isize),
    /// Cluster dragged: auto layout leaves it alone from now on.
    Pin(isize),
    /// Tile clicked: show this session's terminal, resume it, or collapse it.
    Expand(String),
    /// Tile right clicked: offer what can be done with this session.
    TileMenu(String),
    /// Header or bottom plus right clicked: offer what can be done with
    /// every session of the project with this key.
    ProjectMenu(String),
    /// Header plus clicked: pick a folder for a new project, starting
    /// beside the project with this key.
    New(String),
    /// Bottom plus clicked: another session in the project with this key,
    /// no questions asked.
    Add(String),
    /// A plain terminal in the project with this key, from the button
    /// beside the bottom plus, or in the project on the stage, from
    /// Ctrl+Shift+T in a pane.
    Shell(Option<String>),
    /// Terminal closed: collapse the terminal window with this handle.
    Close(isize),
    /// A pane was dragged onto another: swap these two sessions.
    Swap(String, String),
    /// A tile's browser button clicked: bring up this session's browsers.
    Browser(String),
    /// A file in a files tile clicked: show it on the stage beside the
    /// sessions of the project with this key. The folder, then the file's
    /// path inside it.
    View(String, PathBuf, String),
    /// A file view's cross or Esc: close the view with this serial.
    CloseView(usize),
    /// Files of the project with this key changed on disk.
    FilesChanged(String),
    /// A cluster changed size by itself, its files tile growing or
    /// shrinking, or the usage window was folded or dragged: stack the
    /// windows again.
    Arrange,
    /// A setting in the usage window clicked: offer its values.
    SettingMenu(Setting),
}

thread_local! {
    static INPUT: RefCell<Vec<Input>> = const { RefCell::new(Vec::new()) };
    static APP_WINDOW: Cell<isize> = const { Cell::new(0) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Queues input for the app and wakes it.
pub(crate) fn push(input: Input) {
    INPUT.with(|q| q.borrow_mut().push(input));
    post(APP_WINDOW.with(Cell::get), WM_GLANCE_INPUT, 0);
}

fn post(window: isize, msg: u32, wparam: usize) {
    unsafe {
        let _ = PostMessageW(
            Some(HWND(window as *mut c_void)),
            msg,
            WPARAM(wparam),
            LPARAM(0),
        );
    }
}

/// Runs the whole desktop app on the calling thread until quit. With
/// `reload`, the sessions that were running when the last one saved start
/// again by themselves.
pub fn run(port: u16, reload: bool) -> Result<(), String> {
    // A second copy would fail to listen, then show every saved session a
    // second time. Autostart plus a manual start makes that easy to hit.
    if TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(200))
        .is_ok()
    {
        return Err(format!("Glance is already running (port {port} answers)"));
    }
    run_app(port, reload).map_err(|e| e.to_string())
}

fn run_app(port: u16, reload: bool) -> windows::core::Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // The folder picker is a COM object.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        TASKBAR_CREATED.with(|t| t.set(RegisterWindowMessageW(w!("TaskbarCreated"))));
    }
    window::register_class()?;
    usage::register_class()?;
    terminal::register_class()?;
    let notify = create_app_window()?;
    let notify_id = notify.0 as isize;
    APP_WINDOW.with(|w| w.set(notify_id));
    let hotkey = register_hotkey(notify);
    browsers::watch(notify, WM_GLANCE_WINDOW_SHOWN, WM_GLANCE_WINDOW_GONE);

    let saved = store::load();
    let mut registry = Registry::new();
    let now = SystemTime::now();
    for s in &saved.sessions {
        registry.add(s.to_session(now));
    }
    let registry = Arc::new(Mutex::new(registry));
    let usage = Arc::new(Mutex::new(saved.usage.clone()));
    let requests: Arc<Mutex<Vec<Command>>> = Arc::new(Mutex::new(Vec::new()));

    // Listener thread: sockets in, tagged events and commands out.
    let (tx, rx) = mpsc::channel::<Tagged>();
    let (new_tx, new_rx) = mpsc::channel::<Command>();
    thread::spawn(move || {
        if let Err(e) = listener::serve(port, tx, Some(new_tx)) {
            eprintln!("glance: listener stopped: {e}");
        }
    });

    // Feeder thread: apply to the registry, wake the UI. The limits in a
    // status line are the account's, so the latest from any session wins.
    let feed_registry = Arc::clone(&registry);
    let feed_usage = Arc::clone(&usage);
    thread::spawn(move || {
        for t in rx {
            let limits = t.event.status.as_ref().map(|s| &s.limits);
            if let (Some(limits), Ok(mut u)) = (limits.filter(|l| !l.is_empty()), feed_usage.lock())
            {
                *u = Some(Usage {
                    limits: limits.clone(),
                    at: unix_now(),
                });
            }
            let changed = feed_registry
                .lock()
                .map(|mut r| r.apply(&t.glance_id, &t.event, SystemTime::now()))
                .unwrap_or(false);
            post(notify_id, WM_GLANCE_EVENT, changed as usize);
        }
    });

    // Requests wait in a queue the UI drains when woken.
    let queue = Arc::clone(&requests);
    thread::spawn(move || {
        for n in new_rx {
            if let Ok(mut q) = queue.lock() {
                q.push(n);
            }
            post(notify_id, WM_GLANCE_NEW, 0);
        }
    });

    // Start with Windows is on unless the user switched it off. Only the
    // first run decides it; later runs respect whatever the menu says. A dev
    // instance would point it at a build that is about to be replaced.
    let mut autostart_offered = saved.autostart_offered;
    if !autostart_offered && !glance_hooks::dev() {
        autostart_offered = autostart::enable();
    }

    // An ordinary start leaves every saved session paused until clicked.
    let carry: Vec<String> = if reload {
        saved
            .sessions
            .iter()
            .filter(|s| s.running)
            .map(|s| s.id.clone())
            .collect()
    } else {
        Vec::new()
    };
    let on_stage = saved.on_stage.clone().filter(|_| reload);

    let gpu = Gpu::new()?;
    let font = Font::new(&gpu.dw)?;
    let shared = Rc::new(Shared {
        gpu,
        font,
        metrics: Metrics::default(),
        registry,
        staged: RefCell::new(HashSet::new()),
        browsing: RefCell::new(HashSet::new()),
        usage,
        defaults: RefCell::new(saved.defaults.clone()),
    });
    let status_settings = std::env::current_exe()
        .ok()
        .and_then(|exe| store::write_status_settings(&exe));
    let usage_window = match UsageWindow::create(
        Rc::clone(&shared),
        saved.usage_window.as_ref().is_some_and(|p| p.collapsed),
        -10_000,
        -10_000,
    ) {
        Ok(w) => {
            if let Some(p) = saved.usage_window.as_ref() {
                if p.pinned && on_screen(p.x, p.y) {
                    w.pinned.set(true);
                    w.move_to(p.x, p.y);
                }
            }
            Some(w)
        }
        Err(e) => {
            eprintln!("glance: cannot create the usage window: {e}");
            None
        }
    };
    APP.with(|a| {
        let mut app = App {
            shared,
            usage_window,
            status_settings,
            setting_menu_for: None,
            clusters: Vec::new(),
            consoles: HashMap::new(),
            views: HashMap::new(),
            stage: None,
            stage_rect: saved.stage.filter(|r| on_screen(r[0], r[1])),
            grids: saved.grids.clone().into_iter().collect(),
            hotkey,
            next_serial: 1,
            requests,
            notify,
            tray: Tray::add(notify, WM_GLANCE_TRAY),
            paused: saved
                .sessions
                .iter()
                .map(|s| (s.id.clone(), s.clone()))
                .collect(),
            cluster_places: saved
                .clusters
                .iter()
                .map(|c| (c.key.clone(), c.clone()))
                .collect(),
            recent: saved.recent.clone(),
            autostart_offered,
            last_saved: Some(saved),
            frozen: false,
            pick_from: None,
            menu_for: None,
            project_menu_for: None,
            reload: None,
            browsers: HashMap::new(),
        };
        app.reconcile(false);
        app.carry_on(&carry, on_stage.as_deref());
        *a.borrow_mut() = Some(app);
    });

    unsafe {
        SetTimer(Some(notify), 1, 1000, None);
    }

    let mut msg = MSG::default();
    loop {
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if !got.as_bool() || msg.message == WM_QUIT {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    APP.with(|a| {
        if let Some(mut app) = a.borrow_mut().take() {
            app.freeze();
            if let Some(stage) = &app.stage {
                stage.destroy();
            }
            for c in &app.clusters {
                c.destroy();
            }
            if let Some(u) = &app.usage_window {
                u.destroy();
            }
            for c in app.consoles.values() {
                c.kill();
            }
        }
    });
    Ok(())
}

/// The shortcut for the next waiting session, or None when another app
/// holds it. A dev instance adds Shift, so it never fights the installed
/// Glance for it. AltGr is Ctrl+Alt, and AltGr+Space types nothing on the
/// layouts that matter here.
fn register_hotkey(hwnd: HWND) -> Option<&'static str> {
    let (mods, label) = if glance_hooks::dev() {
        (MOD_CONTROL | MOD_ALT | MOD_SHIFT, "Ctrl+Alt+Shift+Space")
    } else {
        (MOD_CONTROL | MOD_ALT, "Ctrl+Alt+Space")
    };
    let ok = unsafe {
        RegisterHotKey(
            Some(hwnd),
            HOTKEY_NEXT,
            mods | MOD_NOREPEAT,
            VK_SPACE.0 as u32,
        )
    };
    if let Err(e) = &ok {
        eprintln!("glance: {label} is taken, no hotkey for the next waiting session: {e}");
    }
    ok.ok().map(|_| label)
}

fn create_app_window() -> windows::core::Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(app_proc),
            hInstance: instance.into(),
            lpszClassName: APP_CLASS,
            ..Default::default()
        };
        RegisterClassW(&wc);
        // Never shown. A tool window, so it can never reach alt-tab.
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            APP_CLASS,
            w!("Glance"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
}

unsafe extern "system" fn app_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == TASKBAR_CREATED.with(Cell::get) {
        with_app(|app| app.tray.show());
        return LRESULT(0);
    }
    match msg {
        // Windows is shutting down or logging off. Save now, while every
        // session still runs: once their processes are killed they would
        // look like crashes.
        WM_QUERYENDSESSION => {
            with_app(App::freeze);
            return LRESULT(1);
        }
        // Menus and dialogs run modal loops that dispatch messages, including
        // ours. They run here, with the app not borrowed, so those messages
        // are handled rather than bounced.
        WM_GLANCE_TRAY => {
            let mouse = lparam.0 as u32;
            if mouse == WM_LBUTTONUP || mouse == WM_RBUTTONUP {
                tray_menu(hwnd);
            }
            return LRESULT(0);
        }
        WM_GLANCE_PICK => {
            let start = with_app(|app| app.pick_from.take()).flatten();
            pick_and_start(hwnd, start);
            return LRESULT(0);
        }
        WM_GLANCE_TILE_MENU => {
            if let Some(id) = with_app(|app| app.menu_for.take()).flatten() {
                tile_menu(hwnd, &id);
            }
            return LRESULT(0);
        }
        WM_GLANCE_PROJECT_MENU => {
            if let Some(key) = with_app(|app| app.project_menu_for.take()).flatten() {
                project_menu(hwnd, &key);
            }
            return LRESULT(0);
        }
        WM_GLANCE_SETTING_MENU => {
            if let Some(setting) = with_app(|app| app.setting_menu_for.take()).flatten() {
                setting_menu(hwnd, setting);
            }
            return LRESULT(0);
        }
        _ => {}
    }
    let ours = matches!(
        msg,
        WM_GLANCE_EVENT
            | WM_GLANCE_INPUT
            | WM_GLANCE_OUTPUT
            | WM_GLANCE_EXIT
            | WM_GLANCE_NEW
            | WM_GLANCE_WINDOW_SHOWN
            | WM_GLANCE_WINDOW_GONE
            | WM_HOTKEY
            | WM_TIMER
    );
    if !ours {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The app is busy when a call it made pumped messages. Try again later
    // rather than touch it twice. A missed timer tick is not worth repeating.
    if with_app(|app| app.on_message(msg, wparam.0)).is_none() && msg != WM_TIMER {
        let _ = PostMessageW(Some(hwnd), msg, wparam, lparam);
    }
    LRESULT(0)
}

/// Runs `f` on the app unless it is already borrowed.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok()?.as_mut().map(f))
}

fn tray_menu(hwnd: HWND) {
    let (recent, hotkey) = with_app(|app| (app.recent.clone(), app.hotkey)).unwrap_or_default();
    let projects: Vec<String> = recent
        .into_iter()
        .filter(|p| Path::new(p).is_dir())
        .collect();
    let autostart = (!glance_hooks::dev()).then(autostart::is_enabled);
    match tray::menu(hwnd, &projects, autostart, hotkey) {
        Some(Choice::New) => pick_and_start(hwnd, projects.first().map(PathBuf::from)),
        Some(Choice::Recent(path)) => start_logged(PathBuf::from(path)),
        Some(Choice::Tidy) => {
            with_app(App::tidy);
        }
        Some(Choice::Raise) => {
            with_app(App::raise);
        }
        Some(Choice::NextWaiting) => {
            with_app(App::next_waiting);
        }
        Some(Choice::Arrange) => {
            with_app(App::fit_stage);
        }
        Some(Choice::ToggleAutostart) => {
            if autostart::is_enabled() {
                autostart::disable();
            } else {
                autostart::enable();
            }
        }
        Some(Choice::EndAll) => {
            if confirm_end(hwnd, None) {
                with_app(|app| app.end_all(None));
            }
        }
        Some(Choice::Quit) => {
            let (agents, shells) = with_app(|app| app.live_counts()).unwrap_or((0, 0));
            let ok = match quit_question(agents, shells) {
                Some(q) => picker::confirm(hwnd, &q),
                None => true,
            };
            if ok {
                with_app(App::freeze);
                unsafe { PostQuitMessage(0) };
            }
        }
        None => {}
    }
}

/// What to ask before quitting with `agents` sessions and `shells` plain
/// terminals running. Nothing when none is.
fn quit_question(agents: usize, shells: usize) -> Option<String> {
    let sessions = match agents {
        0 => None,
        1 => Some(
            "The running session will stop. It comes back as a paused tile the next time \
             Glance starts, and resumes where it left off."
                .to_string(),
        ),
        n => Some(format!(
            "{n} running sessions will stop. They come back as paused tiles the next time \
             Glance starts, and resume where they left off."
        )),
    };
    let terminals = match shells {
        0 => None,
        1 => Some("The open terminal will close, and whatever runs in it.".to_string()),
        n => Some(format!(
            "{n} open terminals will close, and whatever runs in them."
        )),
    };
    let said: Vec<String> = sessions.into_iter().chain(terminals).collect();
    (!said.is_empty()).then(|| format!("Quit Glance?\n\n{}", said.join(" ")))
}

/// What a tile's menu can offer for its session.
enum TileKind {
    /// Glance runs it right now.
    Live,
    /// Glance can resume it.
    Paused,
    /// Started with `glance run` in someone else's terminal.
    Elsewhere,
}

fn tile_menu(hwnd: HWND, id: &str) {
    const OPEN: usize = 1;
    const END: usize = 2;
    let Some((kind, shell)) = with_app(|app| app.tile_kind(id)).flatten() else {
        return;
    };
    let items = match (kind, shell) {
        (TileKind::Live, false) => vec![
            Item::action(OPEN, "Show terminal"),
            Item::Separator,
            Item::action(END, "End session"),
        ],
        (TileKind::Live, true) => vec![
            Item::action(OPEN, "Show terminal"),
            Item::Separator,
            Item::action(END, "Close terminal"),
        ],
        (TileKind::Paused, false) => vec![
            Item::action(OPEN, "Resume"),
            Item::Separator,
            Item::action(END, "End session"),
        ],
        (TileKind::Paused, true) => vec![
            Item::action(OPEN, "Open again"),
            Item::Separator,
            Item::action(END, "Remove terminal"),
        ],
        (TileKind::Elsewhere, _) => vec![Item::action(END, "Remove tile")],
    };
    match tray::popup(hwnd, &items) {
        Some(OPEN) => {
            with_app(|app| app.reveal(id, false));
        }
        Some(END) => {
            with_app(|app| app.end(id));
        }
        _ => {}
    }
}

/// The values a setting can take, the current one checked. Default, none,
/// leaves it to Claude Code's own settings. A running session keeps what it
/// started with; the menu says so, since that is the surprise.
fn setting_menu(hwnd: HWND, setting: Setting) {
    const DEFAULT: usize = 1;
    const FIRST: usize = 2;
    let Some(current) = with_app(|app| {
        app.shared
            .defaults
            .borrow()
            .get(setting)
            .map(str::to_string)
    }) else {
        return;
    };
    let entry = |id, label: &str, checked| Item::Action {
        id,
        label: label.to_string(),
        checked,
    };
    let mut items = vec![
        Item::Disabled("For sessions started or resumed from now on".into()),
        Item::Separator,
        entry(DEFAULT, "Default", current.is_none()),
        Item::Separator,
    ];
    let choices = setting.choices();
    for (i, (value, label)) in choices.iter().enumerate() {
        if setting == Setting::Permissions && i + 1 == choices.len() {
            items.push(Item::Separator);
        }
        items.push(entry(FIRST + i, label, current.as_deref() == Some(*value)));
    }
    let value = match tray::popup(hwnd, &items) {
        Some(DEFAULT) => None,
        Some(id) if id >= FIRST => match choices.get(id - FIRST) {
            Some((value, _)) => Some(value.to_string()),
            None => return,
        },
        _ => return,
    };
    with_app(|app| app.set_default(setting, value));
}

/// How many sessions a batch starts: enough to work on several things at
/// once, few enough to keep an eye on.
const BATCH: usize = 4;

fn project_menu(hwnd: HWND, key: &str) {
    const ADD: usize = 1;
    const START_BATCH: usize = 2;
    const START_OVER: usize = 3;
    const END_ALL: usize = 4;
    const SHELL: usize = 5;
    let items = [
        Item::action(ADD, "New session"),
        Item::action(START_BATCH, format!("Start {BATCH} sessions")),
        Item::action(START_OVER, format!("Start over with {BATCH} sessions")),
        Item::Separator,
        Item::action(SHELL, "New terminal\tCtrl+Shift+T"),
        Item::Separator,
        Item::action(END_ALL, "End all sessions"),
    ];
    let picked = tray::popup(hwnd, &items);
    let ending = matches!(picked, Some(START_OVER | END_ALL));
    if ending && !confirm_end(hwnd, Some(key)) {
        return;
    }
    with_app(|app| match picked {
        Some(ADD) => app.add_sessions(key, 1),
        Some(START_BATCH) => app.add_sessions(key, BATCH),
        Some(START_OVER) => app.start_over(key, BATCH),
        Some(END_ALL) => app.end_all(Some(key)),
        Some(SHELL) => app.open_shell(key),
        _ => {}
    });
}

/// Asks before ending running sessions. Paused ones cost nothing to lose:
/// their conversations stay on disk for `claude --resume`.
fn confirm_end(hwnd: HWND, key: Option<&str>) -> bool {
    let (live, working) = with_app(|app| app.running_in(key)).unwrap_or((0, 0));
    let project = key.map(project_name);
    match end_question(project.as_deref(), live, working) {
        Some(q) => picker::confirm(hwnd, &q),
        None => true,
    }
}

/// What to ask before ending `live` running sessions, `working` of them mid
/// turn, in one project or in all of them. Nothing when none is running.
fn end_question(project: Option<&str>, live: usize, working: usize) -> Option<String> {
    if live == 0 {
        return None;
    }
    let what = match live {
        1 => "the running session".to_string(),
        n => format!("all {n} running sessions"),
    };
    let place = match project {
        Some(p) => format!(" in {p}"),
        None => String::new(),
    };
    let busy = match working {
        0 => String::new(),
        1 if live == 1 => " It is in the middle of a turn.".to_string(),
        1 => " One of them is in the middle of a turn.".to_string(),
        n => format!(" {n} of them are in the middle of a turn."),
    };
    Some(format!("End {what}{place}?{busy}"))
}

fn pick_and_start(hwnd: HWND, start: Option<PathBuf>) {
    if let Some(dir) = picker::pick_folder(hwnd, start.as_deref()) {
        start_logged(dir);
    }
}

fn start_logged(dir: PathBuf) {
    if let Some(Err(e)) = with_app(|app| app.start(None, dir, Vec::new())) {
        eprintln!("glance: cannot start session: {e}");
    }
}

struct App {
    shared: Rc<Shared>,
    /// The account's usage and the defaults for new sessions.
    usage_window: Option<Box<UsageWindow>>,
    /// The settings file that gives a session `glance status` as its status
    /// line. None when it could not be written, and then sessions go without.
    status_settings: Option<PathBuf>,
    /// The setting whose menu is about to show.
    setting_menu_for: Option<Setting>,
    // Boxed on purpose: the window procedures hold a raw pointer to each
    // window struct, so it must not move when the Vec grows.
    #[allow(clippy::vec_box)]
    clusters: Vec<Box<Cluster>>,
    /// Sessions Glance started itself, by session id. A `glance run`
    /// session has a tile but no console: its terminal is somewhere else.
    consoles: HashMap<String, Arc<Console>>,
    /// Files shown on the stage, at most one per project, by project key.
    /// A click on another file replaces it, like VS Code's preview tab, so
    /// browsing a project never piles up panes.
    views: HashMap<String, Arc<Console>>,
    /// The terminal window, showing one project's sessions, while open.
    stage: Option<Box<TerminalWindow>>,
    /// Where the stage was when it last closed.
    stage_rect: Option<[i32; 4]>,
    /// Each project's sessions in the order the stage shows them, by
    /// project key. Paused ones keep their place.
    grids: HashMap<String, Vec<String>>,
    /// The next waiting session's shortcut, as the tray menu shows it.
    hotkey: Option<&'static str>,
    next_serial: usize,
    requests: Arc<Mutex<Vec<Command>>>,
    notify: HWND,
    tray: Tray,
    /// Sessions with no process that a click resumes: restored from disk,
    /// or left behind by a crash.
    paused: HashMap<String, SavedSession>,
    /// Where each project's cluster was, applied when the cluster appears.
    cluster_places: HashMap<String, SavedCluster>,
    /// Projects sessions were started in, newest first, for the tray menu.
    recent: Vec<String>,
    autostart_offered: bool,
    last_saved: Option<SavedState>,
    /// Set once Glance is quitting or Windows is shutting down. The state
    /// on disk is final then: sessions dying on the way out must not be
    /// saved as gone.
    frozen: bool,
    /// Where the next folder picker opens. Set by the plus button.
    pick_from: Option<PathBuf>,
    /// The tile whose menu is about to show.
    menu_for: Option<String>,
    /// The project whose menu is about to show.
    project_menu_for: Option<String>,
    /// A new build to hand over to, once no session is mid turn.
    reload: Option<Reload>,
    /// Browser windows sessions opened, by window handle.
    browsers: HashMap<isize, Browser>,
}

/// A browser window a session opened.
struct Browser {
    session: String,
    /// Minimised by a project switch, so switching back restores it. One
    /// the user minimised stays minimised.
    hidden: bool,
}

impl App {
    fn on_message(&mut self, msg: u32, wparam: usize) {
        match msg {
            WM_GLANCE_EVENT => self.reconcile(wparam != 0),
            WM_GLANCE_INPUT => self.apply_input(),
            WM_GLANCE_OUTPUT => self.output(wparam),
            WM_GLANCE_WINDOW_SHOWN => self.window_shown(wparam as isize),
            WM_GLANCE_WINDOW_GONE => self.window_gone(wparam as isize),
            WM_GLANCE_EXIT => self.exited(wparam),
            WM_GLANCE_NEW => {
                let pending = self
                    .requests
                    .lock()
                    .map(|mut q| std::mem::take(&mut *q))
                    .unwrap_or_default();
                for command in pending {
                    match command {
                        Command::New(n) => {
                            if let Err(e) = self.start(n.name, PathBuf::from(n.cwd), n.args) {
                                eprintln!("glance: cannot start session: {e}");
                            }
                        }
                        Command::Reload(r) => {
                            self.reload = Some(r);
                            self.reconcile(false);
                            self.reload_when_ready();
                        }
                    }
                }
            }
            WM_HOTKEY if wparam as i32 == HOTKEY_NEXT => self.next_waiting(),
            WM_TIMER => {
                self.tick();
                self.reload_when_ready();
            }
            _ => {}
        }
    }

    fn console_by_serial(&self, serial: usize) -> Option<&Arc<Console>> {
        self.consoles
            .values()
            .chain(self.views.values())
            .find(|c| c.serial == serial)
    }

    /// A session's console, or a file view's, by its id.
    fn console_of(&self, id: &str) -> Option<&Arc<Console>> {
        self.consoles
            .get(id)
            .or_else(|| self.views.values().find(|v| v.id == id))
    }

    /// The stage, when it shows this console.
    fn stage_showing(&self, serial: usize) -> Option<&TerminalWindow> {
        self.stage.as_deref().filter(|s| s.shows(serial))
    }

    /// Running agents and running plain terminals.
    fn live_counts(&self) -> (usize, usize) {
        let live = self.consoles.values().filter(|c| c.exit_code().is_none());
        let shells = live.clone().filter(|c| c.shell).count();
        (live.count() - shells, shells)
    }

    /// Sessions in a Glance terminal that are in the middle of a turn.
    fn mid_turn_count(&self) -> usize {
        let Ok(r) = self.shared.registry.lock() else {
            return 0;
        };
        self.consoles
            .iter()
            .filter(|(id, c)| {
                c.exit_code().is_none() && r.get(id).is_some_and(|s| s.phase.mid_turn())
            })
            .count()
    }

    /// Hands over to the new build once no session would be cut off mid
    /// turn: saves, starts the new build's `swap`, and quits. `swap` waits
    /// for this process to exit, installs the build, and starts it with
    /// `--reload`, which resumes the sessions saved as running.
    fn reload_when_ready(&mut self) {
        let Some(reload) = &self.reload else {
            return;
        };
        if !reload.now && self.mid_turn_count() > 0 {
            return;
        }
        let exe = PathBuf::from(&reload.exe);
        self.freeze();
        let started = std::process::Command::new(&exe)
            .args(["swap", "--pid", &std::process::id().to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
        match started {
            Ok(_) => unsafe { PostQuitMessage(0) },
            Err(e) => {
                eprintln!(
                    "glance: cannot reload, {} did not start: {e}",
                    exe.display()
                );
                self.reload = None;
                self.frozen = false;
                self.reconcile(false);
            }
        }
    }

    /// After a reload: starts again the sessions that were running, without
    /// opening a window for each, and puts back the one that was on stage.
    fn carry_on(&mut self, ids: &[String], on_stage: Option<&str>) {
        for id in ids {
            if let Err(e) = self.resume(id, false) {
                eprintln!("glance: cannot resume {id}: {e}");
            }
        }
        if let Some(id) = on_stage.filter(|id| self.consoles.contains_key(*id)) {
            self.reveal(id, false);
        }
    }

    /// What a tile's menu offers, and whether it is a plain terminal.
    fn tile_kind(&self, id: &str) -> Option<(TileKind, bool)> {
        let shell = self.shared.registry.lock().ok()?.get(id)?.shell;
        let kind = if self.paused.contains_key(id) {
            TileKind::Paused
        } else if self
            .consoles
            .get(id)
            .is_some_and(|c| c.exit_code().is_none())
        {
            TileKind::Live
        } else {
            TileKind::Elsewhere
        };
        Some((kind, shell))
    }

    fn output(&mut self, serial: usize) {
        let Some(console) = self.console_by_serial(serial) else {
            return;
        };
        if !console.take_dirty() {
            return;
        }
        if let Some(t) = self.stage_showing(serial) {
            t.refresh(serial);
        }
        // No hook reports on a shell, so its tile tells what it runs from
        // the terminal's title and how busy it is from its output. The
        // tiles redraw on the next tick.
        if console.shell {
            let title = console.title();
            if let Ok(mut r) = self.shared.registry.lock() {
                if let Some(s) = r.get_mut(&console.id) {
                    s.touch(SystemTime::now());
                    s.last_line = title.unwrap_or_default();
                }
            }
        }
    }

    /// The agent exited. A clean exit, as from `/exit`, ends the session.
    /// Anything else, a crash or a kill, leaves it paused, so a click
    /// resumes it instead of losing it.
    fn exited(&mut self, serial: usize) {
        if self.frozen {
            return;
        }
        let Some(console) = self.console_by_serial(serial).cloned() else {
            return;
        };
        // A shell has nothing to resume, and `exit` means done with it
        // whatever code the last command left behind.
        if console.shell {
            self.forget(&console.id);
            self.reconcile(false);
            return;
        }
        if console.exit_code() == Some(0) {
            if let Ok(mut r) = self.shared.registry.lock() {
                if r.get(&console.id).is_some() {
                    r.apply(
                        &console.id,
                        &HookEvent::synthetic("SessionEnd"),
                        SystemTime::now(),
                    );
                }
            }
        } else {
            if let Ok(mut r) = self.shared.registry.lock() {
                if let Some(s) = r.get(&console.id) {
                    let saved = SavedSession::from_session(s, console.args.clone(), false);
                    self.paused.insert(console.id.clone(), saved);
                    r.apply(
                        &console.id,
                        &HookEvent::synthetic(HookEvent::PAUSE),
                        SystemTime::now(),
                    );
                }
            }
            // The pane stays, so whatever went wrong can be read.
            if let Some(t) = self.stage_showing(serial) {
                t.refresh(serial);
            }
        }
        // A clean exit leaves the stage here.
        self.reconcile(true);
    }

    /// Starts an agent in a new session and opens its terminal.
    fn start(
        &mut self,
        name: Option<String>,
        cwd: PathBuf,
        args: Vec<String>,
    ) -> Result<(), String> {
        if !cwd.is_dir() {
            return Err(format!("{} is not a directory", cwd.display()));
        }
        let folder = folder_name(&cwd);
        let base = name.clone().unwrap_or_else(|| folder.clone());
        let id = self.unique_id(&base);
        let shown = name.unwrap_or(folder);
        self.launch(&id, &shown, cwd, args, false, false)?;
        // A new session is where the eye already is: against the tiles, not
        // wherever the stage was left.
        if let Some(key) = self.project_of(&id) {
            if self.fill_stage(&key, true) {
                if let Some(stage) = &self.stage {
                    stage.focus_session(&id);
                }
            }
        }
        Ok(())
    }

    /// Resumes a paused session in the same tile, with its conversation.
    /// With `show`, its terminal opens too.
    fn resume(&mut self, id: &str, show: bool) -> Result<(), String> {
        // Out of `paused` before launching, not after: launching opens the
        // window through `expand`, and a session still marked paused there
        // resumes again, and again, one `claude` each time.
        let Some(saved) = self.paused.remove(id) else {
            return Ok(());
        };
        // A console that crashed may still show its last screen. The resumed
        // one takes its pane's place.
        self.consoles.remove(id);
        let cwd = PathBuf::from(&saved.cwd);
        let result = if cwd.is_dir() {
            let args = saved.launch_args();
            self.launch(id, &saved.name, cwd, args, saved.shell, show)
        } else {
            Err(format!("{} no longer exists", cwd.display()))
        };
        if result.is_err() {
            self.paused.insert(id.to_string(), saved);
        }
        result
    }

    /// Opens a plain terminal in the project with this key, on the stage
    /// beside its sessions, with the keyboard.
    fn open_shell(&mut self, key: &str) {
        let Some(dir) = self.project_dir(key) else {
            return;
        };
        let n = self
            .shared
            .registry
            .lock()
            .map(|r| r.all().filter(|s| s.shell && project_key(s) == key).count())
            .unwrap_or(0);
        let id = self.unique_id("terminal");
        if let Err(e) = self.launch(&id, &shell::name(n), dir, Vec::new(), true, false) {
            eprintln!("glance: cannot open a terminal: {e}");
            return;
        }
        if self.fill_stage(key, false) {
            if let Some(stage) = &self.stage {
                stage.focus_session(&id);
            }
        }
    }

    /// Registers a session, starts its console and, with `show`, opens its
    /// terminal. With `shell`, the console runs a plain shell instead of
    /// the agent.
    fn launch(
        &mut self,
        id: &str,
        name: &str,
        cwd: PathBuf,
        args: Vec<String>,
        shell: bool,
        show: bool,
    ) -> Result<(), String> {
        // One agent per session, whatever path led here. A second would be
        // a runaway, not a feature.
        if self
            .consoles
            .get(id)
            .is_some_and(|c| c.exit_code().is_none())
        {
            return Err(format!("{id} is already running"));
        }
        let program = if shell {
            console::shell_program().ok_or("no shell found (set GLANCE_SHELL)")?
        } else {
            console::agent_program().ok_or("claude.exe not found on PATH (or set GLANCE_AGENT)")?
        };
        recent::remember(&mut self.recent, &cwd.to_string_lossy());

        let register = HookEvent {
            cwd: cwd.to_string_lossy().to_string(),
            name: Some(name.to_string()),
            ..HookEvent::synthetic(HookEvent::REGISTER)
        };
        let was_known = self
            .shared
            .registry
            .lock()
            .map(|mut r| {
                let known = r.get(id).is_some();
                r.apply(id, &register, SystemTime::now());
                if let Some(s) = r.get_mut(id) {
                    s.shell = shell;
                }
                known
            })
            .unwrap_or(false);

        let extra = self.extra_args(&program, &args);
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
            },
            self.notify,
        )
        .map_err(|e| {
            if let Ok(mut r) = self.shared.registry.lock() {
                if was_known {
                    r.apply(
                        id,
                        &HookEvent::synthetic(HookEvent::PAUSE),
                        SystemTime::now(),
                    );
                } else {
                    r.remove(id);
                }
            }
            format!("could not start the agent: {e}")
        })?;
        self.consoles.insert(id.to_string(), console);
        self.reconcile(true);
        if show {
            self.reveal(id, false);
        }
        Ok(())
    }

    /// What goes before a session's own arguments this time: the defaults
    /// from the usage window, and the status line that feeds it. Only for
    /// Claude Code, not for a shell put in its place with `GLANCE_AGENT`,
    /// and not over settings the session brought itself.
    fn extra_args(&self, program: &Path, args: &[String]) -> Vec<String> {
        let claude = program
            .file_stem()
            .is_some_and(|s| s.eq_ignore_ascii_case("claude"));
        if !claude {
            return Vec::new();
        }
        let mut extra = self.shared.defaults.borrow().flags(args);
        if let (Some(path), false) = (&self.status_settings, has_flag(args, "--settings")) {
            extra.push("--settings".into());
            extra.push(path.to_string_lossy().into_owned());
        }
        extra
    }

    /// A setting picked in the usage window. It reaches sessions as they
    /// start or resume.
    fn set_default(&mut self, setting: Setting, value: Option<String>) {
        self.shared.defaults.borrow_mut().set(setting, value);
        if let Some(u) = &self.usage_window {
            u.invalidate();
        }
        self.save();
    }

    /// Ends a session for good: the process, the window, the tile, and its
    /// place in the saved state.
    fn end(&mut self, id: &str) {
        self.forget(id);
        self.reconcile(false);
    }

    /// Ends every session of the project with this key, or of every
    /// project.
    fn end_all(&mut self, key: Option<&str>) {
        let ids: Vec<String> = match self.shared.registry.lock() {
            Ok(r) => r
                .all()
                .filter(|s| key.is_none_or(|k| project_key(s) == k))
                .map(|s| s.id.clone())
                .collect(),
            Err(_) => return,
        };
        for id in &ids {
            self.forget(id);
        }
        self.reconcile(false);
    }

    /// Starts `n` more sessions in the project with this key.
    fn add_sessions(&mut self, key: &str, n: usize) {
        let Some(dir) = self.project_dir(key) else {
            return;
        };
        for _ in 0..n {
            if let Err(e) = self.start(None, dir.clone(), Vec::new()) {
                eprintln!("glance: cannot start session: {e}");
                return;
            }
        }
    }

    /// Replaces the project's sessions with `n` new ones. The new ones
    /// start first, so the cluster never empties and keeps its place. Its
    /// plain terminals stay: a dev server has nothing to do with starting
    /// over.
    fn start_over(&mut self, key: &str, n: usize) {
        let old: Vec<String> = match self.shared.registry.lock() {
            Ok(r) => r
                .all()
                .filter(|s| !s.shell && project_key(s) == key)
                .map(|s| s.id.clone())
                .collect(),
            Err(_) => return,
        };
        self.add_sessions(key, n);
        for id in &old {
            self.forget(id);
        }
        self.reconcile(false);
    }

    /// Running sessions in the project with this key, or in every project,
    /// and how many of them are mid turn.
    fn running_in(&self, key: Option<&str>) -> (usize, usize) {
        let Ok(r) = self.shared.registry.lock() else {
            return (0, 0);
        };
        let running: Vec<&Session> = self
            .consoles
            .iter()
            .filter(|(_, c)| c.exit_code().is_none())
            .filter_map(|(id, _)| r.get(id))
            .filter(|s| key.is_none_or(|k| project_key(s) == k))
            .collect();
        let working = running.iter().filter(|s| s.phase.mid_turn()).count();
        (running.len(), working)
    }

    /// Kills a session's process and drops every trace of it, without
    /// updating the windows.
    fn forget(&mut self, id: &str) {
        if let Some(c) = self.consoles.remove(id) {
            if c.exit_code().is_none() {
                c.kill();
            }
        }
        self.paused.remove(id);
        self.browsers.retain(|&hwnd, b| {
            let keep = b.session != id;
            if !keep {
                browsers::untrack(hwnd_of(hwnd));
            }
            keep
        });
        for order in self.grids.values_mut() {
            order.retain(|s| s != id);
        }
        if let Ok(mut r) = self.shared.registry.lock() {
            r.remove(id);
        }
    }

    fn unique_id(&self, base: &str) -> String {
        let id = session_id(base, SystemTime::now());
        let taken = |candidate: &str| {
            self.consoles.contains_key(candidate)
                || self.paused.contains_key(candidate)
                || self
                    .shared
                    .registry
                    .lock()
                    .map(|r| r.get(candidate).is_some())
                    .unwrap_or(false)
        };
        if !taken(&id) {
            return id;
        }
        (2..)
            .map(|n| format!("{id}-{n}"))
            .find(|c| !taken(c))
            .expect("an unbounded range always finds a free id")
    }

    /// Shows a session's terminal: resumes it when paused, otherwise puts
    /// its project on the stage and gives it the keyboard. With `toggle`,
    /// a session that already has the keyboard in front collapses the
    /// stage instead, which is what a second click on its tile means.
    fn reveal(&mut self, id: &str, toggle: bool) {
        if self.paused.contains_key(id) {
            if let Err(e) = self.resume(id, true) {
                eprintln!("glance: cannot resume {id}: {e}");
            }
            return;
        }
        let Some(serial) = self.consoles.get(id).map(|c| c.serial) else {
            // Started with `glance run`: its terminal is the one it was run in.
            return;
        };
        if let Some(stage) = self.stage_showing(serial) {
            if toggle && stage.is_foreground() && stage.active().as_deref() == Some(id) {
                self.close_stage();
            } else {
                stage.focus_session(id);
            }
            return;
        }
        let Some(key) = self.project_of(id) else {
            return;
        };
        if self.fill_stage(&key, false) {
            if let Some(stage) = &self.stage {
                stage.focus_session(id);
            }
        }
    }

    fn project_of(&self, id: &str) -> Option<String> {
        self.shared.registry.lock().ok()?.get(id).map(project_key)
    }

    /// The sessions of a project that have a console to show, in grid
    /// order. A clean exit has nothing left worth reading; a crash keeps its
    /// pane until resumed.
    fn grid_of(&mut self, key: &str) -> Vec<String> {
        let mut live: Vec<(usize, String)> = {
            let Ok(r) = self.shared.registry.lock() else {
                return Vec::new();
            };
            self.consoles
                .iter()
                .filter(|(id, c)| {
                    c.exit_code() != Some(0) && r.get(id).is_some_and(|s| project_key(s) == key)
                })
                .map(|(id, c)| (c.serial, id.clone()))
                .collect()
        };
        // New ones join in the order they started.
        live.sort();
        let live: Vec<String> = live.into_iter().map(|(_, id)| id).collect();
        let mut ids = layout::grid_order(self.grids.entry(key.to_string()).or_default(), &live);
        // A file view comes last and is never saved in the order.
        if let Some(view) = self.views.get(key) {
            ids.push(view.id.clone());
        }
        ids
    }

    /// Puts a project's sessions on the stage, opening it when closed.
    /// With `dock`, the stage moves against the tiles as a square filling
    /// top to bottom. False when the project has nothing to show.
    fn fill_stage(&mut self, key: &str, dock: bool) -> bool {
        let ids = self.grid_of(key);
        let sessions: Vec<(Arc<Console>, String)> = ids
            .iter()
            .filter_map(|id| Some((Arc::clone(self.console_of(id)?), self.name_of(id))))
            .collect();
        if sessions.is_empty() {
            return false;
        }
        let switched = self.stage.as_ref().map(|s| s.project()).as_deref() != Some(key);
        let (area, tiles_left) = self.stage_area();
        let docked = layout::square(area, tiles_left);
        if let Some(stage) = self.stage.as_ref().filter(|_| dock) {
            stage.set_visible_rect(docked);
        }
        if self.stage.is_none() {
            let place = match self.stage_rect {
                Some(r) if !dock => Place::Rect(r),
                _ => Place::Visible(docked),
            };
            match TerminalWindow::open(Rc::clone(&self.shared), place) {
                Ok(t) => self.stage = Some(t),
                Err(e) => {
                    eprintln!("glance: cannot open the stage: {e}");
                    return false;
                }
            }
        }
        if let Some(stage) = &self.stage {
            stage.show(key, &project_name(key), sessions);
        }
        self.mark_staged();
        if switched {
            self.browsers_follow(key);
        }
        true
    }

    /// A window appeared. When it is a browser that one of our sessions
    /// started, however far down, the session takes it on and it moves
    /// beside the stage.
    fn window_shown(&mut self, id: isize) {
        if self.browsers.contains_key(&id) {
            return;
        }
        let hwnd = hwnd_of(id);
        let Some(process) = browsers::browser_process(hwnd) else {
            return;
        };
        let process = HANDLE(process.as_raw_handle());
        let Some(session) = self
            .consoles
            .iter()
            .find(|(_, c)| c.contains(process))
            .map(|(id, _)| id.clone())
        else {
            return;
        };
        browsers::track(hwnd);
        self.browsers.insert(
            id,
            Browser {
                session,
                hidden: false,
            },
        );
        self.place_browser(hwnd);
        self.mark_browsers();
    }

    fn window_gone(&mut self, id: isize) {
        browsers::untrack(hwnd_of(id));
        if self.browsers.remove(&id).is_some() {
            self.mark_browsers();
        }
    }

    /// Moves a new browser window into the space beside the stage, or
    /// beside where a new session would dock the stage when it is closed.
    fn place_browser(&self, hwnd: HWND) {
        let Some(size) = browsers::size(hwnd) else {
            return;
        };
        let (area, tiles_left) = self.stage_area();
        let stage = self
            .stage
            .as_ref()
            .and_then(|s| snapping::visible_rect(s.hwnd).ok())
            .map(|r| [r.left, r.top, r.right, r.bottom])
            .unwrap_or_else(|| layout::square(area, tiles_left));
        let place = layout::beside_stage(
            work_area(),
            &self.tile_rects(),
            stage,
            size,
            self.px(MARGIN_DIP),
            self.px(GAP_DIP),
            self.px(BROWSER_MIN_DIP),
        );
        if let Some(rect) = place {
            browsers::place(hwnd, rect);
        }
    }

    /// The stage switched to the project with this key: its sessions'
    /// browsers come back, just below the stage, and every other project's
    /// get out of the way.
    fn browsers_follow(&mut self, key: &str) {
        let Some(stage) = self.stage.as_ref().map(|s| s.hwnd) else {
            return;
        };
        let projects: HashMap<String, String> = match self.shared.registry.lock() {
            Ok(r) => self
                .browsers
                .values()
                .filter_map(|b| Some((b.session.clone(), project_key(r.get(&b.session)?))))
                .collect(),
            Err(_) => return,
        };
        for (&id, b) in self.browsers.iter_mut() {
            let hwnd = hwnd_of(id);
            match projects.get(&b.session) {
                // One the user minimised stays down.
                Some(p) if p == key => {
                    if b.hidden || !browsers::minimised(hwnd) {
                        browsers::show_below(hwnd, stage);
                    }
                    b.hidden = false;
                }
                Some(_) => b.hidden |= browsers::hide(hwnd),
                None => {}
            }
        }
    }

    /// Brings up a session's browser windows, from a click on its tile.
    fn show_browsers(&mut self, id: &str) {
        for (&hwnd, b) in self.browsers.iter_mut() {
            if b.session == id {
                browsers::bring_to_front(hwnd_of(hwnd));
                b.hidden = false;
            }
        }
    }

    /// Tells the tiles which sessions have a browser open.
    fn mark_browsers(&mut self) {
        self.browsers.retain(|&id, _| browsers::exists(hwnd_of(id)));
        let now: HashSet<String> = self.browsers.values().map(|b| b.session.clone()).collect();
        if *self.shared.browsing.borrow() != now {
            *self.shared.browsing.borrow_mut() = now;
            for c in &self.clusters {
                c.fit();
            }
        }
    }

    /// Makes the stage match its project: sessions that started or resumed
    /// there join, ended ones leave, and with none left it closes.
    fn sync_stage(&mut self) {
        let Some(key) = self.stage.as_ref().map(|s| s.project()) else {
            return;
        };
        if !self.fill_stage(&key, false) {
            self.close_stage();
        }
    }

    /// Swaps two sessions' places in the stage's grid.
    fn swap(&mut self, a: &str, b: &str) {
        let Some(key) = self.stage.as_ref().map(|s| s.project()) else {
            return;
        };
        if let Some(order) = self.grids.get_mut(&key) {
            let i = order.iter().position(|s| s == a);
            let j = order.iter().position(|s| s == b);
            if let (Some(i), Some(j)) = (i, j) {
                order.swap(i, j);
            }
        }
        self.sync_stage();
    }

    /// A session's name, what its pane's header says.
    fn name_of(&self, id: &str) -> String {
        self.shared
            .registry
            .lock()
            .ok()
            .and_then(|r| r.get(id).map(|s| s.label().to_string()))
            .or_else(|| {
                let path = self.console_of(id)?.path()?;
                Some(path.file_name()?.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| id.to_string())
    }

    /// Shows a file of a project on the stage, beside its sessions, with
    /// the keyboard. It takes the place of the file shown there before.
    fn open_view(&mut self, key: &str, dir: &Path, rel: &str) {
        let path = dir.join(rel.replace('/', "\\"));
        let id = format!("{VIEW}{key}");
        match self.views.get(key) {
            Some(v) if v.path().as_deref() == Some(path.as_path()) => {
                v.reload(self.notify);
            }
            _ => {
                let serial = self.next_serial;
                self.next_serial += 1;
                // The header names the file, then the folder it is in.
                let folder = rel.rsplit_once('/').map_or("", |(f, _)| f).to_string();
                let view = Console::view(id.clone(), serial, path, folder, self.notify);
                self.views.insert(key.to_string(), view);
            }
        }
        if self.fill_stage(key, false) {
            if let Some(stage) = &self.stage {
                stage.focus_session(&id);
            }
        }
    }

    fn close_view(&mut self, serial: usize) {
        self.views.retain(|_, v| v.serial != serial);
        self.sync_stage();
    }

    /// A project's files changed: its view reads its file again, if that
    /// was one of them.
    fn files_changed(&self, key: &str) {
        let Some(view) = self.views.get(key) else {
            return;
        };
        if view.reload(self.notify) {
            if let Some(stage) = self.stage_showing(view.serial) {
                stage.refresh(view.serial);
            }
        }
    }

    /// Tells the tiles which sessions are on the stage.
    fn mark_staged(&self) {
        let now: HashSet<String> = self
            .stage
            .as_ref()
            .map(|s| s.sessions().into_iter().collect())
            .unwrap_or_default();
        if *self.shared.staged.borrow() != now {
            *self.shared.staged.borrow_mut() = now;
            for c in &self.clusters {
                c.invalidate();
            }
        }
    }

    /// Closing the stage is done with looking, so file views go with it.
    fn close_stage(&mut self) {
        self.views.clear();
        if let Some(stage) = self.stage.take() {
            if let Some(r) = stage.rect() {
                self.stage_rect = Some(r);
            }
            stage.destroy();
        }
        self.mark_staged();
    }

    /// Shows the session that has waited on you longest. When the one in
    /// front is waiting too, the next one after it, so pressing again moves
    /// on past a session you are not ready to answer.
    fn next_waiting(&mut self) {
        let waiting: Vec<String> = self
            .shared
            .registry
            .lock()
            .map(|r| {
                r.waiting()
                    .iter()
                    .filter(|s| self.consoles.contains_key(&s.id))
                    .map(|s| s.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let front = self
            .stage
            .as_ref()
            .filter(|s| s.is_foreground())
            .and_then(|s| s.active());
        match inbox::next(&waiting, front.as_deref()).map(str::to_string) {
            Some(id) => self.reveal(&id, false),
            None => {
                if let Some(stage) = &self.stage {
                    stage.bring_to_front();
                }
            }
        }
    }

    /// The space beside the clusters, as visible edges, and whether they
    /// are on its left: where a new session docks the stage, and what "Fit
    /// terminal beside tiles" fills. Measured from where the clusters are,
    /// so one moved to the right puts the stage on the left.
    fn stage_area(&self) -> ([i32; 4], bool) {
        layout::beside(
            work_area(),
            &self.tile_rects(),
            self.px(MARGIN_DIP),
            self.px(GAP_DIP),
        )
    }

    /// Where each cluster and the usage window are, as (left, top, right,
    /// bottom).
    fn tile_rects(&self) -> Vec<[i32; 4]> {
        let usage = self
            .usage_window
            .iter()
            .map(|u| (u.position(), u.size_px()));
        self.clusters
            .iter()
            .map(|c| (c.position(), c.size_px()))
            .chain(usage)
            .map(|((x, y), (w, h))| [x, y, x + w, y + h])
            .collect()
    }

    /// DIPs in physical pixels, at the DPI the clusters are drawn at.
    fn px(&self, dip: i32) -> i32 {
        let dpi = self.clusters.first().map_or(96, |c| c.dpi());
        (dip as f32 * dpi as f32 / 96.0).round() as i32
    }

    /// Fills the space beside the clusters with the stage.
    fn fit_stage(&mut self) {
        let (area, _) = self.stage_area();
        if let Some(stage) = &self.stage {
            stage.set_visible_rect(area);
        }
    }

    /// Once a second: drop long ended sessions and their consoles, redraw
    /// ages, save what changed.
    fn tick(&mut self) {
        let running: Vec<&String> = self
            .consoles
            .iter()
            .filter(|(_, c)| c.exit_code().is_none())
            .map(|(id, _)| id)
            .collect();
        let pruned = self
            .shared
            .registry
            .lock()
            .map(|mut r| {
                // A hook can say a session ended while its process runs on.
                // Every `claude` started in its terminal inherits its tag,
                // so a `claude -p` there ends the tile when it quits. Only
                // the process exiting ends a session Glance runs.
                for id in running {
                    if r.get(id).is_some_and(|s| s.phase == Phase::Ended) {
                        r.apply(
                            id,
                            &HookEvent::synthetic(HookEvent::REGISTER),
                            SystemTime::now(),
                        );
                    }
                }
                r.prune_ended(ENDED_LINGER, SystemTime::now())
            })
            .unwrap_or(0);
        if pruned > 0 {
            // A console goes with its tile once its process is gone. One
            // still running keeps going: it will be adopted again by its
            // next hook.
            // The stage follows in `reconcile`.
            let gone: Vec<String> = self
                .consoles
                .iter()
                .filter(|(id, c)| {
                    c.exit_code().is_some()
                        && self
                            .shared
                            .registry
                            .lock()
                            .map(|r| r.get(id).is_none())
                            .unwrap_or(false)
                })
                .map(|(id, _)| id.clone())
                .collect();
            for id in gone {
                self.consoles.remove(&id);
            }
            self.reconcile(true);
        } else {
            for c in &self.clusters {
                c.invalidate();
            }
        }
        self.save();
    }

    /// Everything worth bringing back, as it is right now.
    fn snapshot(&self) -> SavedState {
        let mut sessions = Vec::new();
        if let Ok(r) = self.shared.registry.lock() {
            for s in r.all() {
                if let Some(p) = self.paused.get(&s.id) {
                    sessions.push(SavedSession::from_session(s, p.args.clone(), false));
                } else if let Some(c) = self.consoles.get(&s.id) {
                    if c.exit_code().is_none() && s.phase != Phase::Ended {
                        sessions.push(SavedSession::from_session(s, c.args.clone(), true));
                    }
                }
            }
        }
        let clusters = self
            .clusters
            .iter()
            .map(|c| {
                let (x, y) = c.position();
                SavedCluster {
                    key: c.key.clone(),
                    pinned: c.pinned,
                    x,
                    y,
                    collapsed: c.collapsed,
                    files_collapsed: c.files_collapsed(),
                    files_height: c.files_height(),
                }
            })
            .collect();
        SavedState {
            clusters,
            recent: self.recent.clone(),
            autostart_offered: self.autostart_offered,
            stage: self
                .stage
                .as_ref()
                .and_then(|s| s.rect())
                .or(self.stage_rect),
            on_stage: self
                .stage
                .as_ref()
                .and_then(|s| s.active())
                .filter(|id| !id.starts_with(VIEW)),
            grids: self
                .grids
                .iter()
                .map(|(key, order)| {
                    let kept: Vec<String> = order
                        .iter()
                        .filter(|id| sessions.iter().any(|s| &s.id == *id))
                        .cloned()
                        .collect();
                    (key.clone(), kept)
                })
                .filter(|(_, order)| !order.is_empty())
                .collect(),
            sessions,
            defaults: self.shared.defaults.borrow().clone(),
            usage: self.shared.usage.lock().ok().and_then(|u| u.clone()),
            usage_window: self.usage_window.as_ref().map(|u| {
                let (x, y) = u.position();
                SavedPanel {
                    pinned: u.pinned.get(),
                    x,
                    y,
                    collapsed: u.collapsed.get(),
                }
            }),
            ..Default::default()
        }
    }

    /// Writes the state when it changed since the last write.
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

    /// Makes the windows match the projects in the registry.
    fn reconcile(&mut self, phase_changed: bool) {
        let projects: HashMap<String, usize> = self
            .shared
            .registry
            .lock()
            .map(|r| {
                let mut m = HashMap::new();
                for s in r.all() {
                    *m.entry(project_key(s)).or_insert(0) += 1;
                }
                m
            })
            .unwrap_or_default();

        // Remove clusters whose project is gone.
        let mut i = 0;
        while i < self.clusters.len() {
            if projects.contains_key(&self.clusters[i].key) {
                i += 1;
            } else {
                let c = self.clusters.remove(i);
                c.destroy();
            }
        }

        // Add clusters for new projects, off screen until laid out, or where
        // the user last put them.
        for key in projects.keys() {
            if self.clusters.iter().any(|c| &c.key == key) {
                continue;
            }
            match Cluster::create(
                Rc::clone(&self.shared),
                key.clone(),
                project_name(key),
                self.project_dir(key),
                -10_000,
                -10_000,
            ) {
                Ok(mut c) => {
                    if let Some(place) = self.cluster_places.get(key) {
                        c.collapsed = place.collapsed;
                        c.set_files_collapsed(place.files_collapsed);
                        c.set_files_height(place.files_height);
                        // A monitor that is gone would leave it unreachable.
                        if place.pinned && on_screen(place.x, place.y) {
                            c.pinned = true;
                            c.move_to(place.x, place.y);
                        }
                    }
                    self.clusters.push(c);
                }
                Err(e) => eprintln!("glance: cannot create window: {e}"),
            }
        }

        for c in &self.clusters {
            c.fit();
        }
        // A status line can bring the first limits, which adds rows.
        if let Some(u) = &self.usage_window {
            u.fit();
        }
        self.arrange();
        if std::env::var_os("GLANCE_DEBUG").is_some() {
            for c in &self.clusters {
                eprintln!(
                    "cluster {} at {:?} size {:?} pinned={}",
                    c.name,
                    c.position(),
                    c.size_px(),
                    c.pinned
                );
            }
        }

        // Pane headers show each session's phase.
        if phase_changed {
            if let Some(stage) = &self.stage {
                stage.invalidate();
            }
        }
        self.sync_stage();

        let (total, waiting) = self
            .shared
            .registry
            .lock()
            .map(|r| (r.len(), r.waiting().len()))
            .unwrap_or((0, 0));
        let app = if glance_hooks::dev() {
            "Glance dev"
        } else {
            "Glance"
        };
        let tip = match (total, waiting) {
            _ if self.reload.is_some() => format!("{app}: reloads when no session is working"),
            (0, _) => format!("{app}: no sessions"),
            (t, 0) => format!("{app}: {t} session{}", if t == 1 { "" } else { "s" }),
            (t, w) => format!("{app}: {t} sessions, {w} waiting"),
        };
        self.tray.set_tip(&tip);
    }

    /// Clicks and drags collected by the window procedures.
    fn apply_input(&mut self) {
        let inputs = INPUT.with(|q| std::mem::take(&mut *q.borrow_mut()));
        let mut relayout = false;
        for input in inputs {
            match input {
                Input::Toggle(hwnd) => {
                    if let Some(c) = self.clusters.iter_mut().find(|c| c.hwnd.0 as isize == hwnd) {
                        c.collapsed = !c.collapsed;
                        c.fit();
                        relayout = true;
                    }
                }
                Input::Pin(hwnd) => {
                    if let Some(c) = self.clusters.iter_mut().find(|c| c.hwnd.0 as isize == hwnd) {
                        c.pinned = true;
                        relayout = true;
                    }
                }
                Input::Expand(id) => self.reveal(&id, true),
                Input::TileMenu(id) => {
                    self.menu_for = Some(id);
                    post(self.notify.0 as isize, WM_GLANCE_TILE_MENU, 0);
                }
                Input::ProjectMenu(key) => {
                    self.project_menu_for = Some(key);
                    post(self.notify.0 as isize, WM_GLANCE_PROJECT_MENU, 0);
                }
                Input::New(key) => {
                    // Projects tend to sit side by side, so the picker opens
                    // in the folder that holds this one.
                    self.pick_from = self
                        .project_dir(&key)
                        .map(|d| d.parent().map(Path::to_path_buf).unwrap_or(d));
                    post(self.notify.0 as isize, WM_GLANCE_PICK, 0);
                }
                Input::Add(key) => {
                    if let Some(dir) = self.project_dir(&key) {
                        if let Err(e) = self.start(None, dir, Vec::new()) {
                            eprintln!("glance: cannot start session: {e}");
                        }
                    }
                }
                Input::Shell(key) => {
                    let key = key.or_else(|| self.stage.as_ref().map(|s| s.project()));
                    if let Some(key) = key {
                        self.open_shell(&key);
                    }
                }
                Input::Close(hwnd) => {
                    if self
                        .stage
                        .as_ref()
                        .is_some_and(|s| s.hwnd.0 as isize == hwnd)
                    {
                        self.close_stage();
                    }
                }
                Input::Swap(a, b) => self.swap(&a, &b),
                Input::Browser(id) => self.show_browsers(&id),
                Input::View(key, dir, rel) => self.open_view(&key, &dir, &rel),
                Input::CloseView(serial) => self.close_view(serial),
                Input::FilesChanged(key) => self.files_changed(&key),
                Input::Arrange => relayout = true,
                Input::SettingMenu(s) => {
                    self.setting_menu_for = Some(s);
                    post(self.notify.0 as isize, WM_GLANCE_SETTING_MENU, 0);
                }
            }
        }
        if relayout {
            self.arrange();
        }
    }

    /// The folder the project with this key lives in, as a session in it
    /// spelled it.
    fn project_dir(&self, key: &str) -> Option<PathBuf> {
        let r = self.shared.registry.lock().ok()?;
        let dir = r
            .all()
            .find(|s| project_key(s) == key && !s.cwd.is_empty())
            .map(|s| PathBuf::from(&s.cwd));
        dir
    }

    /// Unpins every cluster, so they all go back down the left edge.
    fn raise(&mut self) {
        if let Some(u) = &self.usage_window {
            u.raise();
        }
        for c in &self.clusters {
            c.raise();
        }
    }

    fn tidy(&mut self) {
        if let Some(u) = &self.usage_window {
            u.pinned.set(false);
        }
        for c in &mut self.clusters {
            c.pinned = false;
        }
        self.arrange();
    }

    /// Stacks the unpinned windows down the left edge of the work area, the
    /// usage window first: it is about every project, so above them all.
    fn arrange(&self) {
        let usage = self.usage_window.as_deref().filter(|u| !u.pinned.get());
        let free: Vec<&Cluster> = self
            .clusters
            .iter()
            .map(|c| c.as_ref())
            .filter(|c| !c.pinned)
            .collect();
        let Some(dpi) = usage
            .map(UsageWindow::dpi)
            .or_else(|| free.first().map(|c| c.dpi()))
        else {
            return;
        };
        let work = work_area();
        let scale = dpi as f32 / 96.0;
        let width = (self.shared.metrics.width * scale).round() as i32;
        let heights: Vec<i32> = usage
            .map(|u| u.size_px().1)
            .into_iter()
            .chain(free.iter().map(|c| c.size_px().1))
            .collect();
        let mut positions = layout::stack(
            &heights,
            width,
            (MARGIN_DIP as f32 * scale) as i32,
            (GAP_DIP as f32 * scale) as i32,
            work,
        )
        .into_iter();
        if let Some(u) = usage {
            if let Some((x, y)) = positions.next() {
                if u.position() != (x, y) {
                    u.move_to(x, y);
                    u.invalidate();
                }
            }
        }
        for (c, (x, y)) in free.iter().zip(positions) {
            if c.position() != (x, y) {
                c.move_to(x, y);
                // A window that was created off screen has never painted.
                // Moving it into view does not always ask it to.
                c.invalidate();
            }
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn hwnd_of(id: isize) -> HWND {
    HWND(id as *mut c_void)
}

/// Whether a window's top left corner, nudged inside, lands on a monitor.
fn on_screen(x: i32, y: i32) -> bool {
    let p = POINT {
        x: x + 20,
        y: y + 10,
    };
    !unsafe { MonitorFromPoint(p, MONITOR_DEFAULTTONULL) }.is_invalid()
}

fn folder_name(cwd: &Path) -> String {
    cwd.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "session".into())
}

/// Primary monitor work area as (left, top, right, bottom).
pub(crate) fn work_area() -> (i32, i32, i32, i32) {
    let mut r = RECT::default();
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut r as *mut RECT as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    if r.right <= r.left {
        return (0, 0, 1920, 1080);
    }
    (r.left, r.top, r.right, r.bottom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ending_asks_only_when_something_runs() {
        assert_eq!(end_question(Some("app"), 0, 0), None);
        assert_eq!(
            end_question(Some("app"), 1, 1).as_deref(),
            Some("End the running session in app? It is in the middle of a turn.")
        );
        assert_eq!(
            end_question(None, 4, 0).as_deref(),
            Some("End all 4 running sessions?")
        );
        assert_eq!(
            end_question(Some("app"), 4, 1).as_deref(),
            Some("End all 4 running sessions in app? One of them is in the middle of a turn.")
        );
        assert_eq!(
            end_question(Some("app"), 3, 2).as_deref(),
            Some("End all 3 running sessions in app? 2 of them are in the middle of a turn.")
        );
    }

    #[test]
    fn quitting_says_what_stops_and_what_comes_back() {
        assert_eq!(quit_question(0, 0), None);
        assert_eq!(
            quit_question(2, 0).as_deref(),
            Some(
                "Quit Glance?\n\n2 running sessions will stop. They come back as paused tiles \
                 the next time Glance starts, and resume where they left off."
            )
        );
        assert_eq!(
            quit_question(0, 1).as_deref(),
            Some("Quit Glance?\n\nThe open terminal will close, and whatever runs in it.")
        );
        assert_eq!(
            quit_question(1, 3).as_deref(),
            Some(
                "Quit Glance?\n\nThe running session will stop. It comes back as a paused tile \
                 the next time Glance starts, and resumes where it left off. 3 open terminals \
                 will close, and whatever runs in them."
            )
        );
    }
}
