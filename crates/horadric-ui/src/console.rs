//! A session Horadric owns: the agent in a pseudo console, and the terminal
//! state its output builds.
//!
//! A console outlives its window. Collapsing a terminal destroys the window
//! and the renderer, and the reader thread keeps parsing output into the
//! grid, so expanding it again shows the screen as it is now.
//!
//! A console can also have no program at all and show a file read only
//! ([`Console::view`]). The pane draws it like any other grid, which is how
//! it gets selection, scrolling and fallback fonts for nothing.

use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::Point;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;
use horadric_hooks::{OWNER_ENV, SESSION_ENV};
use horadric_pty::{find_program, Command, Pty};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::app::{WM_HORADRIC_EXIT, WM_HORADRIC_OUTPUT};
use crate::viewer::{self, Cell, Row, Span};
use crate::{clipboard, highlight, palette, shell};

/// History per session. Rows are allocated as output scrolls into them, at
/// about 24 bytes a cell, so 2000 rows of 120 columns is under 6 MB even when
/// full. Forty full sessions would be 230 MB, which is the number to watch.
const SCROLLBACK: usize = 2000;

/// Set by Claude Code for the processes it starts, naming that session.
/// When Horadric itself was started from inside Claude Code they leak into
/// every tile, and the agent there believes it is someone's child: it stops
/// saving its transcript, among other things. User preferences such as
/// `CLAUDE_EFFORT` are left alone.
const PARENT_SESSION_ENV: [&str; 11] = [
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_AGENT_SDK_VERSION",
    "CLAUDE_PID",
    "AI_AGENT",
];

pub const DEFAULT_COLS: u16 = 120;
pub const DEFAULT_ROWS: u16 = 36;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }

    fn screen_lines(&self) -> usize {
        self.rows as usize
    }

    fn columns(&self) -> usize {
        self.cols as usize
    }
}

/// The terminal and the parser that feeds it. One lock for both, because a
/// synchronized update that times out has to be flushed through the parser
/// into the terminal in one step.
pub struct Screen {
    pub term: Term<Events>,
    parser: Processor,
}

pub struct Console {
    pub id: String,
    /// Small number for window messages, which carry integers, not strings.
    pub serial: usize,
    /// None for a file view, which has no program.
    pty: Option<Arc<Pty>>,
    view: Option<Mutex<View>>,
    pub screen: Mutex<Screen>,
    size: Mutex<GridSize>,
    /// Set by the reader when output arrived and no wake message is queued
    /// yet. Keeps a chatty session from flooding the message queue.
    dirty: AtomicBool,
    exit: Mutex<Option<u32>>,
    title: Arc<Mutex<Option<String>>>,
    /// What the agent was started with, kept so a restart can start it the
    /// same way.
    pub args: Vec<String>,
    /// A plain shell, not an agent.
    pub shell: bool,
}

/// The file a view shows, and how it is laid out in the grid now.
struct View {
    path: PathBuf,
    /// Size and time of the read shown, to tell whether the file changed.
    stamp: Option<(u64, SystemTime)>,
    lines: Arc<Vec<String>>,
    spans: Vec<Vec<Span>>,
    rows: Vec<Row>,
    gutter: usize,
    /// Counts reads, so colours for an older one are not applied to a newer.
    read: u64,
}

/// What to start. `program` is the agent, normally `claude.exe`.
pub struct Launch {
    pub id: String,
    pub serial: usize,
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Put before `args` for this start only and never saved with the
    /// session: Horadric's defaults and its status line, which a resume
    /// should take as they are then, not as they were.
    pub extra: Vec<String>,
    pub cwd: PathBuf,
    /// A plain shell. It is not tagged as a session, so a `claude` typed
    /// into it is nobody's and stays off the tiles, as outside Horadric.
    pub shell: bool,
}

/// The agent binary: `HORADRIC_AGENT` when set, which is also how a plain
/// shell gets into a tile for testing, otherwise `claude.exe` from `PATH`.
pub fn agent_program() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let name = std::env::var("HORADRIC_AGENT").unwrap_or_else(|_| "claude".into());
    if Path::new(&name).is_absolute() {
        return Some(PathBuf::from(name));
    }
    let bare = name.trim_end_matches(".exe");
    find_program(bare, &path, &[".exe"], Path::is_file)
}

/// The shell a plain terminal runs, see [`shell::program`].
pub fn shell_program() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let chosen = std::env::var("HORADRIC_SHELL").ok();
    let comspec = std::env::var("COMSPEC").ok();
    shell::program(chosen.as_deref(), comspec.as_deref(), |name| {
        find_program(name, &path, &[".exe"], Path::is_file)
    })
}

impl Console {
    /// Starts the agent and the threads that read its output and wait for it.
    /// `notify` is the window that hears about output and exit.
    pub fn spawn(launch: Launch, notify: HWND) -> io::Result<Arc<Console>> {
        let size = GridSize {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
        };
        let cmd = Command {
            program: launch.program,
            args: launch.extra.iter().chain(&launch.args).cloned().collect(),
            cwd: launch.cwd,
            env_set: if launch.shell {
                vec![("COLORTERM".into(), "truecolor".into())]
            } else {
                vec![
                    (SESSION_ENV.into(), launch.id.clone()),
                    (OWNER_ENV.into(), horadric_hooks::port().to_string()),
                    ("COLORTERM".into(), "truecolor".into()),
                ]
            },
            // A Horadric started from inside a session would hand its own tag
            // down, and a `claude` in the shell would report as that session.
            env_remove: if launch.shell {
                [&PARENT_SESSION_ENV[..], &[SESSION_ENV, OWNER_ENV]].concat()
            } else {
                PARENT_SESSION_ENV.to_vec()
            },
            cols: size.cols,
            rows: size.rows,
        };
        let (pty, output) = Pty::spawn(&cmd)?;
        let pty = Arc::new(pty);

        let title = Arc::new(Mutex::new(None));
        let events = Events {
            pty: Some(Arc::clone(&pty)),
            title: Arc::clone(&title),
        };
        let config = Config {
            scrolling_history: SCROLLBACK,
            ..Config::default()
        };
        let console = Arc::new(Console {
            id: launch.id,
            serial: launch.serial,
            pty: Some(Arc::clone(&pty)),
            view: None,
            screen: Mutex::new(Screen {
                term: Term::new(config, &size, events),
                parser: Processor::new(),
            }),
            size: Mutex::new(size),
            dirty: AtomicBool::new(false),
            exit: Mutex::new(None),
            title,
            args: launch.args,
            shell: launch.shell,
        });

        let notify = notify.0 as isize;
        let reader = Arc::clone(&console);
        thread::spawn(move || reader.read(output, notify));
        let waiter = Arc::clone(&console);
        thread::spawn(move || {
            let code = pty.wait();
            if let Ok(mut e) = waiter.exit.lock() {
                *e = Some(code);
            }
            post(notify, WM_HORADRIC_EXIT, waiter.serial);
        });
        Ok(console)
    }

    fn read(&self, mut output: File, notify: isize) {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match output.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if let Ok(mut s) = self.screen.lock() {
                let Screen { term, parser } = &mut *s;
                parser.advance(term, &buf[..n]);
            }
            if !self.dirty.swap(true, Ordering::AcqRel) {
                post(notify, WM_HORADRIC_OUTPUT, self.serial);
            }
        }
    }

    /// A console with no program, showing the file at `path` read only.
    /// `detail` is what its header says after the name. The colours arrive
    /// a moment later from a thread, announced like output to `notify`.
    pub fn view(
        id: String,
        serial: usize,
        path: PathBuf,
        detail: String,
        notify: HWND,
    ) -> Arc<Console> {
        let size = GridSize {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
        };
        let title = Arc::new(Mutex::new(Some(detail)));
        let events = Events {
            pty: None,
            title: Arc::clone(&title),
        };
        let console = Arc::new(Console {
            id,
            serial,
            pty: None,
            view: Some(Mutex::new(View {
                path,
                stamp: None,
                lines: Arc::new(Vec::new()),
                spans: Vec::new(),
                rows: Vec::new(),
                gutter: 0,
                read: 0,
            })),
            screen: Mutex::new(Screen {
                term: Term::new(Config::default(), &size, events),
                parser: Processor::new(),
            }),
            size: Mutex::new(size),
            dirty: AtomicBool::new(false),
            exit: Mutex::new(None),
            title,
            args: Vec::new(),
            shell: false,
        });
        console.load(notify.0 as isize);
        console
    }

    pub fn is_view(&self) -> bool {
        self.view.is_some()
    }

    /// The file a view shows.
    pub fn path(&self) -> Option<PathBuf> {
        Some(self.view.as_ref()?.lock().ok()?.path.clone())
    }

    /// Reads the file again when it changed since it was shown, keeping the
    /// line at the top where it is. True when it did.
    pub fn reload(self: &Arc<Self>, notify: HWND) -> bool {
        let Some(view) = &self.view else {
            return false;
        };
        let (path, shown) = match view.lock() {
            Ok(v) => (v.path.clone(), v.stamp),
            Err(_) => return false,
        };
        if stamp(&path) == shown {
            return false;
        }
        self.load(notify.0 as isize);
        true
    }

    /// Reads the file and shows it plain, then colours it on a thread.
    fn load(self: &Arc<Self>, notify: isize) {
        let Some(view) = &self.view else { return };
        let (path, read) = {
            let Ok(mut v) = view.lock() else { return };
            v.read += 1;
            (v.path.clone(), v.read)
        };
        let now = stamp(&path);
        let lines = Arc::new(read_lines(&path));
        if let Ok(mut v) = view.lock() {
            v.stamp = now;
            v.lines = Arc::clone(&lines);
            v.spans = Vec::new();
        }
        self.lay_out();
        if now.is_none() {
            // Nothing was read: the line saying so needs no colours.
            return;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let me = Arc::clone(self);
        thread::spawn(move || {
            let spans = highlight::highlight(&name, &lines);
            if spans.is_empty() {
                return;
            }
            let Some(view) = &me.view else { return };
            match view.lock() {
                Ok(mut v) if v.read == read => v.spans = spans,
                _ => return,
            }
            me.lay_out();
            if !me.dirty.swap(true, Ordering::AcqRel) {
                post(notify, WM_HORADRIC_OUTPUT, me.serial);
            }
        });
    }

    /// Lays the file out again for the grid's width, into a fresh terminal
    /// with room for every row, scrolled so the same line stays on top.
    fn lay_out(&self) {
        let Some(view) = &self.view else { return };
        let Ok(mut v) = view.lock() else { return };
        let Ok(mut s) = self.screen.lock() else {
            return;
        };
        let top = {
            let grid = s.term.grid();
            let row = grid.history_size().saturating_sub(grid.display_offset());
            v.rows.get(row).map_or(0, |r| r.line)
        };
        let size = self.size();
        let r = viewer::render(&v.lines, &v.spans, size.cols as usize);
        let events = Events {
            pty: None,
            title: Arc::clone(&self.title),
        };
        let config = Config {
            scrolling_history: r.rows.len(),
            ..Config::default()
        };
        let mut term = Term::new(config, &size, events);
        term.is_focused = s.term.is_focused;
        let mut parser = Processor::new();
        parser.advance(&mut term, &r.bytes);
        let back = term
            .grid()
            .history_size()
            .saturating_sub(viewer::row_of(&r.rows, top));
        term.scroll_display(Scroll::Delta(back as i32));
        s.term = term;
        s.parser = parser;
        v.rows = r.rows;
        v.gutter = r.gutter;
    }

    /// The selected text. For a view, as it is in the file: no line numbers,
    /// and a wrapped line in one piece.
    pub fn selection_text(&self) -> Option<String> {
        let s = self.screen.lock().ok()?;
        let Some(view) = &self.view else {
            return s.term.selection_to_string();
        };
        let range = s.term.selection.as_ref()?.to_range(&s.term)?;
        let history = s.term.grid().history_size() as i32;
        let cell = |p: Point| Cell {
            row: (p.line.0 + history).max(0) as usize,
            col: p.column.0,
        };
        let v = view.lock().ok()?;
        Some(viewer::copy(
            &v.lines,
            &v.rows,
            v.gutter,
            cell(range.start),
            cell(range.end),
        ))
    }

    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        if let Some(pty) = &self.pty {
            pty.write(bytes);
        }
    }

    /// Clears the output flag. True when there was new output to show.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    pub fn size(&self) -> GridSize {
        self.size.lock().map(|s| *s).unwrap_or(GridSize {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
        })
    }

    /// Resizes grid and console together, so the program redraws for the
    /// size the window really has.
    pub fn resize(&self, size: GridSize) {
        if size.cols == 0 || size.rows == 0 {
            return;
        }
        if let Ok(mut current) = self.size.lock() {
            if *current == size {
                return;
            }
            *current = size;
        }
        match &self.pty {
            Some(pty) => {
                if let Ok(mut s) = self.screen.lock() {
                    s.term.resize(size);
                }
                let _ = pty.resize(size.cols, size.rows);
            }
            None => self.lay_out(),
        }
    }

    pub fn kill(&self) {
        if let Some(pty) = &self.pty {
            pty.kill();
        }
    }

    /// Whether a process is the agent or was started by it, however far
    /// down: a browser its tests opened, say.
    pub fn contains(&self, process: HANDLE) -> bool {
        self.pty.as_ref().is_some_and(|p| p.contains(process))
    }

    pub fn exit_code(&self) -> Option<u32> {
        self.exit.lock().ok().and_then(|e| *e)
    }

    /// The title the program set with OSC 0 or 2, if it says more than
    /// the name of the executable.
    pub fn title(&self) -> Option<String> {
        let raw = self.title.lock().ok()?.clone()?;
        shell::title(&raw)
    }

    /// Applies a synchronized update the program never finished, once its
    /// time is up. Returns how long to wait when one is still open.
    pub fn flush_sync(&self) -> Option<Duration> {
        let mut s = self.screen.lock().ok()?;
        let deadline = s.parser.sync_timeout().sync_timeout()?;
        let now = Instant::now();
        if now >= deadline {
            let Screen { term, parser } = &mut *s;
            parser.stop_sync(term);
            None
        } else {
            Some(deadline - now)
        }
    }
}

fn stamp(path: &Path) -> Option<(u64, SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// A file's lines for a view, or a line saying why there are none.
fn read_lines(path: &Path) -> Vec<String> {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > viewer::MAX_BYTES) {
        let mb = viewer::MAX_BYTES / 1024 / 1024;
        return vec![format!("Too big to show here, over {mb} MB.")];
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return vec![format!("Cannot read it: {e}")],
    };
    if viewer::is_binary(&bytes) {
        return vec![format!("A binary file, {} bytes.", bytes.len())];
    }
    let (mut lines, total) = viewer::lines(&String::from_utf8_lossy(&bytes));
    let rest = total - lines.len();
    if rest > 0 {
        lines.push(String::new());
        lines.push(format!("{rest} more lines not shown."));
    }
    lines
}

fn post(notify: isize, msg: u32, serial: usize) {
    unsafe {
        let _ = PostMessageW(
            Some(HWND(notify as *mut c_void)),
            msg,
            WPARAM(serial),
            LPARAM(0),
        );
    }
}

/// Requests the terminal makes of the outside world while parsing. Runs on
/// the reader thread with the screen locked, so nothing here may block.
pub struct Events {
    pty: Option<Arc<Pty>>,
    title: Arc<Mutex<Option<String>>>,
}

impl EventListener for Events {
    fn send_event(&self, event: Event) {
        match event {
            // Answers to device status and attribute queries.
            Event::PtyWrite(text) => {
                if let Some(pty) = &self.pty {
                    pty.write(text.into_bytes());
                }
            }
            // Programs ask for the background to pick a light or dark theme.
            Event::ColorRequest(index, format) => {
                if let Some(pty) = &self.pty {
                    pty.write(format(palette::default_rgb(index)).into_bytes());
                }
            }
            Event::Title(t) => {
                if let Ok(mut slot) = self.title.lock() {
                    *slot = Some(t);
                }
            }
            Event::ResetTitle => {
                if let Ok(mut slot) = self.title.lock() {
                    *slot = None;
                }
            }
            // OSC 52. Copy only: the default refuses programs reading it.
            Event::ClipboardStore(_, text) => clipboard::set_text(&text),
            _ => {}
        }
    }
}
