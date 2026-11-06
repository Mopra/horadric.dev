//! `horadric secret run`, `set` and `list` in the app: the key each session
//! is born with, the question to the human, and the program started with
//! the values in its environment and its output masked on the way back.
//! The values never leave the app. See docs/plan/vault.md.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::ffi::{c_void, OsString};
use std::io::{Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use horadric_core::vault::secret::{log_line, missing_line, once_only, valid_name, Mask, Owner};
use horadric_hooks::listener::{SecretNames, SecretRun, SecretSet, SecretStream};
use serde_json::json;
use windows::core::HSTRING;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, KillTimer, PostMessageW, SetTimer, WM_CLOSE,
};

use super::store;
use crate::dialog::{Dialog, Tone};

/// How long a `secret run` waits for the human before it fails with the
/// line that tells the agent to block the quest instead.
const WAIT: Duration = Duration::from_secs(60);

/// How long a `secret set` waits: the human typed it, so is at the
/// machine, but may have to find the app's window first.
const SET_WAIT: Duration = Duration::from_secs(120);

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Each live session's key, by session id. In memory only: a key dies
/// with its session, or with the app.
static KEYS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// A new random key for a session, to go into its environment as
/// `HORADRIC_SECRET_KEY` when it starts.
pub fn mint_key(session: &str) -> String {
    let mut bytes = [0u8; 32];
    crate::portal_crypto::random(&mut bytes);
    let key: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    KEYS.lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(session.to_string(), key.clone());
    key
}

/// Forgets a session's key, once the session has ended.
pub fn drop_key(session: &str) {
    if let Some(keys) = KEYS.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        keys.remove(session);
    }
}

/// Whether `key` is the live session's own. Compared in constant time, so
/// the time an answer takes says nothing about how much of a guess was
/// right.
pub fn key_ok(session: &str, key: &str) -> bool {
    let keys = KEYS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(kept) = keys.as_ref().and_then(|k| k.get(session)) else {
        return false;
    };
    let (a, b) = (kept.as_bytes(), key.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// Where a session is, as the app knows it: what E fills from the
/// session's registry entry before calling [`serve`] or [`names`].
#[derive(Clone, Debug)]
pub struct Place {
    /// The project's key.
    pub key: String,
    /// The project's own folder, its main working tree.
    pub dir: PathBuf,
    /// The session's own folder, its worktree when it has one.
    pub cwd: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Once,
    Always,
    No,
}

/// A question put to the human, to wait for off the UI thread.
pub struct Asked {
    rx: Receiver<Option<usize>>,
    gone: Arc<AtomicBool>,
    id: u64,
    title: String,
    always: bool,
}

impl Asked {
    /// The human's answer, or None when nobody answered within `within`;
    /// the question is then taken off the screen. Closing it is No.
    pub fn wait(self, within: Duration) -> Option<Answer> {
        match self.rx.recv_timeout(within) {
            Ok(pressed) => Some(match (pressed, self.always) {
                (Some(0), _) => Answer::Once,
                (Some(1), true) => Answer::Always,
                _ => Answer::No,
            }),
            Err(_) => {
                self.gone.store(true, Ordering::Relaxed);
                close_if_shown(self.id, &self.title);
                None
            }
        }
    }

    fn pressed(self, within: Duration) -> Option<Option<usize>> {
        match self.rx.recv_timeout(within) {
            Ok(p) => Some(p),
            Err(_) => {
                self.gone.store(true, Ordering::Relaxed);
                close_if_shown(self.id, &self.title);
                None
            }
        }
    }
}

/// Asks the human Once, Always (only when `always`) or No. Call it on the
/// UI thread, inside the app's borrow or not: the question opens once the
/// current message is handled, outside the borrow, as a modal dialog
/// whose loop keeps the app running, and one at a time. Wait for the
/// answer with [`Asked::wait`] on another thread.
pub fn ask(session: &str, title: &str, text: &str, always: bool) -> Asked {
    let program = title_program(title);
    let mut buttons = vec!["Once".to_string()];
    if always {
        buttons.push(match program {
            Some(p) => format!("Always for {p}"),
            None => "Always".to_string(),
        });
    }
    buttons.push("No".to_string());
    let mut asked = put(session, title, text, buttons);
    asked.always = always;
    asked
}

/// Lets a title say which program Always is for: "Use X for `gh`?" gives
/// `gh`. Plain titles get a plain Always.
fn title_program(title: &str) -> Option<&str> {
    let start = title.find('`')? + 1;
    let len = title[start..].find('`')?;
    Some(&title[start..start + len])
}

struct Question {
    id: u64,
    title: String,
    text: String,
    buttons: Vec<String>,
    tx: Sender<Option<usize>>,
    gone: Arc<AtomicBool>,
    tries: u32,
}

thread_local! {
    static QUEUE: RefCell<VecDeque<Question>> = const { RefCell::new(VecDeque::new()) };
    static ASKING: Cell<bool> = const { Cell::new(false) };
}

/// The question on screen now, so a timed out wait closes only its own.
static SHOWING: Mutex<Option<u64>> = Mutex::new(None);
static NEXT: AtomicU64 = AtomicU64::new(1);

fn put(_session: &str, title: &str, text: &str, buttons: Vec<String>) -> Asked {
    let (tx, rx) = mpsc::channel();
    let gone = Arc::new(AtomicBool::new(false));
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    QUEUE.with(|q| {
        q.borrow_mut().push_back(Question {
            id,
            title: title.to_string(),
            text: text.to_string(),
            buttons,
            tx,
            gone: Arc::clone(&gone),
            tries: 0,
        })
    });
    soon(0);
    Asked {
        rx,
        gone,
        id,
        title: title.to_string(),
        always: false,
    }
}

/// A thread timer: its callback runs from the message loop, so outside
/// whatever borrow of the app the caller is in.
fn soon(ms: u32) {
    unsafe {
        SetTimer(None, 0, ms, Some(show_queued));
    }
}

unsafe extern "system" fn show_queued(_: HWND, _: u32, id: usize, _: u32) {
    let _ = unsafe { KillTimer(None, id) };
    if ASKING.with(Cell::get) {
        // The loop already running takes this one when its own is answered.
        return;
    }
    ASKING.with(|a| a.set(true));
    while let Some(mut q) = QUEUE.with(|q| q.borrow_mut().pop_front()) {
        if q.gone.load(Ordering::Relaxed) {
            continue;
        }
        *SHOWING.lock().unwrap_or_else(|e| e.into_inner()) = Some(q.id);
        let buttons: Vec<&str> = q.buttons.iter().map(String::as_str).collect();
        let shown = Instant::now();
        let pressed = crate::app::ask(&Dialog {
            tone: Tone::Question,
            title: &q.title,
            text: &q.text,
            buttons: &buttons,
            // Enter on a question that came out of nowhere must not give
            // a secret away.
            default: buttons.len() - 1,
            check: None,
        });
        *SHOWING.lock().unwrap_or_else(|e| e.into_inner()) = None;
        // No answer at once means the app was busy in a loop of its own
        // and showed nothing, so it is asked again shortly.
        if pressed.is_none()
            && shown.elapsed() < Duration::from_millis(300)
            && !q.gone.load(Ordering::Relaxed)
            && q.tries < 100
        {
            q.tries += 1;
            QUEUE.with(|queue| queue.borrow_mut().push_front(q));
            soon(500);
            break;
        }
        let _ = q.tx.send(pressed);
    }
    ASKING.with(|a| a.set(false));
}

fn close_if_shown(id: u64, title: &str) {
    if *SHOWING.lock().unwrap_or_else(|e| e.into_inner()) != Some(id) {
        return;
    }
    unsafe {
        if let Ok(hwnd) = FindWindowW(crate::dialog::CLASS, &HSTRING::from(title)) {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn identity_slug(dir: &Path) -> Option<String> {
    super::identity::of_project(dir).slug().map(str::to_string)
}

/// A project's name as the human knows it: its folder's.
fn project_name(key: &str) -> String {
    Path::new(key)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| key.to_string())
}

/// The `PATH` the app itself was started with, without its relative
/// entries: those would be looked up from wherever the app stands.
fn app_path() -> OsString {
    let raw = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::env::split_paths(&raw).filter(|d| d.is_absolute());
    std::env::join_paths(dirs).unwrap_or_default()
}

/// The extensions a program by name may have, in `PATHEXT`'s order, of the
/// ones a process can be started from.
fn exts() -> Vec<String> {
    const RUNNABLE: [&str; 4] = [".exe", ".com", ".cmd", ".bat"];
    let ext = std::env::var("PATHEXT").unwrap_or_default().to_ascii_lowercase();
    let mut out: Vec<String> = ext
        .split(';')
        .map(str::trim)
        .filter(|e| RUNNABLE.contains(e))
        .map(str::to_string)
        .collect();
    if out.is_empty() {
        out = RUNNABLE.iter().map(|e| e.to_string()).collect();
    }
    out
}

/// The program's full path: an absolute path as given, else found on the
/// app's `PATH`, never in the current folder, where Windows looks first
/// and an agent can drop a `gh.cmd`.
fn program_path(program: &str) -> Result<PathBuf, String> {
    let exts = exts();
    if program.contains(['/', '\\']) {
        let p = PathBuf::from(program);
        if !p.is_absolute() {
            return Err(format!(
                "{program} is a relative path. Give the program by name, found on PATH, or by its full path."
            ));
        }
        if p.is_file() {
            return Ok(p);
        }
        return exts
            .iter()
            .map(|e| PathBuf::from(format!("{program}{e}")))
            .find(|c| c.is_file())
            .ok_or_else(|| format!("{program} does not exist."));
    }
    let lower = program.to_ascii_lowercase();
    let mut tries: Vec<&str> = Vec::new();
    if exts.iter().any(|e| lower.ends_with(e.as_str())) {
        tries.push("");
    }
    tries.extend(exts.iter().map(String::as_str));
    horadric_pty::find_program(program, &app_path(), &tries, Path::is_file)
        .ok_or_else(|| format!("{program} is not on PATH."))
}

/// A path as compared: lower case, one kind of slash, no trailing one.
fn spelled(p: &Path) -> String {
    let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = p.to_string_lossy().to_ascii_lowercase().replace('/', "\\");
    s.trim_start_matches(r"\\?\").trim_end_matches('\\').to_string()
}

fn inside(path: &Path, dir: &Path) -> bool {
    let (p, d) = (spelled(path), spelled(dir));
    !d.is_empty() && (p == d || p.starts_with(&format!("{d}\\")))
}

/// What the human reads of the command: the program's name and the
/// arguments, cut short. Never logged.
fn shown_command(program: &Path, args: &[String]) -> String {
    let name = program
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut line = std::iter::once(name)
        .chain(args.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");
    if line.chars().count() > 160 {
        line = line.chars().take(157).collect::<String>() + "...";
    }
    line
}

/// The secret run route: checks the session and its key, finds the
/// program and the secrets, asks the human where needed, then starts the
/// program on a worker thread and streams its masked output back. Call it
/// on the UI thread; it returns at once.
pub fn serve(place: Option<Place>, req: SecretRun) {
    let SecretRun {
        session,
        key,
        names,
        stdin,
        program,
        args,
        cwd,
        out,
    } = req;
    if session.is_empty() || !key_ok(&session, &key) {
        out.fail("Refused: this is not a live Horadric session with its own key.");
        return;
    }
    let Some(place) = place else {
        out.fail("Refused: Horadric does not know this session's project.");
        return;
    };
    let mut wanted: Vec<String> = Vec::new();
    for n in names.iter().chain(&stdin) {
        if !valid_name(n) {
            out.fail(&format!("{n} is not a secret's name: upper case letters, digits and _."));
            return;
        }
        if !wanted.contains(n) {
            wanted.push(n.clone());
        }
    }
    if wanted.is_empty() {
        out.fail("Name the secrets to use: horadric secret run NAME[,NAME] -- program args...");
        return;
    }
    let cwd = PathBuf::from(&cwd);
    if !cwd.is_dir() || !(inside(&cwd, &place.dir) || inside(&cwd, &place.cwd)) {
        out.fail("Refused: run it from inside the session's project folder.");
        return;
    }
    let program = match program_path(&program) {
        Ok(p) => p,
        Err(why) => {
            out.fail(&why);
            return;
        }
    };
    let program_lower = program.to_string_lossy().to_ascii_lowercase();
    let identity = identity_slug(&place.dir);
    let at = now();
    let log = |name: &str, outcome: &str| {
        store::log(&log_line(at, &place.key, &session, name, &program_lower, outcome));
    };
    let mut secrets = Vec::new();
    for n in &wanted {
        match store::secret(&place.key, identity.as_deref(), n) {
            Some(s) => secrets.push(s),
            None => {
                log(n, "missing");
                out.fail(&missing_line(n));
                return;
            }
        }
    }
    let unasked: Vec<String> = secrets
        .iter()
        .filter(|s| !s.allowed.contains(&program_lower))
        .map(|s| s.name.clone())
        .collect();
    let asked = (!unasked.is_empty()).then(|| {
        let writable = inside(&program, &place.dir)
            || inside(&program, &place.cwd)
            || inside(&program, &std::env::temp_dir());
        let always = !once_only(&program_lower) && !writable;
        let stem = program
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let title = format!("Use {} for `{stem}`?", unasked.join(", "));
        let text = format!(
            "A session of {} asks to run `{}` with {} in its environment.\n\n{}",
            project_name(&place.key),
            shown_command(&program, &args),
            unasked.join(", "),
            program.display(),
        );
        ask(&session, &title, &text, always)
    });
    let job = Job {
        session,
        project: place.key,
        program,
        program_lower,
        args,
        cwd,
        env: super::identity::session_env(&place.dir),
        secrets,
        stdin,
        unasked,
        at,
    };
    thread::spawn(move || job.run(asked, out));
}

struct Job {
    session: String,
    project: String,
    program: PathBuf,
    program_lower: String,
    args: Vec<String>,
    cwd: PathBuf,
    env: super::identity::SessionEnv,
    secrets: Vec<store::Secret>,
    stdin: Vec<String>,
    /// The secrets not yet allowed for this program, which were asked.
    unasked: Vec<String>,
    at: u64,
}

impl Job {
    fn log(&self, name: &str, outcome: &str) {
        store::log(&log_line(
            self.at,
            &self.project,
            &self.session,
            name,
            &self.program_lower,
            outcome,
        ));
    }

    fn run(self, asked: Option<Asked>, out: SecretStream) {
        let answer = match asked {
            None => Answer::Once,
            Some(a) => match a.wait(WAIT) {
                Some(a) => a,
                None => {
                    for n in &self.unasked {
                        self.log(n, "unanswered");
                    }
                    let n = &self.unasked[0];
                    out.fail(&format!(
                        "Nobody answered whether {n} may go to {}. Ask the human to allow it, \
                         or block the quest with --on-secret {n}.",
                        self.program.display()
                    ));
                    return;
                }
            },
        };
        for s in &self.secrets {
            let outcome = match (self.unasked.contains(&s.name), answer) {
                (false, _) => "allowed",
                (true, Answer::Once) => "asked: once",
                (true, Answer::Always) => "asked: always",
                (true, Answer::No) => "refused",
            };
            self.log(&s.name, outcome);
        }
        if answer == Answer::No {
            out.fail(&format!(
                "The human said no to {} for {}.",
                self.unasked.join(", "),
                self.program.display()
            ));
            return;
        }
        let at = self.at;
        let what = self
            .program
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let _ = store::change(|v| {
            for kept in &self.secrets {
                let Some(s) = v
                    .secrets
                    .iter_mut()
                    .find(|s| s.name == kept.name && s.owner == kept.owner)
                else {
                    continue;
                };
                if answer == Answer::Always
                    && self.unasked.contains(&s.name)
                    && !s.allowed.contains(&self.program_lower)
                {
                    s.allowed.push(self.program_lower.clone());
                }
                s.last_used = Some(store::Used {
                    at,
                    session: self.session.clone(),
                    what: what.clone(),
                });
            }
        });
        self.start(out);
    }

    fn command(&self) -> Command {
        let mut c = if horadric_pty::is_batch(&self.program) {
            // A batch file only runs through cmd, and its arguments are
            // read twice there, so they are quoted the way the pty's
            // launches quote them.
            let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
            let line = horadric_pty::launch_line(&self.program, &self.args);
            let head = horadric_pty::command_line(Path::new(&comspec), &[]);
            let mut c = Command::new(&comspec);
            c.raw_arg(line.strip_prefix(&head).unwrap_or(&line).trim_start());
            c
        } else {
            let mut c = Command::new(&self.program);
            c.args(&self.args);
            c
        };
        c.current_dir(&self.cwd).creation_flags(CREATE_NO_WINDOW);
        c.env_remove("HORADRIC_SECRET_KEY");
        for name in &self.env.remove {
            c.env_remove(name);
        }
        for (k, v) in &self.env.set {
            c.env(k, v);
        }
        for s in &self.secrets {
            c.env(&s.name, &s.value);
        }
        c.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(if self.stdin.is_empty() {
            Stdio::null()
        } else {
            Stdio::piped()
        });
        c
    }

    fn start(self, out: SecretStream) {
        let tree = Tree::new();
        let mut child = match self.command().spawn() {
            Ok(c) => c,
            Err(e) => {
                out.fail(&format!("cannot start {}: {e}", self.program.display()));
                return;
            }
        };
        if let Some(t) = &tree {
            t.adopt(&child);
        }
        let values: Vec<(String, String)> = self
            .secrets
            .iter()
            .map(|s| (s.name.clone(), s.value.clone()))
            .collect();
        if let Some(mut input) = child.stdin.take() {
            let lines: Vec<String> = self
                .stdin
                .iter()
                .filter_map(|n| values.iter().find(|(name, _)| name == n))
                .map(|(_, v)| format!("{v}\n"))
                .collect();
            // Its own thread: a program that never reads its input must
            // not hold the output back.
            thread::spawn(move || {
                for l in lines {
                    if input.write_all(l.as_bytes()).is_err() {
                        break;
                    }
                }
            });
        }
        let failed = Arc::new(AtomicBool::new(false));
        let readers: Vec<_> = [
            child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
            child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
        ]
        .into_iter()
        .enumerate()
        .filter_map(|(i, pipe)| {
            let pipe = pipe?;
            let mut out = out.clone();
            let mut mask = Mask::new(values.clone());
            let failed = Arc::clone(&failed);
            Some(thread::spawn(move || pump(pipe, &mut mask, i == 1, &mut out, &failed)))
        })
        .collect();
        // A CLI that went away takes the program with it.
        let code = loop {
            if failed.load(Ordering::Relaxed) {
                if let Some(t) = &tree {
                    t.end();
                }
                let _ = child.kill();
            }
            match child.try_wait() {
                Ok(Some(status)) => break status.code().unwrap_or(-1),
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(_) => break -1,
            }
        };
        for r in readers {
            let _ = r.join();
        }
        out.exit(code);
        drop(tree);
    }
}

/// Passes one pipe through its mask to the CLI until the pipe ends.
fn pump(
    mut pipe: Box<dyn Read + Send>,
    mask: &mut Mask,
    is_err: bool,
    out: &mut SecretStream,
    failed: &AtomicBool,
) {
    let send = |out: &mut SecretStream, b: &[u8]| {
        if b.is_empty() {
            return Ok(());
        }
        if is_err {
            out.err(b)
        } else {
            out.out(b)
        }
    };
    let mut buf = [0u8; 8192];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let masked = mask.push(&buf[..n]);
                if send(out, &masked).is_err() {
                    failed.store(true, Ordering::Relaxed);
                    return;
                }
            }
        }
    }
    let tail = mask.finish();
    if send(out, &tail).is_err() {
        failed.store(true, Ordering::Relaxed);
    }
}

/// A job that ends everything in it when its last handle closes, so a
/// program started for a secret dies with the app.
struct Tree(OwnedHandle);

impl Tree {
    fn new() -> Option<Tree> {
        unsafe {
            let job = CreateJobObjectW(None, None).ok()?;
            let job = OwnedHandle::from_raw_handle(job.0);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                HANDLE(job.as_raw_handle()),
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const c_void,
                std::mem::size_of_val(&limits) as u32,
            )
            .ok()?;
            Some(Tree(job))
        }
    }

    fn adopt(&self, child: &Child) {
        unsafe {
            let _ = AssignProcessToJobObject(
                HANDLE(self.0.as_raw_handle()),
                HANDLE(child.as_raw_handle()),
            );
        }
    }

    fn end(&self) {
        unsafe {
            let _ = TerminateJobObject(HANDLE(self.0.as_raw_handle()), 1);
        }
    }
}

/// `horadric secret set`: keeps a value once the human says so in the
/// app, since an agent could otherwise swap a secret for a value of its
/// own. Call it on the UI thread; it returns at once.
pub fn set(req: SecretSet) {
    let SecretSet {
        session,
        cwd,
        name,
        value,
        owner,
        reply,
    } = req;
    let error = |why: String| reply.send(json!({ "error": why }));
    if !valid_name(&name) {
        error(format!("{name} is not a secret's name: upper case letters, digits and _, a letter first."));
        return;
    }
    if value.is_empty() {
        error("An empty value is not kept.".into());
        return;
    }
    let (owner, whose) = match owner.as_str() {
        "everywhere" => (Owner::Everywhere, "every project".to_string()),
        "project" | "" => {
            let key = crate::project::folder_key(&cwd);
            if key.is_empty() {
                error("Run it inside a project's folder, or give --everywhere.".into());
                return;
            }
            let whose = project_name(&key);
            (Owner::Project(key), whose)
        }
        o => match o.strip_prefix("identity:") {
            Some(slug) => match super::identity::get(slug) {
                Some(id) => (Owner::Identity(slug.to_string()), id.name),
                None => {
                    error(format!("No identity {slug} in this Horadric."));
                    return;
                }
            },
            None => {
                error(format!("{o} is not an owner: project, everywhere or identity:SLUG."));
                return;
            }
        },
    };
    let replaces = store::read()
        .secrets
        .iter()
        .any(|s| s.name == name && s.owner == owner);
    let title = format!("Keep {name}?");
    let text = format!(
        "A terminal asks to keep {name} for {whose}{}. Keep it only if you typed it yourself.",
        if replaces { ", replacing the value kept now" } else { "" }
    );
    let asked = put(
        session.as_deref().unwrap_or(""),
        &title,
        &text,
        vec!["Keep".into(), "No".into()],
    );
    let project = match &owner {
        Owner::Project(k) => k.clone(),
        o => o.label(),
    };
    let session = session.unwrap_or_default();
    thread::spawn(move || {
        let pressed = asked.pressed(SET_WAIT);
        let at = now();
        if pressed != Some(Some(0)) {
            store::log(&log_line(at, &project, &session, &name, "-", "set refused"));
            reply.send(json!({ "error": format!("{name} was not kept: the human did not say yes in Horadric.") }));
            return;
        }
        if !reply.wanted() {
            return;
        }
        let kept = store::change(|v| {
            match v.secrets.iter_mut().find(|s| s.name == name && s.owner == owner) {
                Some(s) => s.value = value,
                None => v.secrets.push(store::Secret {
                    name: name.clone(),
                    owner: owner.clone(),
                    value,
                    allowed: Vec::new(),
                    last_used: None,
                }),
            }
        });
        match kept {
            Ok(()) => {
                store::log(&log_line(at, &project, &session, &name, "-", "set"));
                reply.send(json!({ "text": format!("{name} is kept for {whose}.") }));
            }
            Err(e) => reply.send(json!({ "error": format!("{name} was not kept: {e}") })),
        }
    });
}

/// `horadric secret list`: the names a session may use, never a value.
pub fn names(place: Option<Place>, req: SecretNames) {
    if req.session.is_empty() || !key_ok(&req.session, &req.key) {
        req.reply.send(json!({ "error": "Refused: this is not a live Horadric session with its own key." }));
        return;
    }
    let Some(place) = place else {
        req.reply.send(json!({ "error": "Horadric does not know this session's project." }));
        return;
    };
    let identity = identity_slug(&place.dir);
    let (secrets, logins) = store::names(&place.key, identity.as_deref());
    let mut all: Vec<String> = secrets.into_iter().map(|(n, _)| n).collect();
    all.extend(logins.into_iter().map(|(n, _)| n));
    all.sort();
    all.dedup();
    req.reply.send(json!({ "names": all }));
}
