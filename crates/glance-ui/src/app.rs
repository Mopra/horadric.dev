//! The UI thread: owns the cluster windows and the terminal windows, the
//! consoles behind them, and keeps the set of windows in sync with the set
//! of sessions.
//!
//! Everything that happens off the UI thread arrives as a message to one
//! hidden message-only window: hook events from the feeder thread, new
//! session requests from the listener, output and exit from the consoles,
//! and clicks queued by the window procedures. A window, not the thread,
//! because thread messages are dropped while Windows runs a modal loop, and
//! resizing a terminal by its edge is a modal loop. A one second timer on
//! the same window moves the age lines.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use glance_core::{session_id, HookEvent, Registry};
use glance_hooks::listener::{self, NewSession, Tagged};
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostMessageW, RegisterClassW,
    SetTimer, SystemParametersInfoW, TranslateMessage, HWND_MESSAGE, MSG, SPI_GETWORKAREA,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_QUIT, WM_TIMER,
    WNDCLASSW,
};

use crate::console::{self, Console, Launch};
use crate::glyphs::Font;
use crate::layout::{self, Metrics};
use crate::render::Gpu;
use crate::terminal::{self, TerminalWindow};
use crate::window::{self, project_key, project_name, Cluster, Shared};

/// A hook event changed the registry. `wparam` is 1 when a phase changed.
const WM_GLANCE_EVENT: u32 = WM_APP + 1;
/// A window procedure queued input with [`push`].
const WM_GLANCE_INPUT: u32 = WM_APP + 2;
/// A console has new output. `wparam` is its serial.
pub(crate) const WM_GLANCE_OUTPUT: u32 = WM_APP + 3;
/// A console's process exited. `wparam` is its serial.
pub(crate) const WM_GLANCE_EXIT: u32 = WM_APP + 4;
/// `glance new` asked for a session.
const WM_GLANCE_NEW: u32 = WM_APP + 5;

const APP_CLASS: PCWSTR = w!("GlanceApp");
const ENDED_LINGER: Duration = Duration::from_secs(20);
const MARGIN_DIP: i32 = 12;
const GAP_DIP: i32 = 12;

/// Something a window procedure saw that the app has to act on. Window
/// procedures only get `&self`, and the app owns the windows, so they queue
/// it here and the app applies it right after.
pub(crate) enum Input {
    /// Header clicked: collapse or expand the cluster with this window.
    Toggle(isize),
    /// Cluster dragged: auto layout leaves it alone from now on.
    Pin(isize),
    /// Tile clicked: show this session's terminal, or collapse it.
    Expand(String),
    /// Plus clicked: start a session in the project with this key.
    New(String),
    /// Terminal closed: collapse the console with this serial.
    Collapse(usize),
}

thread_local! {
    static INPUT: RefCell<Vec<Input>> = const { RefCell::new(Vec::new()) };
    static APP_WINDOW: Cell<isize> = const { Cell::new(0) };
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

/// Runs the whole desktop app on the calling thread until quit.
pub fn run(port: u16) -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    window::register_class()?;
    terminal::register_class()?;
    let notify = create_app_window()?;
    let notify_id = notify.0 as isize;
    APP_WINDOW.with(|w| w.set(notify_id));

    let registry = Arc::new(Mutex::new(Registry::new()));
    let requests: Arc<Mutex<Vec<NewSession>>> = Arc::new(Mutex::new(Vec::new()));

    // Listener thread: sockets in, tagged events and new session requests out.
    let (tx, rx) = mpsc::channel::<Tagged>();
    let (new_tx, new_rx) = mpsc::channel::<NewSession>();
    thread::spawn(move || {
        if let Err(e) = listener::serve(port, tx, Some(new_tx)) {
            eprintln!("glance: listener stopped: {e}");
        }
    });

    // Feeder thread: apply to the registry, wake the UI.
    let feed_registry = Arc::clone(&registry);
    thread::spawn(move || {
        for t in rx {
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

    let gpu = Gpu::new()?;
    let font = Font::new(&gpu.dw)?;
    let shared = Rc::new(Shared {
        gpu,
        font,
        metrics: Metrics::default(),
        registry,
    });
    APP.with(|a| {
        *a.borrow_mut() = Some(App {
            shared,
            clusters: Vec::new(),
            consoles: HashMap::new(),
            terminals: Vec::new(),
            next_serial: 1,
            requests,
            notify,
        })
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
        if let Some(app) = a.borrow_mut().take() {
            for t in &app.terminals {
                t.destroy();
            }
            for c in &app.clusters {
                c.destroy();
            }
        }
    });
    Ok(())
}

fn create_app_window() -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(app_proc),
            hInstance: instance.into(),
            lpszClassName: APP_CLASS,
            ..Default::default()
        };
        RegisterClassW(&wc);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            APP_CLASS,
            w!("Glance"),
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
    }
}

unsafe extern "system" fn app_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ours = matches!(
        msg,
        WM_GLANCE_EVENT
            | WM_GLANCE_INPUT
            | WM_GLANCE_OUTPUT
            | WM_GLANCE_EXIT
            | WM_GLANCE_NEW
            | WM_TIMER
    );
    if !ours {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The app is busy when a call it made pumped messages. Try again later
    // rather than touch it twice. A missed timer tick is not worth repeating.
    let handled = APP.with(|a| match a.try_borrow_mut() {
        Ok(mut app) => {
            if let Some(app) = app.as_mut() {
                app.on_message(msg, wparam.0);
            }
            true
        }
        Err(_) => false,
    });
    if !handled && msg != WM_TIMER {
        let _ = PostMessageW(Some(hwnd), msg, wparam, lparam);
    }
    LRESULT(0)
}

struct App {
    shared: Rc<Shared>,
    // Boxed on purpose: the window procedures hold a raw pointer to each
    // window struct, so it must not move when the Vec grows.
    #[allow(clippy::vec_box)]
    clusters: Vec<Box<Cluster>>,
    /// Sessions Glance started itself, by session id. A `glance run`
    /// session has a tile but no console: its terminal is somewhere else.
    consoles: HashMap<String, Arc<Console>>,
    #[allow(clippy::vec_box)]
    terminals: Vec<Box<TerminalWindow>>,
    next_serial: usize,
    requests: Arc<Mutex<Vec<NewSession>>>,
    notify: HWND,
}

impl App {
    fn on_message(&mut self, msg: u32, wparam: usize) {
        match msg {
            WM_GLANCE_EVENT => self.reconcile(wparam != 0),
            WM_GLANCE_INPUT => self.apply_input(),
            WM_GLANCE_OUTPUT => self.output(wparam),
            WM_GLANCE_EXIT => self.exited(wparam),
            WM_GLANCE_NEW => {
                let pending = self
                    .requests
                    .lock()
                    .map(|mut q| std::mem::take(&mut *q))
                    .unwrap_or_default();
                for n in pending {
                    if let Err(e) = self.start(n.name, PathBuf::from(n.cwd), n.args) {
                        eprintln!("glance: cannot start session: {e}");
                    }
                }
            }
            WM_TIMER => self.tick(),
            _ => {}
        }
    }

    fn console_by_serial(&self, serial: usize) -> Option<&Arc<Console>> {
        self.consoles.values().find(|c| c.serial == serial)
    }

    fn terminal_by_serial(&self, serial: usize) -> Option<&TerminalWindow> {
        self.terminals
            .iter()
            .map(|t| t.as_ref())
            .find(|t| t.console.serial == serial)
    }

    fn output(&mut self, serial: usize) {
        let Some(console) = self.console_by_serial(serial) else {
            return;
        };
        if console.take_dirty() {
            if let Some(t) = self.terminal_by_serial(serial) {
                t.invalidate();
                t.refresh_title();
            }
        }
    }

    /// The agent exited. Claude Code says so with `SessionEnd` when it can;
    /// this covers a crash or a kill, where it could not.
    fn exited(&mut self, serial: usize) {
        let Some(console) = self.console_by_serial(serial).cloned() else {
            return;
        };
        let changed = self
            .shared
            .registry
            .lock()
            .map(|mut r| {
                r.get(&console.id).is_some()
                    && r.apply(
                        &console.id,
                        &HookEvent::synthetic("SessionEnd"),
                        SystemTime::now(),
                    )
            })
            .unwrap_or(false);
        // A clean exit closes the window, as `/exit` should. A failure keeps
        // it open so the error can be read.
        if console.exit_code() == Some(0) {
            self.collapse(serial);
        } else if let Some(t) = self.terminal_by_serial(serial) {
            t.refresh_title();
            t.invalidate();
        }
        self.reconcile(changed);
    }

    /// Starts an agent in a console of its own and opens its terminal.
    fn start(
        &mut self,
        name: Option<String>,
        cwd: PathBuf,
        args: Vec<String>,
    ) -> std::result::Result<(), String> {
        let program =
            console::agent_program().ok_or("claude.exe not found on PATH (or set GLANCE_AGENT)")?;
        if !cwd.is_dir() {
            return Err(format!("{} is not a directory", cwd.display()));
        }
        let folder = folder_name(&cwd);
        let base = name.clone().unwrap_or_else(|| folder.clone());
        let id = self.unique_id(&base);
        let shown = name.unwrap_or(folder);

        let register = HookEvent {
            cwd: cwd.to_string_lossy().to_string(),
            name: Some(shown),
            ..HookEvent::synthetic(HookEvent::REGISTER)
        };
        if let Ok(mut r) = self.shared.registry.lock() {
            r.apply(&id, &register, SystemTime::now());
        }

        let serial = self.next_serial;
        self.next_serial += 1;
        let console = Console::spawn(
            Launch {
                id: id.clone(),
                serial,
                program,
                args,
                cwd,
            },
            self.notify,
        )
        .map_err(|e| {
            if let Ok(mut r) = self.shared.registry.lock() {
                r.remove(&id);
            }
            format!("could not start the agent: {e}")
        })?;
        self.consoles.insert(id.clone(), console);
        self.reconcile(true);
        self.expand(&id);
        Ok(())
    }

    fn unique_id(&self, base: &str) -> String {
        let id = session_id(base, SystemTime::now());
        let taken = |candidate: &str| {
            self.consoles.contains_key(candidate)
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

    /// Shows a session's terminal. Collapses it when it is already in front.
    fn expand(&mut self, id: &str) {
        let Some(console) = self.consoles.get(id).cloned() else {
            // Started with `glance run`: its terminal is the one it was run in.
            return;
        };
        if let Some(t) = self.terminal_by_serial(console.serial) {
            if t.is_foreground() {
                self.collapse(console.serial);
            } else {
                t.bring_to_front();
            }
            return;
        }
        let label = self
            .shared
            .registry
            .lock()
            .ok()
            .and_then(|r| {
                r.get(id)
                    .map(|s| format!("{}, {}", s.name, project_name(&project_key(s))))
            })
            .unwrap_or_else(|| id.to_string());
        match TerminalWindow::open(Rc::clone(&self.shared), console, label) {
            Ok(t) => self.terminals.push(t),
            Err(e) => eprintln!("glance: cannot open terminal: {e}"),
        }
    }

    fn collapse(&mut self, serial: usize) {
        if let Some(i) = self
            .terminals
            .iter()
            .position(|t| t.console.serial == serial)
        {
            let t = self.terminals.remove(i);
            t.destroy();
        }
    }

    /// Once a second: drop long ended sessions and their consoles, redraw ages.
    fn tick(&mut self) {
        let pruned = self
            .shared
            .registry
            .lock()
            .map(|mut r| r.prune_ended(ENDED_LINGER, SystemTime::now()))
            .unwrap_or(0);
        if pruned > 0 {
            // A console goes with its tile once its process is gone. One
            // still running keeps going: it will be adopted again by its
            // next hook.
            let gone: Vec<(String, usize)> = self
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
                .map(|(id, c)| (id.clone(), c.serial))
                .collect();
            for (id, serial) in gone {
                self.collapse(serial);
                self.consoles.remove(&id);
            }
            self.reconcile(true);
        } else {
            for c in &self.clusters {
                c.invalidate();
            }
        }
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

        // Add clusters for new projects, off screen until laid out.
        for key in projects.keys() {
            if !self.clusters.iter().any(|c| &c.key == key) {
                match Cluster::create(
                    Rc::clone(&self.shared),
                    key.clone(),
                    project_name(key),
                    -10_000,
                    -10_000,
                ) {
                    Ok(c) => self.clusters.push(c),
                    Err(e) => eprintln!("glance: cannot create window: {e}"),
                }
            }
        }

        for c in &self.clusters {
            c.fit();
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

        if phase_changed {
            for c in &self.clusters {
                c.raise();
            }
        }
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
                Input::Expand(id) => self.expand(&id),
                Input::New(key) => {
                    let cwd = self.shared.registry.lock().ok().and_then(|r| {
                        r.all()
                            .find(|s| project_key(s) == key && !s.cwd.is_empty())
                            .map(|s| PathBuf::from(&s.cwd))
                    });
                    if let Some(cwd) = cwd {
                        if let Err(e) = self.start(None, cwd, Vec::new()) {
                            eprintln!("glance: cannot start session: {e}");
                        }
                    }
                }
                Input::Collapse(serial) => self.collapse(serial),
            }
        }
        if relayout {
            self.arrange();
        }
    }

    /// Stacks the unpinned clusters down the right edge of the work area.
    fn arrange(&self) {
        let free: Vec<&Cluster> = self
            .clusters
            .iter()
            .map(|c| c.as_ref())
            .filter(|c| !c.pinned)
            .collect();
        if free.is_empty() {
            return;
        }
        let work = work_area();
        let scale = free[0].dpi() as f32 / 96.0;
        let width = (self.shared.metrics.width * scale).round() as i32;
        let heights: Vec<i32> = free.iter().map(|c| c.size_px().1).collect();
        let positions = layout::stack(
            &heights,
            width,
            (MARGIN_DIP as f32 * scale) as i32,
            (GAP_DIP as f32 * scale) as i32,
            work,
        );
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
