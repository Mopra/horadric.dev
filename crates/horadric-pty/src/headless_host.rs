//! A headless session in a host process of its own, so `claude.exe`
//! outlives the UI: a reload, a crash, or a Quit that keeps sessions.
//!
//! `horadric host --headless` reads a [`HeadlessSpec`] on stdin, starts
//! claude.exe on pipes with [`Headless`] (in a kill-on-close job the host
//! owns, so claude never outlives its host) and serves one named pipe in
//! the protocol of [`LineMessage`]. It keeps every line since the start,
//! both claude's stdout and what clients sent to its stdin, up to
//! [`LOG_CAP`]. A client that attaches gets that backlog, then
//! [`LineMessage::Live`], then the live tail, so a reload mid turn loses
//! nothing.
//!
//! One client at a time, the newest, as with terminal hosts. A client that
//! goes away detaches: claude runs on. [`LineMessage::Close`] ends the
//! session for good. Once claude ends the host tells its client the exit
//! and goes; with no client it waits [`LINGER`] for one, so a reload at
//! that moment still hears how it ended.
//!
//! [`Hosted`] is the client's side, shaped like [`Headless`], so a UI can
//! swap one for the other.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::headless::{control_request, prompt_line, Headless, Options};
use crate::host::launch;
use crate::pipe::Pipe;
use crate::wire::{
    headless_hello, headless_pipe_name, headless_pipe_prefix, read_headless_hello, too_long,
    LineDecoder, LineLog, LineMessage, Logged, MAX_LINE,
};

/// The lines a host keeps, in bytes. A long session's tool results run to
/// tens of MB; past this the oldest go, and the transcript on disk still
/// has them.
pub const LOG_CAP: usize = 32 * 1024 * 1024;

/// How long a host whose claude ended waits for its client to read it.
const LAST_WORDS: Duration = Duration::from_secs(5);

/// How long a host whose claude ended with no client waits for one.
pub const LINGER: Duration = Duration::from_secs(30);

/// How long claude gets to end on its own after a close, before its job
/// ends it. Ending on its own fires its SessionEnd hook.
const GRACE: Duration = Duration::from_secs(3);

/// How long stdout's end waits for claude to exit, to read its code.
const EXIT_WAIT: Duration = Duration::from_secs(10);

/// How long a client's close waits for its frame to be written.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

/// A client this many bytes behind is let go rather than queued for
/// without end. It attaches again and gets the backlog.
const BEHIND: usize = 128 * 1024 * 1024;

/// What a headless host is to run and where to serve it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadlessSpec {
    pub pipe: String,
    pub cwd: PathBuf,
    #[serde(default)]
    pub resume: Option<String>,
    #[serde(default)]
    pub options: Options,
    /// Handed back to every client that attaches, untouched, so a UI after
    /// a reload knows what the session was to it (a project key, say).
    #[serde(default)]
    pub tag: String,
}

/// What a host says about itself as a client attaches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    /// claude.exe's process id.
    pub pid: u32,
    pub cwd: PathBuf,
    pub tag: String,
    /// When the host started claude, in seconds since 1970.
    pub started: u64,
}

/// Runs a headless host until claude ends and its client heard. Returns
/// only when it could not start; the error is for stderr, where the client
/// that started it reads it.
pub fn serve_headless(spec: HeadlessSpec) -> io::Result<()> {
    let host = Host::start(&spec)?;
    let ended = host.ended;
    thread::spawn(move || {
        let _ = ended.recv();
        std::process::exit(0);
    });
    let mut next = Some(host.first);
    loop {
        let pipe = match next.take() {
            Some(p) => p,
            None => match Pipe::serve(&spec.pipe, false) {
                Ok(p) => p,
                Err(_) => {
                    thread::sleep(Duration::from_secs(1));
                    continue;
                }
            },
        };
        if pipe.accept().is_ok() {
            welcome(pipe, &host.shared);
        }
    }
}

/// A host whose claude runs, before anyone connected.
struct Host {
    first: Pipe,
    shared: Arc<Shared>,
    /// Hears once claude ended and a client, if any came, was told.
    ended: Receiver<()>,
}

struct Shared {
    state: Arc<Mutex<State>>,
    claude: Mutex<Headless>,
    /// [`HostInfo`] as JSON.
    info: String,
}

struct State {
    log: LineLog,
    client: Option<Client>,
    next_id: u64,
    /// claude's last stderr lines and exit code, once it ended.
    end: Option<(Vec<String>, u32)>,
    /// Set while the end waits for a client: the one that comes hands it
    /// the receiver that hears when it has read everything.
    waiting: Option<Sender<Receiver<()>>>,
    /// A client asked to end the session, so nobody waits to hear it end.
    closing: bool,
}

/// The client connected now, as the host sees it.
struct Client {
    id: u64,
    /// Encoded frames for its writer thread.
    tx: Sender<Vec<u8>>,
    /// Bytes queued and not yet written.
    queued: Arc<AtomicUsize>,
    /// Hears from the writer once it has flushed and let go.
    done: Receiver<()>,
}

impl Client {
    /// Queues a message. False once the client is too far behind to keep.
    fn send(&self, message: &LineMessage) -> bool {
        let frame = message.encode();
        let len = frame.len();
        let now = self.queued.fetch_add(len, Ordering::AcqRel) + len;
        let _ = self.tx.send(frame);
        now <= BEHIND
    }

    /// The end, after which the writer lets the client go.
    fn tell_end(self, stderr: &[String], code: u32) -> Receiver<()> {
        for line in stderr {
            self.send(&LineMessage::Stderr(line.clone()));
        }
        self.send(&LineMessage::Exit(code));
        self.done
    }
}

impl Host {
    fn start(spec: &HeadlessSpec) -> io::Result<Host> {
        // Taken before claude starts, so a second host for the same session
        // fails here instead of running a second claude.
        let first = Pipe::serve(&spec.pipe, true)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", spec.pipe)))?;
        let state = Arc::new(Mutex::new(State {
            log: LineLog::new(LOG_CAP),
            client: None,
            next_id: 0,
            end: None,
            waiting: None,
            closing: false,
        }));
        let (stdout_end, stdout_ended) = mpsc::channel::<()>();
        let lines = Arc::clone(&state);
        let claude = Headless::start_with(
            &spec.cwd,
            spec.resume.as_deref(),
            &spec.options,
            move |line| {
                let Some(line) = line else {
                    let _ = stdout_end.send(());
                    return;
                };
                let line = if line.len() > MAX_LINE {
                    too_long(line.len())
                } else {
                    line
                };
                let Ok(mut st) = lines.lock() else { return };
                let message = LineMessage::Out(line.clone());
                st.log.push(Logged::Out(line));
                if st.client.as_ref().is_some_and(|c| !c.send(&message)) {
                    st.client = None;
                }
            },
        )?;
        let info = HostInfo {
            pid: claude.pid(),
            cwd: spec.cwd.clone(),
            tag: spec.tag.clone(),
            started: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        };
        let shared = Arc::new(Shared {
            state,
            claude: Mutex::new(claude),
            info: serde_json::to_string(&info).map_err(io::Error::other)?,
        });

        let (ended_tx, ended) = mpsc::channel();
        {
            let shared = Arc::clone(&shared);
            thread::spawn(move || {
                let _ = stdout_ended.recv();
                let code = exit_code(&shared.claude);
                let stderr = shared
                    .claude
                    .lock()
                    .map(|c| c.stderr_tail())
                    .unwrap_or_default();
                let done = {
                    let Ok(mut st) = shared.state.lock() else {
                        let _ = ended_tx.send(());
                        return;
                    };
                    st.end = Some((stderr.clone(), code));
                    match st.client.take() {
                        Some(c) => Some(c.tell_end(&stderr, code)),
                        None if st.closing => None,
                        None => {
                            let (tx, rx) = mpsc::channel();
                            st.waiting = Some(tx);
                            drop(st);
                            rx.recv_timeout(LINGER).ok()
                        }
                    }
                };
                if let Some(done) = done {
                    let _ = done.recv_timeout(LAST_WORDS);
                }
                let _ = ended_tx.send(());
            });
        }
        Ok(Host {
            first,
            shared,
            ended,
        })
    }
}

/// claude's exit code once its stdout has ended, ending it if it lingers.
fn exit_code(claude: &Mutex<Headless>) -> u32 {
    let deadline = Instant::now() + EXIT_WAIT;
    loop {
        let Ok(mut c) = claude.lock() else { return 1 };
        if let Some(code) = c.exit_code() {
            return code;
        }
        if Instant::now() > deadline {
            c.kill();
        }
        drop(c);
        thread::sleep(Duration::from_millis(20));
    }
}

/// Gives a new client the hello, the info, the backlog, and the pipe from
/// now on; or, when claude has ended, the end, and lets it go.
fn welcome(pipe: Pipe, shared: &Arc<Shared>) {
    let pipe = Arc::new(pipe);
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let queued = Arc::new(AtomicUsize::new(0));
    let Ok(mut st) = shared.state.lock() else {
        return;
    };
    let id = st.next_id;
    st.next_id += 1;
    let hello = headless_hello().to_vec();
    queued.fetch_add(hello.len(), Ordering::AcqRel);
    let _ = tx.send(hello);
    let client = Client {
        id,
        tx,
        queued: Arc::clone(&queued),
        done: done_rx,
    };
    client.send(&LineMessage::Info(shared.info.clone()));
    for message in st.log.backlog() {
        client.send(&message);
    }
    let live = match st.end.clone() {
        Some((stderr, code)) => {
            let done = client.tell_end(&stderr, code);
            if let Some(waiting) = st.waiting.take() {
                let _ = waiting.send(done);
            }
            false
        }
        None => {
            // The client before this one loses its writer, which
            // disconnects it.
            st.client = Some(client);
            true
        }
    };
    drop(st);

    {
        let pipe = Arc::clone(&pipe);
        thread::spawn(move || {
            for frame in rx {
                let len = frame.len();
                if pipe.write_all(&frame).is_err() {
                    break;
                }
                queued.fetch_sub(len, Ordering::AcqRel);
            }
            pipe.flush();
            pipe.disconnect();
            let _ = done_tx.send(());
        });
    }
    if live {
        let shared = Arc::clone(shared);
        thread::spawn(move || listen(&pipe, &shared, id));
    }
}

/// Reads what a client says until it goes, then forgets it if it is still
/// the client.
fn listen(pipe: &Pipe, shared: &Arc<Shared>, id: u64) {
    let mut decoder = LineDecoder::default();
    let mut buf = vec![0u8; 64 * 1024];
    'conn: loop {
        let n = match pipe.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        decoder.push(&buf[..n]);
        loop {
            match decoder.take() {
                Ok(Some(LineMessage::Send(line))) => {
                    if let Ok(mut st) = shared.state.lock() {
                        st.log.push(Logged::Sent(line.clone()));
                    }
                    if let Ok(c) = shared.claude.lock() {
                        let _ = c.send_line(&line);
                    }
                }
                Ok(Some(LineMessage::Close)) => close(shared),
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => break 'conn,
            }
        }
    }
    if let Ok(mut st) = shared.state.lock() {
        if st.client.as_ref().is_some_and(|c| c.id == id) {
            st.client = None;
        }
    }
}

/// Ends the session: claude's stdin closes so it can end itself, and its
/// job ends whatever is left after [`GRACE`]. The end then goes the usual
/// way, through stdout's end.
fn close(shared: &Arc<Shared>) {
    if let Ok(mut st) = shared.state.lock() {
        st.closing = true;
    }
    if let Ok(c) = shared.claude.lock() {
        c.close_input();
    }
    let shared = Arc::clone(shared);
    thread::spawn(move || {
        let deadline = Instant::now() + GRACE;
        while Instant::now() < deadline {
            let running = shared.claude.lock().is_ok_and(|mut c| c.running());
            if !running {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        if let Ok(mut c) = shared.claude.lock() {
            c.kill();
        }
    });
}

/// What a host says to its client, as [`Hosted`] hands it on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A line claude wrote to stdout, as [`Headless`]'s `on_line` gets it.
    Out(String),
    /// A line a client sent to claude's stdin before this one attached:
    /// a prompt, an answer to an ask, an interrupt. Only in the backlog,
    /// in its place among the [`Event::Out`] lines.
    Sent(String),
    /// The backlog lost this many of its oldest lines, which the session's
    /// transcript still has. Comes first when it does.
    Dropped(u64),
    /// The backlog is over; every line from here on is new.
    Live,
    /// The end, last: claude's exit code, or None when the host went away
    /// without saying (killed by hand, or a client that took over).
    End(Option<u32>),
}

/// A headless session running in a host, as its client sees it.
/// Dropping it detaches: claude runs on, and [`Hosted::attach`] finds it
/// again. [`Hosted::close`] ends it for good.
pub struct Hosted {
    id: String,
    info: HostInfo,
    pipe: Arc<Pipe>,
    input: Mutex<Option<Sender<Vec<u8>>>>,
    /// The thread that writes `input` to the pipe, joined by a close.
    writer: Mutex<Option<thread::JoinHandle<()>>>,
    running: Arc<AtomicBool>,
    /// Set by Drop, so the reader it stops says nothing more.
    detached: Arc<AtomicBool>,
    /// Set by the reader as it ends, so Drop knows to stop cancelling.
    reader_gone: Arc<AtomicBool>,
    stderr: Arc<Mutex<Vec<String>>>,
    next_request: AtomicU64,
}

impl Hosted {
    /// Starts a host running `exe host --headless` for session `id` of
    /// Horadric `instance`, with claude.exe in `cwd`, resuming `resume`
    /// when given, and attaches to it. `tag` comes back in every attach's
    /// [`HostInfo`]. Every [`Event`] goes to `on_event` on a reader thread.
    /// AlreadyExists when the session has a host already.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        exe: &Path,
        instance: &str,
        id: &str,
        cwd: &Path,
        resume: Option<&str>,
        options: &Options,
        tag: &str,
        on_event: impl FnMut(Event) + Send + 'static,
    ) -> io::Result<Hosted> {
        let pipe = headless_pipe_name(instance, id);
        // Looked for, not connected to: a host takes any connection as
        // its client, which would let the real one go.
        let taken = Hosted::list(instance)
            .iter()
            .any(|other| headless_pipe_name(instance, other) == pipe);
        if taken {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("session {id} has a host already"),
            ));
        }
        let spec = HeadlessSpec {
            pipe: pipe.clone(),
            cwd: cwd.to_path_buf(),
            resume: resume.map(str::to_string),
            options: options.clone(),
            tag: tag.to_string(),
        };
        let json = serde_json::to_vec(&spec).map_err(io::Error::other)?;
        let connected = launch(exe, &["host", "--headless"], &json, || connect(&pipe))?;
        Ok(Hosted::begin(id, connected, on_event))
    }

    /// Attaches to the host of session `id`, which replays its backlog
    /// before the live tail. NotFound when there is no such host.
    pub fn attach(
        instance: &str,
        id: &str,
        on_event: impl FnMut(Event) + Send + 'static,
    ) -> io::Result<Hosted> {
        let connected = connect(&headless_pipe_name(instance, id))?;
        Ok(Hosted::begin(id, connected, on_event))
    }

    /// The ids of every headless session whose host runs for `instance`,
    /// with a client or without, for a UI to attach after a reload.
    pub fn list(instance: &str) -> Vec<String> {
        crate::pipe::list(&headless_pipe_prefix(instance))
    }

    fn begin(
        id: &str,
        connected: Connected,
        mut on_event: impl FnMut(Event) + Send + 'static,
    ) -> Hosted {
        let Connected {
            pipe,
            info,
            mut incoming,
        } = connected;
        let (input, rx) = mpsc::channel::<Vec<u8>>();
        let writer = {
            let pipe = Arc::clone(&pipe);
            thread::spawn(move || {
                for frame in rx {
                    if pipe.write_all(&frame).is_err() {
                        break;
                    }
                }
            })
        };
        let running = Arc::new(AtomicBool::new(true));
        let detached = Arc::new(AtomicBool::new(false));
        let reader_gone = Arc::new(AtomicBool::new(false));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        {
            let pipe = Arc::clone(&pipe);
            let running = Arc::clone(&running);
            let detached = Arc::clone(&detached);
            let reader_gone = Arc::clone(&reader_gone);
            let stderr = Arc::clone(&stderr);
            thread::spawn(move || {
                let code = loop {
                    match incoming.recv(&pipe) {
                        Some(LineMessage::Out(l)) => on_event(Event::Out(l)),
                        Some(LineMessage::Sent(l)) => on_event(Event::Sent(l)),
                        Some(LineMessage::Dropped(n)) => on_event(Event::Dropped(n)),
                        Some(LineMessage::Live) => on_event(Event::Live),
                        Some(LineMessage::Stderr(l)) => {
                            if let Ok(mut s) = stderr.lock() {
                                s.push(l);
                            }
                        }
                        Some(LineMessage::Exit(code)) => break Some(code),
                        Some(_) => {}
                        None => break None,
                    }
                };
                running.store(false, Ordering::Release);
                drop(pipe);
                reader_gone.store(true, Ordering::Release);
                if !detached.load(Ordering::Acquire) {
                    on_event(Event::End(code));
                }
            });
        }
        Hosted {
            id: id.to_string(),
            info,
            pipe,
            input: Mutex::new(Some(input)),
            writer: Mutex::new(Some(writer)),
            running,
            detached,
            reader_gone,
            stderr,
            next_request: AtomicU64::new(1),
        }
    }

    pub fn send_prompt(&self, text: &str) -> io::Result<()> {
        self.send_line(&prompt_line(text))
    }

    /// Stops the turn in progress. The CLI ends it with a `result` and
    /// stays up for the next prompt.
    pub fn interrupt(&self) -> io::Result<()> {
        self.send_line(&control_request(self.request_id(), "interrupt"))
    }

    /// Writes one raw line to claude's stdin, such as the
    /// `control_response` from `Folio::answer`. The host keeps it for the
    /// backlog.
    pub fn send_line(&self, line: &str) -> io::Result<()> {
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        self.send(LineMessage::Send(line))
    }

    /// Ends the session for good: claude gets its stdin closed and a grace
    /// period to end itself, then its whole tree is ended. The end comes
    /// back as [`Event::End`] like any other.
    ///
    /// Returns once the close is written, or after [`CLOSE_WAIT`]: an app
    /// that quits right after would otherwise take a close still queued
    /// with it, and its hosts and claude would run on to be found again at
    /// the next start. Nothing more can be sent after.
    pub fn close(&self) {
        let _ = self.send(LineMessage::Close);
        if let Ok(mut input) = self.input.lock() {
            input.take();
        }
        let writer = self.writer.lock().ok().and_then(|mut w| w.take());
        if let Some(writer) = writer {
            let deadline = Instant::now() + CLOSE_WAIT;
            while !writer.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
        }
    }

    fn send(&self, message: LineMessage) -> io::Result<()> {
        if !self.running() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "claude.exe has ended",
            ));
        }
        let input = self
            .input
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?;
        input
            .as_ref()
            .and_then(|tx| tx.send(message.encode()).ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "the host has gone"))
    }

    /// The session's id, as [`Hosted::list`] gives it.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// What the host said about itself as this client attached.
    pub fn info(&self) -> &HostInfo {
        &self.info
    }

    /// claude.exe's process id.
    pub fn pid(&self) -> u32 {
        self.info.pid
    }

    /// Whether claude.exe is still running, as far as its host has said.
    pub fn running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// The last lines claude.exe wrote to stderr, oldest first. The host
    /// sends them as claude ends, so they are there by [`Event::End`].
    pub fn stderr_tail(&self) -> Vec<String> {
        self.stderr.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Unique beyond this client, since an interrupt this one sends may
    /// still be answered after another attached.
    fn request_id(&self) -> String {
        format!(
            "horadric_{}_{}",
            std::process::id(),
            self.next_request.fetch_add(1, Ordering::Relaxed)
        )
    }
}

impl Drop for Hosted {
    /// Detaches: lets go of the pipe, which the host takes as its client
    /// gone, and claude runs on. The reader may be between reads as a
    /// cancel lands, so it is cancelled until it has ended.
    fn drop(&mut self) {
        self.detached.store(true, Ordering::Release);
        if let Ok(mut input) = self.input.lock() {
            input.take();
        }
        let pipe = Arc::clone(&self.pipe);
        let gone = Arc::clone(&self.reader_gone);
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !gone.load(Ordering::Acquire) && Instant::now() < deadline {
                pipe.cancel();
                thread::sleep(Duration::from_millis(20));
            }
        });
    }
}

/// A host just connected to, its hello and info read.
struct Connected {
    pipe: Arc<Pipe>,
    info: HostInfo,
    incoming: Incoming,
}

/// Connects to the headless host on `pipe`. NotFound when there is none,
/// InvalidData when it is not one this build speaks to.
fn connect(pipe: &str) -> io::Result<Connected> {
    let pipe = Arc::new(Pipe::connect(pipe)?);
    let mut first = [0u8; 5];
    pipe.read_exact(&mut first)?;
    read_headless_hello(&first).map_err(|why| io::Error::new(io::ErrorKind::InvalidData, why))?;
    let mut incoming = Incoming::default();
    let Some(LineMessage::Info(json)) = incoming.recv(&pipe) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the headless host did not say what it runs",
        ));
    };
    let info =
        serde_json::from_str(&json).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Connected {
        pipe,
        info,
        incoming,
    })
}

#[derive(Default)]
struct Incoming {
    decoder: LineDecoder,
    buf: Vec<u8>,
}

impl Incoming {
    /// The next thing the host says. None once it is gone or garbled.
    fn recv(&mut self, pipe: &Pipe) -> Option<LineMessage> {
        if self.buf.is_empty() {
            self.buf = vec![0u8; 64 * 1024];
        }
        loop {
            match self.decoder.take() {
                Ok(Some(m)) => return Some(m),
                Ok(None) => {}
                Err(_) => return None,
            }
            match pipe.read(&mut self.buf) {
                Ok(0) | Err(_) => return None,
                Ok(n) => self.decoder.push(&self.buf[..n]),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host run in this process on threads, with cmd.exe standing in
    /// for claude: it runs each stdin line as a command and echoes it.
    /// Start, send, detach, attach again for the backlog, close.
    #[test]
    fn a_headless_host_keeps_lines_for_the_next_client_and_ends_on_close() {
        let instance = format!("test{}", std::process::id());
        let id = "chat";
        let spec = HeadlessSpec {
            pipe: headless_pipe_name(&instance, id),
            cwd: std::env::temp_dir(),
            resume: None,
            options: Options {
                program: Some(
                    std::env::var("ComSpec")
                        .unwrap_or_else(|_| "cmd.exe".into())
                        .into(),
                ),
                ..Options::default()
            },
            tag: "project-a".into(),
        };
        let host = Host::start(&spec).unwrap();
        let shared = Arc::clone(&host.shared);
        let ended = host.ended;
        let first_pipe = host.first;
        let pipe_name = spec.pipe.clone();
        thread::spawn(move || {
            let mut next = Some(first_pipe);
            for _ in 0..2 {
                let pipe = next
                    .take()
                    .unwrap_or_else(|| Pipe::serve(&pipe_name, false).unwrap());
                pipe.accept().unwrap();
                welcome(pipe, &shared);
            }
        });

        assert_eq!(Hosted::list(&instance), vec![id.to_string()]);
        let (tx, rx) = mpsc::channel();
        let first = Hosted::attach(&instance, id, move |e| {
            let _ = tx.send(e);
        })
        .unwrap();
        assert_eq!(first.info().tag, "project-a");
        assert!(first.pid() > 0);
        first.send_line("echo said-before-detach").unwrap();
        wait_for(
            &rx,
            |e| matches!(e, Event::Out(l) if l.contains("said-before-detach") && !l.contains("echo")),
        );
        drop(first);
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "a detach says nothing more"
        );

        let (tx, rx) = mpsc::channel();
        let second = Hosted::attach(&instance, id, move |e| {
            let _ = tx.send(e);
        })
        .unwrap();
        let backlog = wait_for(&rx, |e| *e == Event::Live);
        assert!(backlog.contains(&Event::Sent("echo said-before-detach".into())));
        assert!(backlog
            .iter()
            .any(|e| matches!(e, Event::Out(l) if l == "said-before-detach")));
        second.send_line("echo said-live").unwrap();
        wait_for(&rx, |e| *e == Event::Out("said-live".into()));

        second.close();
        let rest = wait_for(&rx, |e| matches!(e, Event::End(_)));
        assert_eq!(
            rest.last(),
            Some(&Event::End(Some(0))),
            "cmd ends on stdin's end"
        );
        assert!(!second.running());
        assert!(second.send_line("echo too late").is_err());
        ended.recv_timeout(Duration::from_secs(10)).unwrap();
    }

    /// A client that closes and is gone at once, as an app quitting with
    /// its sessions, still ends the session.
    #[test]
    fn a_close_dropped_at_once_still_ends_the_host() {
        let instance = format!("close{}", std::process::id());
        let id = "chat";
        let spec = HeadlessSpec {
            pipe: headless_pipe_name(&instance, id),
            cwd: std::env::temp_dir(),
            resume: None,
            options: Options {
                program: Some(
                    std::env::var("ComSpec")
                        .unwrap_or_else(|_| "cmd.exe".into())
                        .into(),
                ),
                ..Options::default()
            },
            tag: "project-a".into(),
        };
        let host = Host::start(&spec).unwrap();
        let shared = Arc::clone(&host.shared);
        let ended = host.ended;
        let first_pipe = host.first;
        thread::spawn(move || {
            first_pipe.accept().unwrap();
            welcome(first_pipe, &shared);
        });
        let client = Hosted::attach(&instance, id, |_| {}).unwrap();
        for n in 0..50 {
            client.send_line(&format!("echo queued-{n}")).unwrap();
        }
        client.close();
        drop(client);
        ended.recv_timeout(Duration::from_secs(10)).unwrap();
    }

    /// Every event up to and with the first that `want` takes.
    fn wait_for(rx: &Receiver<Event>, want: impl Fn(&Event) -> bool) -> Vec<Event> {
        let mut seen = Vec::new();
        loop {
            let e = rx
                .recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|_| panic!("timed out after {seen:?}"));
            let done = want(&e);
            seen.push(e);
            if done {
                return seen;
            }
        }
    }
}
