//! A session Glance owns: the agent in a pseudo console, and the terminal
//! state its output builds.
//!
//! A console outlives its window. Collapsing a terminal destroys the window
//! and the renderer, and the reader thread keeps parsing output into the
//! grid, so expanding it again shows the screen as it is now.

use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;
use glance_hooks::SESSION_ENV;
use glance_pty::{find_program, Command, Pty};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::app::{WM_GLANCE_EXIT, WM_GLANCE_OUTPUT};
use crate::{clipboard, palette};

/// History per session. Rows are allocated as output scrolls into them, at
/// about 24 bytes a cell, so 2000 rows of 120 columns is under 6 MB even when
/// full. Forty full sessions would be 230 MB, which is the number to watch.
const SCROLLBACK: usize = 2000;

/// Set by Claude Code for the processes it starts, naming that session.
/// When Glance itself was started from inside Claude Code they leak into
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
    pty: Arc<Pty>,
    pub screen: Mutex<Screen>,
    size: Mutex<GridSize>,
    /// Set by the reader when output arrived and no wake message is queued
    /// yet. Keeps a chatty session from flooding the message queue.
    dirty: AtomicBool,
    exit: Mutex<Option<u32>>,
    title: Arc<Mutex<Option<String>>>,
    /// Where the window was when it last collapsed, as left, top, right,
    /// bottom in physical pixels. Expanding again puts it back there.
    pub placement: Mutex<Option<(i32, i32, i32, i32)>>,
}

/// What to start. `program` is the agent, normally `claude.exe`.
pub struct Launch {
    pub id: String,
    pub serial: usize,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

/// The agent binary: `GLANCE_AGENT` when set, which is also how a plain
/// shell gets into a tile for testing, otherwise `claude.exe` from `PATH`.
pub fn agent_program() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let name = std::env::var("GLANCE_AGENT").unwrap_or_else(|_| "claude".into());
    if Path::new(&name).is_absolute() {
        return Some(PathBuf::from(name));
    }
    let bare = name.trim_end_matches(".exe");
    find_program(bare, &path, &[".exe"], Path::is_file)
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
            args: launch.args,
            cwd: launch.cwd,
            env_set: vec![
                (SESSION_ENV.into(), launch.id.clone()),
                ("COLORTERM".into(), "truecolor".into()),
            ],
            env_remove: PARENT_SESSION_ENV.to_vec(),
            cols: size.cols,
            rows: size.rows,
        };
        let (pty, output) = Pty::spawn(&cmd)?;
        let pty = Arc::new(pty);

        let title = Arc::new(Mutex::new(None));
        let events = Events {
            pty: Arc::clone(&pty),
            title: Arc::clone(&title),
        };
        let config = Config {
            scrolling_history: SCROLLBACK,
            ..Config::default()
        };
        let console = Arc::new(Console {
            id: launch.id,
            serial: launch.serial,
            pty,
            screen: Mutex::new(Screen {
                term: Term::new(config, &size, events),
                parser: Processor::new(),
            }),
            size: Mutex::new(size),
            dirty: AtomicBool::new(false),
            exit: Mutex::new(None),
            title,
            placement: Mutex::new(None),
        });

        let notify = notify.0 as isize;
        let reader = Arc::clone(&console);
        thread::spawn(move || reader.read(output, notify));
        let waiter = Arc::clone(&console);
        thread::spawn(move || {
            let code = waiter.pty.wait();
            if let Ok(mut e) = waiter.exit.lock() {
                *e = Some(code);
            }
            post(notify, WM_GLANCE_EXIT, waiter.serial);
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
                post(notify, WM_GLANCE_OUTPUT, self.serial);
            }
        }
    }

    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        self.pty.write(bytes);
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
        if let Ok(mut s) = self.screen.lock() {
            s.term.resize(size);
        }
        let _ = self.pty.resize(size.cols, size.rows);
    }

    pub fn exit_code(&self) -> Option<u32> {
        self.exit.lock().ok().and_then(|e| *e)
    }

    /// The title the program set with OSC 0 or 2, if any.
    pub fn title(&self) -> Option<String> {
        self.title.lock().ok().and_then(|t| t.clone())
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
    pty: Arc<Pty>,
    title: Arc<Mutex<Option<String>>>,
}

impl EventListener for Events {
    fn send_event(&self, event: Event) {
        match event {
            // Answers to device status and attribute queries.
            Event::PtyWrite(text) => self.pty.write(text.into_bytes()),
            // Programs ask for the background to pick a light or dark theme.
            Event::ColorRequest(index, format) => {
                self.pty
                    .write(format(palette::default_rgb(index)).into_bytes());
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
