//! `horadric secret run`, `set` and `list` in the app: the key each session
//! is born with, the question to the human, and the program started with
//! the values in its environment and its output masked on the way back.
//! The values never leave the app. See docs/plan/vault.md.

use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{c_void, OsString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use horadric_core::vault::secret::{
    always_refused, approval, approved, changed_pins, command_path, fingerprint, git_config_parameters,
    git_key_refused, git_refs, git_version, gh_helper, log_line, GitRefs,
    missing_line, npm_shim, once_only, pinned, plainly, runs_builds, valid_name, Mask, Owner, Pin,
    PinKind,
};
use horadric_hooks::listener::{SecretNames, SecretRun, SecretSet, SecretStream};
use serde_json::json;
use windows::core::{w, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Security::Cryptography::{
    BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash, BCryptHashData, BCRYPT_HASH_HANDLE,
    BCRYPT_SHA256_ALG_HANDLE,
};
use windows::Win32::Security::{
    AccessCheck, DuplicateToken, GetFileSecurityW, SecurityImpersonation, GENERIC_MAPPING,
    PRIVILEGE_SET, PSECURITY_DESCRIPTOR, TOKEN_DUPLICATE, TOKEN_QUERY,
};
use windows::Win32::Storage::FileSystem::{GetFinalPathNameByHandleW, FILE_NAME_NORMALIZED};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcessToken, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, FindWindowW, KillTimer, PostMessageW, RegisterClassW,
    SetTimer, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WNDCLASSW,
};

use super::store;
use crate::dialog::{Dialog, Tone};

/// How long a `secret run` waits for the human before it fails with the
/// line that tells the agent to block the quest instead.
const WAIT: Duration = Duration::from_secs(60);

/// How long a `secret set` waits: the human typed it, so is at the
/// machine, but may have to find the app's window first.
const SET_WAIT: Duration = Duration::from_secs(120);

/// The longest command line put to the human. Longer is refused rather
/// than cut: a question that hides part of what runs is no question.
const SHOWN_MAX: usize = 1500;

/// How long the output is still read after the program and its tree have
/// ended, for what was in the pipes.
const DRAIN: Duration = Duration::from_secs(2);

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_SUSPENDED: u32 = 0x0000_0004;

/// Each live session's key, by session id, as `keys.dat` keeps them: a
/// session reattached after the app restarts still holds the key it was
/// born with, so the app must still know it. None until first read.
static KEYS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn keys_path() -> Option<PathBuf> {
    Some(horadric_hooks::state_dir()?.join("keys.dat"))
}

/// Set when `keys.dat` is there but would not open or read: it is kept as
/// it is, for whatever made it unreadable may pass (a profile not yet
/// loaded, a scanner), and new keys live in memory only until a restart.
static KEYS_UNREADABLE: AtomicBool = AtomicBool::new(false);

/// The keys, read from `keys.dat` the first time. No file is no keys; a
/// file that will not open is logged and never written over.
fn with_keys<R>(f: impl FnOnce(&mut HashMap<String, String>) -> R) -> R {
    let mut keys = KEYS.lock().unwrap_or_else(|e| e.into_inner());
    let keys = keys.get_or_insert_with(|| {
        let Some(path) = keys_path() else {
            return HashMap::new();
        };
        let read = store::read_sealed(&path).and_then(|b| match b {
            None => Ok(HashMap::new()),
            Some(b) => serde_json::from_slice(&b).map_err(|e| e.to_string()),
        });
        read.unwrap_or_else(|e| {
            eprintln!(
                "horadric: cannot read {}: {e}; it is left as it is and new session keys last until a restart",
                path.display()
            );
            KEYS_UNREADABLE.store(true, Ordering::Relaxed);
            HashMap::new()
        })
    });
    f(keys)
}

/// A key that misses the file still works until the app restarts, so a
/// failed write stops nothing; it is logged, and tried once more since
/// the usual cause (a scanner holding the file) passes in a moment.
fn save_keys(keys: &HashMap<String, String>) {
    if KEYS_UNREADABLE.load(Ordering::Relaxed) {
        return;
    }
    let (Some(path), Ok(bytes)) = (keys_path(), serde_json::to_vec(keys)) else {
        return;
    };
    if let Err(e) = store::write_sealed(&path, &bytes) {
        eprintln!("horadric: cannot write {}: {e}, trying again", path.display());
        thread::sleep(Duration::from_millis(100));
        if let Err(e) = store::write_sealed(&path, &bytes) {
            eprintln!(
                "horadric: cannot write {}: {e}; sessions started now lose their secrets on a restart",
                path.display()
            );
        }
    }
}

/// A new random key for a session, to go into its environment as
/// `HORADRIC_SECRET_KEY` when it starts.
pub fn mint_key(session: &str) -> String {
    let mut bytes = [0u8; 32];
    crate::portal_crypto::random(&mut bytes);
    let key: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    with_keys(|keys| {
        keys.insert(session.to_string(), key.clone());
        save_keys(keys);
    });
    key
}

/// Forgets a session's key, once the session has ended.
pub fn drop_key(session: &str) {
    with_keys(|keys| {
        if keys.remove(session).is_some() {
            save_keys(keys);
        }
    });
}

/// Whether `key` is the live session's own. Compared in constant time, so
/// the time an answer takes says nothing about how much of a guess was
/// right.
pub fn key_ok(session: &str, key: &str) -> bool {
    with_keys(|keys| {
        let Some(kept) = keys.get(session) else {
            return false;
        };
        let (a, b) = (kept.as_bytes(), key.as_bytes());
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
    })
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

/// How a wait for the human ended.
enum Waited {
    Pressed(Option<usize>),
    Unanswered,
    /// The CLI that asked went away first.
    Left,
}

impl Asked {
    /// The human's answer, or None when nobody answered within `within`;
    /// the question is then taken off the screen. Closing it is No.
    pub fn wait(self, within: Duration) -> Option<Answer> {
        let always = self.always;
        match self.until(within, None) {
            Waited::Pressed(p) => Some(answer(p, always)),
            _ => None,
        }
    }

    /// Waits for the button pressed, giving up early when `left` is set.
    fn until(self, within: Duration, left: Option<&AtomicBool>) -> Waited {
        let end = Instant::now() + within;
        loop {
            let now = Instant::now();
            if now >= end {
                break;
            }
            match self.rx.recv_timeout((end - now).min(Duration::from_millis(250))) {
                Ok(p) => return Waited::Pressed(p),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if left.is_some_and(|l| l.load(Ordering::Relaxed)) {
                self.take_down();
                return Waited::Left;
            }
        }
        self.take_down();
        Waited::Unanswered
    }

    fn take_down(&self) {
        self.gone.store(true, Ordering::Relaxed);
        close_if_shown(self.id, &self.title);
    }
}

fn answer(pressed: Option<usize>, always: bool) -> Answer {
    match (pressed, always) {
        (Some(0), _) => Answer::Once,
        (Some(1), true) => Answer::Always,
        _ => Answer::No,
    }
}

/// Asks the human Once, Always (only when `always`) or No. Call it from
/// any thread, inside the app's borrow or not: the question opens on the
/// UI thread once the current message is handled, outside the borrow, as
/// a modal dialog whose loop keeps the app running, and one at a time.
/// Wait for the answer with [`Asked::wait`] on another thread. The first
/// call must come from the UI thread, which [`serve`] and [`set`] do.
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
    /// For a first button that leads to a masked field: its prompt, and
    /// where what the human typed goes, before the button is answered.
    masked: Option<(String, Sender<Option<String>>)>,
}

/// The questions not yet shown. Shared, since a worker thread may ask.
static QUEUE: Mutex<VecDeque<Question>> = Mutex::new(VecDeque::new());

thread_local! {
    static ASKING: Cell<bool> = const { Cell::new(false) };
}

/// The question on screen now, so a timed out wait closes only its own.
static SHOWING: Mutex<Option<u64>> = Mutex::new(None);
static NEXT: AtomicU64 = AtomicU64::new(1);

/// A message window of the UI thread, for a worker to say a question
/// waits: a timer can only be set for the thread that sets it.
static WAKER: AtomicIsize = AtomicIsize::new(0);
const WM_QUESTION: u32 = WM_APP + 0x5a;

/// Makes the waker if there is none yet. Call it on the UI thread.
fn waker() {
    if WAKER.load(Ordering::Acquire) != 0 {
        return;
    }
    unsafe {
        let Ok(instance) = GetModuleHandleW(None) else {
            return;
        };
        let class = w!("HoradricVaultQuestions");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(waker_proc),
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        if let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        ) {
            WAKER.store(hwnd.0 as isize, Ordering::Release);
        }
    }
}

unsafe extern "system" fn waker_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_QUESTION {
        soon(0);
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn put(session: &str, title: &str, text: &str, buttons: Vec<String>) -> Asked {
    put_with(session, title, text, buttons, None)
}

fn put_with(
    _session: &str,
    title: &str,
    text: &str,
    buttons: Vec<String>,
    masked: Option<(String, Sender<Option<String>>)>,
) -> Asked {
    let (tx, rx) = mpsc::channel();
    let gone = Arc::new(AtomicBool::new(false));
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    QUEUE.lock().unwrap_or_else(|e| e.into_inner()).push_back(Question {
        id,
        title: title.to_string(),
        text: text.to_string(),
        buttons,
        tx,
        gone: Arc::clone(&gone),
        tries: 0,
        masked,
    });
    let hwnd = WAKER.load(Ordering::Acquire);
    if hwnd == 0 {
        // Only ever the UI thread, before any worker asked.
        waker();
        soon(0);
    } else {
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut c_void)), WM_QUESTION, WPARAM(0), LPARAM(0));
        }
    }
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

fn next_question() -> Option<Question> {
    QUEUE.lock().unwrap_or_else(|e| e.into_inner()).pop_front()
}

unsafe extern "system" fn show_queued(_: HWND, _: u32, id: usize, _: u32) {
    let _ = unsafe { KillTimer(None, id) };
    if ASKING.with(Cell::get) {
        // The loop already running takes this one when its own is answered.
        return;
    }
    ASKING.with(|a| a.set(true));
    while let Some(mut q) = next_question() {
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
            QUEUE.lock().unwrap_or_else(|e| e.into_inner()).push_front(q);
            soon(500);
            break;
        }
        if let (Some(0), Some((prompt, typed))) = (pressed, &q.masked) {
            let _ = typed.send(crate::app::ask_secret(&q.title, prompt, "keep it"));
        }
        // A question that was never shown has no answer, not a No: its
        // asker hears the line hang up and logs it as unanswered.
        if let Some(pressed) = pressed {
            let _ = q.tx.send(Some(pressed));
        }
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

/// The sessions with a question open, so one session cannot pile
/// questions up until the human presses yes to make them stop. `""`
/// stands for every terminal that is no session's.
static OPEN: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// A session's one open question, given back when dropped.
struct Claim(String);

impl Drop for Claim {
    fn drop(&mut self) {
        OPEN.lock().unwrap_or_else(|e| e.into_inner()).retain(|s| *s != self.0);
    }
}

fn claim(session: &str) -> Option<Claim> {
    let mut open = OPEN.lock().unwrap_or_else(|e| e.into_inner());
    if open.iter().any(|s| s == session) {
        return None;
    }
    open.push(session.to_string());
    Some(Claim(session.to_string()))
}

const ALREADY_ASKING: &str =
    "Refused: a question is already open in Horadric for this session. Wait for the human to answer it.";

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn identity_slug(dir: &Path) -> Option<String> {
    super::identity::of_project(dir).slug().map(str::to_string)
}

/// A secret as the human reads it, with whose it is: "Optipeople's
/// GITHUB_TOKEN".
fn owned(name: &str, owner: &Owner) -> String {
    match owner {
        Owner::Project(key) => format!("{}'s {name}", crate::project::project_name(key)),
        Owner::Identity(slug) => {
            let who = super::identity::get(slug).map_or_else(|| slug.clone(), |i| i.name);
            format!("{who}'s {name}")
        }
        Owner::Everywhere => format!("{name} (kept for every project)"),
    }
}

/// A session's name as its tile shows it and the folder it works in, from
/// the state file, for the human to know who asks. None when the file has
/// not got it yet.
fn session_seen(id: &str) -> Option<(String, String)> {
    let path = horadric_hooks::state_dir()?.join("state.json");
    let bytes = std::fs::read(path).ok()?;
    let state = horadric_core::saved::SavedState::from_json(&bytes);
    let s = state.sessions.iter().find(|s| s.id == id)?;
    let label = s.to_session(SystemTime::now()).label().to_string();
    Some((label, s.cwd.clone()))
}

/// Who asks, for the question: "session “Fix the build” of Horadric".
fn asker(session: &str, project: Option<&str>) -> String {
    if session.is_empty() {
        return "A terminal that is not a Horadric session".to_string();
    }
    let seen = session_seen(session);
    let label = seen
        .as_ref()
        .map_or_else(|| session.get(..8).unwrap_or(session).to_string(), |(l, _)| l.clone());
    let project = project.map(str::to_string).or_else(|| {
        seen.map(|(_, cwd)| crate::project::project_name(&crate::project::folder_key(&cwd)))
    });
    match project {
        Some(p) => format!("Session \u{201c}{label}\u{201d} of {p}"),
        None => format!("Session \u{201c}{label}\u{201d}"),
    }
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

/// A path that names a share or a device, `\\server\share`, `\\?\UNC\...`,
/// `\\.\pipe\...` and the like, which a secret never runs from or in.
/// A verbatim local path, `\\?\C:\...`, is neither.
fn remote_or_device(p: &str) -> bool {
    let p = p.replace('/', "\\");
    match p.strip_prefix(r"\\?\") {
        Some(rest) => {
            let b = rest.as_bytes();
            !(b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\')
        }
        None => p.starts_with(r"\\"),
    }
}

/// Why a program as typed is refused before it is looked for, None when
/// it may be. A `:` anywhere but after a full path's drive letter is a
/// drive relative path (`C:gh.exe`) or a stream (`gh.exe:x`).
fn refused_program(program: &str) -> Option<String> {
    if remote_or_device(program) {
        return Some(format!(
            "Refused: {program} is on a share or a device. Run a program on this PC."
        ));
    }
    let drive = program.len() >= 3
        && program.as_bytes()[0].is_ascii_alphabetic()
        && program.as_bytes()[1] == b':'
        && matches!(program.as_bytes()[2], b'\\' | b'/');
    let colons = program.matches(':').count();
    if colons > usize::from(drive) || (colons == 1 && !drive) {
        return Some(format!(
            "Refused: {program} is not a program's name or full path. Give it by name, found on PATH, or as C:\\full\\path.exe."
        ));
    }
    None
}

/// The program's full path: an absolute path as given, else found on the
/// app's `PATH`, never in the current folder, where Windows looks first
/// and an agent can drop a `gh.cmd`.
fn program_path(program: &str) -> Result<PathBuf, String> {
    if let Some(why) = refused_program(program) {
        return Err(why);
    }
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

/// The program as it will run: opened so nobody can change or swap it
/// until the run ends, its canonical long path (no 8.3 names, links
/// followed), and the SHA-256 of what is in it.
struct Pinned {
    /// Held for the whole run: no write and no delete or rename share.
    _file: File,
    path: PathBuf,
    /// On a share, by way of a mapped drive or a link.
    remote: bool,
    sha256: String,
}

fn pin(program: &Path) -> Result<Pinned, String> {
    const FILE_SHARE_READ: u32 = 0x1;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(program)
        .map_err(|e| format!("cannot open {}: {e}", program.display()))?;
    let mut buf = vec![0u16; 1024];
    let mut len;
    loop {
        len = unsafe {
            GetFinalPathNameByHandleW(HANDLE(file.as_raw_handle()), &mut buf, FILE_NAME_NORMALIZED)
        } as usize;
        if len == 0 {
            return Err(format!("cannot tell where {} really is.", program.display()));
        }
        if len < buf.len() {
            break;
        }
        buf.resize(len + 1, 0);
    }
    let full = OsString::from_wide(&buf[..len]).to_string_lossy().into_owned();
    let (path, remote) = if let Some(unc) = full.strip_prefix("\\\\?\\UNC\\") {
        (format!("\\\\{unc}"), true)
    } else {
        (full.strip_prefix("\\\\?\\").unwrap_or(&full).to_string(), false)
    };
    let sha256 = hash(&mut file).ok_or_else(|| format!("cannot read {path}."))?;
    Ok(Pinned {
        _file: file,
        path: PathBuf::from(path),
        remote,
        sha256,
    })
}

/// SHA-256 of everything in `file`, through CNG, a piece at a time.
fn hash(file: &mut impl Read) -> Option<String> {
    let mut h = BCRYPT_HASH_HANDLE::default();
    unsafe { BCryptCreateHash(BCRYPT_SHA256_ALG_HANDLE, &mut h, None, None, 0) }
        .ok()
        .ok()?;
    // Small, as a package of thousands of files is hashed a file at a time.
    let mut buf = vec![0u8; 1 << 16];
    let mut out = [0u8; 32];
    let done = loop {
        match file.read(&mut buf) {
            Ok(0) => break unsafe { BCryptFinishHash(h, &mut out, 0) }.is_ok(),
            Ok(n) => {
                if unsafe { BCryptHashData(h, &buf[..n], 0) }.is_err() {
                    break false;
                }
            }
            Err(_) => break false,
        }
    };
    let _ = unsafe { BCryptDestroyHash(h) };
    done.then(|| out.iter().map(|b| format!("{b:02x}")).collect())
}

/// Whether this user, as the app runs, holds any of `rights` on `path`.
/// Anything that cannot be checked counts as yes, which only takes Always
/// away.
fn may(path: &Path, rights: u32) -> bool {
    const OWNER_GROUP_DACL: u32 = 0x1 | 0x2 | 0x4;
    const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let name = PCWSTR(wide.as_ptr());
        let mut need = 0u32;
        let _ = GetFileSecurityW(name, OWNER_GROUP_DACL, None, 0, &mut need);
        if need == 0 {
            return true;
        }
        let mut sd = vec![0u8; need as usize];
        let sdp = PSECURITY_DESCRIPTOR(sd.as_mut_ptr().cast());
        if !GetFileSecurityW(name, OWNER_GROUP_DACL, Some(sdp), need, &mut need).as_bool() {
            return true;
        }
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY | TOKEN_DUPLICATE, &mut token).is_err() {
            return true;
        }
        let token = OwnedHandle::from_raw_handle(token.0);
        let mut imp = HANDLE::default();
        if DuplicateToken(HANDLE(token.as_raw_handle()), SecurityImpersonation, &mut imp).is_err() {
            return true;
        }
        let imp = OwnedHandle::from_raw_handle(imp.0);
        let mapping = GENERIC_MAPPING {
            GenericRead: 0x0012_0089,
            GenericWrite: 0x0012_0116,
            GenericExecute: 0x0012_00a0,
            GenericAll: 0x001f_01ff,
        };
        let mut privileges = [0u8; 256];
        let mut privileges_len = privileges.len() as u32;
        let mut granted = 0u32;
        let mut ok = BOOL(0);
        if AccessCheck(
            sdp,
            HANDLE(imp.as_raw_handle()),
            MAXIMUM_ALLOWED,
            &mapping,
            Some(privileges.as_mut_ptr().cast::<PRIVILEGE_SET>()),
            &mut privileges_len,
            &mut granted,
            &mut ok,
        )
        .is_err()
        {
            return true;
        }
        ok.as_bool() && granted & rights != 0
    }
}

/// Whether this user could change the program, or plant something beside
/// it, without elevation: then so could an agent, and Always would hand
/// the secret to whatever it put there.
fn changeable(program: &Path) -> bool {
    const FILE_WRITE_DATA: u32 = 0x2;
    const FILE_APPEND_DATA: u32 = 0x4;
    const DELETE: u32 = 0x1_0000;
    const WRITE_DAC: u32 = 0x4_0000;
    const WRITE_OWNER: u32 = 0x8_0000;
    let file = FILE_WRITE_DATA | FILE_APPEND_DATA | DELETE | WRITE_DAC | WRITE_OWNER;
    may(program, file) || program.parent().is_none_or(writable_dir)
}

/// The hooks folder an Always run's git is pointed at: the app's own,
/// empty, read-only. Checked empty right before each start; a hook put in
/// it between that and git looking still runs, a window of the start alone,
/// and only for a process of this user that knew to aim at it.
fn no_hooks() -> Result<PathBuf, String> {
    let dir = horadric_hooks::state_dir()
        .ok_or("cannot find Horadric's own folder")?
        .join("no-hooks");
    if !dir.is_dir() {
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    }
    let empty = std::fs::read_dir(&dir).map(|mut d| d.next().is_none()).unwrap_or(false);
    if !empty {
        return Err(format!(
            "Refused: {} should be empty and is not, so git could run what is in it.",
            dir.display()
        ));
    }
    if let Ok(m) = std::fs::metadata(&dir) {
        let mut p = m.permissions();
        if !p.readonly() {
            p.set_readonly(true);
            let _ = std::fs::set_permissions(&dir, p);
        }
    }
    Ok(dir)
}

/// The attributes file an Always run's git is pointed at, beside
/// [`no_hooks`] and kept empty the same way, so the user's own global one
/// names no filter or driver either.
fn no_attributes() -> Result<PathBuf, String> {
    let file = horadric_hooks::state_dir()
        .ok_or("cannot find Horadric's own folder")?
        .join("no-attributes");
    if !file.exists() {
        std::fs::write(&file, b"").map_err(|e| format!("cannot make {}: {e}", file.display()))?;
    }
    let empty = std::fs::symlink_metadata(&file).is_ok_and(|m| m.is_file() && m.len() == 0);
    if !empty {
        return Err(format!(
            "Refused: {} should be an empty file and is not, so git could run what it names.",
            file.display()
        ));
    }
    Ok(file)
}

/// What an Always run of a pinned CLI never inherits: ways for the
/// environment to name a program for git or ssh to run, or to send its
/// traffic and token somewhere else.
const QUIET_REMOVED: &[&str] = &[
    "GIT_SSH_COMMAND",
    "GIT_SSH",
    "GIT_SSH_VARIANT",
    "GIT_PROXY_COMMAND",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_EXTERNAL_DIFF",
    "GIT_EXEC_PATH",
    "GIT_TEMPLATE_DIR",
    "GIT_SSL_NO_VERIFY",
    "GIT_SSL_CAINFO",
    "GIT_SSL_CAPATH",
    "GIT_CURL_VERBOSE",
    "GIT_REDACT_COOKIES",
    "GIT_CONFIG_SYSTEM",
];

/// `GIT_CONFIG_COUNT` and its `GIT_CONFIG_KEY_n` and `GIT_CONFIG_VALUE_n`.
fn numbered_git_config(name: &str) -> bool {
    let up = name.to_ascii_uppercase();
    up == "GIT_CONFIG_COUNT" || up.starts_with("GIT_CONFIG_KEY_") || up.starts_with("GIT_CONFIG_VALUE_")
}

/// Whether a variable the run would inherit is taken away from it: the
/// fixed list, every `GIT_TRACE*`, and numbered settings.
fn quiet_removed(name: &str) -> bool {
    let up = name.to_ascii_uppercase();
    QUIET_REMOVED.contains(&up.as_str()) || up.starts_with("GIT_TRACE") || numbered_git_config(&up)
}

/// The variables, as the child will have them, that change where its
/// traffic goes or whom it trusts: every `GIT_*` it keeps, proxies and
/// CA bundles. Each goes into the fingerprint by name and SHA-256, so one
/// set with `setx` since the Always asks again and the question names it.
fn watched_env(env: &super::identity::SessionEnv) -> Vec<(String, String)> {
    const NAMED: &[&str] = &[
        "CURL_CA_BUNDLE",
        "NODE_EXTRA_CA_CERTS",
        "NODE_TLS_REJECT_UNAUTHORIZED",
        "REQUESTS_CA_BUNDLE",
        "AWS_CA_BUNDLE",
    ];
    let watched = |k: &str| {
        let up = k.to_ascii_uppercase();
        (up.starts_with("GIT_") && !quiet_removed(&up))
            || up.ends_with("_PROXY")
            || up.starts_with("SSL_CERT_")
            || NAMED.contains(&up.as_str())
    };
    let mut names: Vec<String> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .chain(env.set.iter().map(|(k, _)| k.clone()))
        .filter(|k| watched(k))
        .map(|k| k.to_ascii_uppercase())
        .collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .filter_map(|n| {
            let v = child_var(env, &n)?;
            let h = hash(&mut v.as_bytes())?;
            let n = n.replace([',', '=', '|'], "_");
            Some((format!("the variable {n}"), h))
        })
        .collect()
}

/// The tree git knows with no file in it, as the source of attributes:
/// then no `.gitattributes` in the project, or in a submodule, can name a
/// filter or a diff or merge driver for git to run.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Whether the git a CLI would find reads `GIT_ATTR_SOURCE`, which came in
/// git 2.40. Asked once; no git at all is fine, since nothing runs one.
fn git_takes_attr_source() -> bool {
    static KNOWN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *KNOWN.get_or_init(|| {
        let Some(git) = horadric_pty::find_program("git", &app_path(), &[".exe"], Path::is_file) else {
            return true;
        };
        horadric_hooks::no_window(&mut Command::new(git))
            .arg("--version")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .and_then(|o| git_version(&String::from_utf8_lossy(&o.stdout)))
            .is_some_and(|v| v >= (2, 40))
    })
}

/// Whether this user could put a file in `dir` without elevation.
fn writable_dir(dir: &Path) -> bool {
    const WRITE_DAC: u32 = 0x4_0000;
    const WRITE_OWNER: u32 = 0x8_0000;
    const FILE_ADD_FILE: u32 = 0x2;
    const FILE_ADD_SUBDIRECTORY: u32 = 0x4;
    const FILE_DELETE_CHILD: u32 = 0x40;
    may(dir, FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY | FILE_DELETE_CHILD | WRITE_DAC | WRITE_OWNER)
}

/// A `.dll` at the top of the folder the program runs in, where Windows
/// looks for a library loaded by bare name once its own and the system
/// folders have none: the agent could plant one there for a program that
/// has Always.
fn loose_dll(cwd: &Path) -> Option<String> {
    std::fs::read_dir(cwd).ok()?.flatten().find_map(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        let dll = name.to_ascii_lowercase().ends_with(".dll") && e.path().is_file();
        dll.then_some(name)
    })
}

/// The PATH an Always run gets: of the child's own, only absolute folders
/// this user cannot write to and that are outside the project, so a
/// helper or library looked up by name is one the agent could not have
/// put there. A shim's node folder goes first, its node pinned already.
fn locked_path(own: Option<String>, node: Option<&Path>, place: &[&Path]) -> (OsString, Vec<String>) {
    let own = own.map(OsString::from).unwrap_or_else(app_path);
    let mut kept: Vec<PathBuf> = node.and_then(Path::parent).map(Path::to_path_buf).into_iter().collect();
    let mut left = Vec::new();
    for d in std::env::split_paths(&own).filter(|d| !d.as_os_str().is_empty()) {
        let why = if !d.is_absolute() {
            Some("a relative folder")
        } else if !d.is_dir() {
            None
        } else if place.iter().any(|p| inside(&d, p)) {
            Some("inside the project or temp")
        } else if writable_dir(&d) {
            Some("you can write to it")
        } else {
            kept.push(d.clone());
            None
        };
        if let Some(why) = why {
            left.push(format!("{} ({why})", d.display()));
        }
    }
    (std::env::join_paths(kept).unwrap_or_default(), left)
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

/// What an npm shim runs, which Horadric runs itself: node, by its full
/// path, with the package's script. The shim's own text is never run by
/// cmd, so neither its parsing nor a `node` found in the project folder
/// comes into it.
struct ShimRun {
    node: PathBuf,
    script: PathBuf,
    package: PathBuf,
    /// The global `node_modules` the package sits in, where node looks
    /// for any module the package does not hold itself.
    modules: PathBuf,
}

/// The npm shim `program` is, None for anything else. Its node is the
/// one beside it, as the shim would take, else the first on PATH.
fn shim_of(program: &Path) -> Option<ShimRun> {
    let ext = program.extension()?.to_string_lossy().to_ascii_lowercase();
    if ext != "cmd" {
        return None;
    }
    let shim = npm_shim(&std::fs::read_to_string(program).ok()?)?;
    let dir = program.parent()?;
    let package = dir.join(&shim.package);
    let script = dir.join(&shim.script);
    if !package.is_dir() || !script.is_file() {
        return None;
    }
    let beside = dir.join("node.exe");
    let node = if beside.is_file() {
        beside
    } else {
        horadric_pty::find_program("node", &app_path(), &[".exe"], Path::is_file)?
    };
    Some(ShimRun {
        node,
        script,
        package,
        modules: dir.join("node_modules"),
    })
}

/// The variable `name` as the child will have it: the identity's own
/// first, then the app's unless the identity takes it away.
fn child_var(env: &super::identity::SessionEnv, name: &str) -> Option<String> {
    if let Some((_, v)) = env.set.iter().rev().find(|(k, _)| k.eq_ignore_ascii_case(name)) {
        return Some(v.clone());
    }
    if env.remove.iter().any(|k| k.eq_ignore_ascii_case(name)) {
        return None;
    }
    std::env::var(name).ok()
}

/// What runs when the program does, beyond its own file: what is pinned
/// for it, the content of all of it, and what its arguments ask.
struct Runs {
    shim: Option<ShimRun>,
    /// The program is one [`pinned`] knows, or an npm shim.
    pinned: bool,
    pins: Vec<Pin>,
    /// The program's SHA-256 and each pin's, as an Always keeps it; None
    /// when a pin could not be read, which leaves only Once.
    fingerprint: Option<String>,
    /// The program's name as the human knows it: `vercel`, not `vercel.cmd`.
    stem: String,
    /// The subcommand an Always is for: `pr create`.
    command: String,
    /// Why Always is not offered for reasons of its own: its arguments,
    /// a git include too deep to pin, a node folder outside the package.
    refused: Option<String>,
    /// The repository whose git config was pinned, the only one an Always
    /// run's git may use.
    git_repo: Option<GitRepo>,
}

impl Runs {
    fn of(program: &Pinned, env: &super::identity::SessionEnv, args: &[String], cwd: &Path) -> Runs {
        let shim = shim_of(&program.path);
        let path = program.path.to_string_lossy().into_owned();
        let var = |k: &str| child_var(env, k);
        let mut pins = pinned(&path, &var);
        let mut refused = None;
        if let Some(s) = &shim {
            let name = s
                .package
                .strip_prefix(&s.modules)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default()
                .replace([',', '=', '|'], "_");
            pins.get_or_insert_with(Vec::new).extend([
                Pin {
                    what: format!("the {name} package"),
                    path: s.package.to_string_lossy().into_owned(),
                    kind: PinKind::Tree,
                },
                Pin {
                    what: "node.exe".into(),
                    path: s.node.to_string_lossy().into_owned(),
                    kind: PinKind::File,
                },
                // A module the package lacks is looked for here next, so
                // one planted here would load.
                Pin {
                    what: "the global npm packages".into(),
                    path: s.modules.to_string_lossy().into_owned(),
                    kind: PinKind::Names,
                },
            ]);
            refused = node_escape(s, &var);
        }
        let is_pinned = pins.is_some();
        let mut pins = pins.unwrap_or_default();
        let mut git_repo = None;
        if is_pinned {
            match git_pins(cwd, &var) {
                Ok((more, repo)) => {
                    pins.extend(more);
                    git_repo = repo;
                }
                Err(why) => refused = refused.or(Some(why)),
            }
        }
        let stem = program
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let refused = refused.or_else(|| always_refused(&stem, args));
        let fingerprint = pin_hashes(&pins).map(|mut h| {
            if is_pinned {
                h.extend(watched_env(env));
            }
            fingerprint(&program.sha256, &h)
        });
        Runs {
            shim,
            pinned: is_pinned,
            pins,
            fingerprint,
            command: command_path(args),
            stem,
            refused,
            git_repo,
        }
    }

    /// The program, subcommand and input an Always is kept for: a value
    /// written to its input is a use of its own, so an Always without
    /// `--stdin`, or with other names, does not cover one with.
    fn bound(&self, program_lower: &str, stdin: &[String]) -> String {
        let mut out = program_lower.to_string();
        if !self.command.is_empty() {
            out.push_str(&format!(" {}", self.command));
        }
        if !stdin.is_empty() {
            let mut names = stdin.to_vec();
            names.sort();
            names.dedup();
            out.push_str(&format!(" <stdin {}>", names.join(" ")));
        }
        out
    }
}

/// Why node could load a module from outside the pinned package: the
/// folders it looks in above the global one, and the home ones. NODE_PATH
/// is taken out of the run, so it is not one.
fn node_escape(shim: &ShimRun, var: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let homes = ["USERPROFILE", "HOME"].into_iter().filter_map(var);
    for home in homes {
        for dir in [".node_modules", ".node_libraries"] {
            let p = Path::new(&home).join(dir);
            if p.exists() {
                return Some(format!("{} exists, and node loads modules from it", p.display()));
            }
        }
    }
    // A scoped package looks in its scope's own node_modules before the
    // global one.
    if let Some(scope) = shim.package.parent().filter(|p| *p != shim.modules) {
        let p = scope.join("node_modules");
        if p.exists() {
            return Some(format!("{} exists, and node loads modules from it", p.display()));
        }
    }
    let top = shim.modules.parent()?;
    top.ancestors()
        .skip(1)
        .map(|a| a.join("node_modules"))
        .find(|p| p.exists())
        .map(|p| format!("{} exists, and node loads modules from it", p.display()))
}

/// Where `p`, as a git config writes it, points: `~/` from home, else
/// from `base` when relative.
fn git_path(p: &str, base: &Path, var: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        return Some(Path::new(&var("USERPROFILE").or_else(|| var("HOME"))?).join(rest));
    }
    let path = Path::new(p);
    Some(if path.is_absolute() { path.to_path_buf() } else { base.join(path) })
}

/// What git reads that decides what it runs, for a CLI that runs git in
/// `cwd`: the repository's config, hooks and attributes (filters run
/// commands), the user's own config and what each includes. An include
/// that includes again is not followed: Err, and only Once.
/// The repository [`git_pins`] pinned: its git folder (a worktree's own,
/// which names the common one) and the top of its work tree.
#[derive(Clone, Debug, PartialEq)]
struct GitRepo {
    dir: PathBuf,
    top: PathBuf,
}

/// Err naming the first setting of `file` that is not known to be data,
/// since git could run a program from it with the token in reach. gh's
/// own credential helper passes when its gh is one the agent could not
/// have put there.
fn git_keys_ok(refs: &GitRefs, file: &Path) -> Result<(), String> {
    for k in &refs.keys {
        let helper = (k.section == "credential" && k.name == "helper")
            .then(|| gh_helper(&k.value))
            .flatten();
        let fine = match helper {
            Some(gh) => {
                let path = if Path::new(&gh).is_absolute() {
                    Some(PathBuf::from(&gh))
                } else {
                    horadric_pty::find_program(&gh, &app_path(), &["", ".exe"], Path::is_file)
                };
                path.is_some_and(|p| p.is_file() && !changeable(&p))
            }
            None => !git_key_refused(k),
        };
        if !fine {
            return Err(format!(
                "{} in {} can make git run a program",
                plainly(&k.shown()),
                file.display()
            ));
        }
    }
    Ok(())
}

/// Where an Always run's git may look for a repository: the one pinned,
/// by its folders, so no discovery can land on another; with none, no
/// folder above the run's, so it finds none. What to set, and what of the
/// app's own to take away.
fn git_place(repo: Option<&GitRepo>, cwd: &Path) -> (Vec<(&'static str, String)>, Vec<&'static str>) {
    let shown = |p: &Path| p.to_string_lossy().into_owned();
    match repo {
        // A worktree's own git folder names the common one, so
        // GIT_COMMON_DIR is left to git.
        Some(r) => (
            vec![("GIT_DIR", shown(&r.dir)), ("GIT_WORK_TREE", shown(&r.top))],
            vec!["GIT_COMMON_DIR", "GIT_INDEX_FILE", "GIT_CEILING_DIRECTORIES", "GIT_OBJECT_DIRECTORY"],
        ),
        None => {
            let ceilings: Vec<String> = cwd.ancestors().skip(1).map(shown).collect();
            (
                vec![("GIT_CEILING_DIRECTORIES", ceilings.join(";"))],
                vec!["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY"],
            )
        }
    }
}

/// Whether `dir` is a git folder git would take: HEAD in it, objects and
/// refs in it or in the common folder it names.
fn valid_git_dir(dir: &Path, common: &Path) -> bool {
    dir.join("HEAD").is_file() && common.join("objects").is_dir() && common.join("refs").is_dir()
}

fn git_pins(cwd: &Path, var: &dyn Fn(&str) -> Option<String>) -> Result<(Vec<Pin>, Option<GitRepo>), String> {
    let file = |what: &str, path: &Path| Pin {
        what: what.to_string(),
        path: path.to_string_lossy().into_owned(),
        kind: PinKind::File,
    };
    let home = var("USERPROFILE").or_else(|| var("HOME"));
    let mut configs: Vec<(String, PathBuf)> = Vec::new();
    match var("GIT_CONFIG_GLOBAL") {
        Some(g) => configs.push(("your global git config".into(), PathBuf::from(g))),
        None => {
            if let Some(h) = &home {
                configs.push(("your global git config".into(), Path::new(h).join(".gitconfig")));
            }
        }
    }
    let xdg = var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| Path::new(h).join(".config")));
    if let Some(x) = xdg {
        configs.push(("your git config in .config".into(), x.join("git").join("config")));
    }
    // Git takes the first folder up that has a `.git`, or that is itself
    // a repository's folder (a bare one, or inside `.git`): the latter is
    // refused rather than followed.
    let mut top = None;
    for d in cwd.ancestors() {
        if d.join(".git").exists() {
            top = Some(d);
            break;
        }
        if ["HEAD", "objects", "refs"].iter().all(|n| d.join(n).exists()) {
            return Err(format!("it would run in {}, a git repository's own folder", d.display()));
        }
    }
    let mut hooks: Option<PathBuf> = None;
    let mut extra: Vec<Pin> = Vec::new();
    let mut repo = None;
    if let Some(top) = top {
        let dot = top.join(".git");
        let gitdir = if dot.is_file() {
            let text = std::fs::read_to_string(&dot).map_err(|_| "its .git file cannot be read".to_string())?;
            let to = text
                .lines()
                .find_map(|l| l.trim().strip_prefix("gitdir:"))
                .ok_or("its .git file names no repository")?
                .trim()
                .to_string();
            git_path(&to, top, var).ok_or("its .git file names no repository")?
        } else {
            dot
        };
        let common = match std::fs::read_to_string(gitdir.join("commondir")) {
            Ok(c) => git_path(c.trim(), &gitdir, var).unwrap_or_else(|| gitdir.clone()),
            Err(_) => gitdir.clone(),
        };
        // Git passes over a `.git` that is no repository and goes on up, to
        // one nothing here pinned. The run is told which git folder to use,
        // and this is the backup.
        if !valid_git_dir(&gitdir, &common) {
            return Err(format!("{} is not a git repository git would use", top.join(".git").display()));
        }
        repo = Some(GitRepo {
            dir: gitdir.clone(),
            top: top.to_path_buf(),
        });
        // Which repository it is lives in these two: swapped, the same
        // paths would name another's config.
        let dot = top.join(".git");
        if dot.is_file() {
            extra.push(file("this worktree's .git file", &dot));
        }
        extra.push(file("this worktree's commondir", &gitdir.join("commondir")));
        configs.push(("this repository's git config".into(), common.join("config")));
        configs.push(("this worktree's git config".into(), gitdir.join("config.worktree")));
        // A submodule has a config, hooks and attributes of its own that
        // nothing here pins, and git can be led into one.
        if top.join(".gitmodules").exists() || common.join("modules").exists() {
            return Err("this repository has submodules, whose git config is not pinned".into());
        }
        hooks = Some(common.join("hooks"));
        extra.push(file("this repository's info/attributes", &common.join("info").join("attributes")));
        extra.push(file("this repository's .gitattributes", &top.join(".gitattributes")));
        extra.push(file("this repository's .lfsconfig", &top.join(".lfsconfig")));
        // The repository's own wins over the user's.
        for (_, c) in configs.iter().rev() {
            if let Some(h) = std::fs::read_to_string(c).ok().and_then(|t| git_refs(&t).hooks_path) {
                hooks = git_path(&h, top, var);
                break;
            }
        }
    }
    let mut pins: Vec<Pin> = configs.iter().map(|(w, p)| file(w, p)).collect();
    let mut n = 0;
    for (_, c) in &configs {
        let Ok(text) = std::fs::read_to_string(c) else {
            continue;
        };
        let base = c.parent().unwrap_or(Path::new(""));
        let refs = git_refs(&text);
        if refs.odd {
            return Err(format!("{} holds a value git could read otherwise than Horadric does", c.display()));
        }
        git_keys_ok(&refs, c)?;
        for inc in refs.includes {
            let Some(p) = git_path(&inc, base, var) else {
                return Err("a git config includes a file that cannot be placed".into());
            };
            if let Ok(t) = std::fs::read_to_string(&p) {
                let refs = git_refs(&t);
                if !refs.includes.is_empty() {
                    return Err(format!("{} includes yet another git config", p.display()));
                }
                if refs.odd {
                    return Err(format!("{} holds a value git could read otherwise than Horadric does", p.display()));
                }
                git_keys_ok(&refs, &p)?;
            }
            n += 1;
            pins.push(file(&format!("a git config included ({n})"), &p));
        }
    }
    if let Some(h) = hooks {
        pins.push(Pin {
            what: "this repository's git hooks".into(),
            path: h.to_string_lossy().into_owned(),
            kind: PinKind::Tree,
        });
    }
    pins.extend(extra);
    Ok((pins, repo))
}

/// Each pin's SHA-256, read fresh, never by modified time, which can be
/// set back: a folder's from the cache only while a change watch on it
/// has seen nothing. None when one could not be read through.
fn pin_hashes(pins: &[Pin]) -> Option<Vec<(String, String)>> {
    pins.iter()
        .map(|p| {
            let path = Path::new(&p.path);
            let h = match (std::fs::symlink_metadata(path), p.kind) {
                (Err(e), _) if e.kind() == std::io::ErrorKind::NotFound => Some("absent".to_string()),
                (Err(_), _) => None,
                (Ok(_), PinKind::Tree) => watched_tree(path, None),
                (Ok(_), PinKind::TreeBut(skip)) => watched_tree(path, Some(skip)),
                (Ok(_), PinKind::Names) => hash_names(path),
                (Ok(m), PinKind::File) if m.is_file() && !reparse(&m) => hash(&mut File::open(path).ok()?),
                (Ok(_), PinKind::File) => None,
            };
            h.map(|h| (p.what.clone(), h))
        })
        .collect()
}

/// A link or junction, whose content is somewhere no pin covers.
fn reparse(m: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// A tree's hash and the change notification armed before it was read.
struct Cached {
    hash: String,
    watch: Watch,
    /// The folder the path named when the watch was armed. The watch is
    /// on that folder, so a rename that puts another in its place is seen
    /// only here.
    id: (u32, u64),
}

/// The volume and file id of what `path` names itself, a link not
/// followed: the same id is the same folder, whatever it is called now.
fn file_id(path: &Path) -> Option<(u32, u64)> {
    use windows::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(0x7)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }.ok()?;
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Some((info.dwVolumeSerialNumber, index))
}

/// A change notification on a folder and everything under it, signalled
/// by the first change. Asked with a zero wait, so no thread waits on it
/// and nothing can be missed between a change and its being seen.
struct Watch(isize);

// SAFETY: a change notification handle may be waited on and closed from
// any thread.
unsafe impl Send for Watch {}

impl Watch {
    fn on(dir: &Path) -> Option<Watch> {
        use windows::Win32::Storage::FileSystem::{
            FindFirstChangeNotificationW, FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_DIR_NAME,
            FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SECURITY,
            FILE_NOTIFY_CHANGE_SIZE,
        };
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
        let filter = FILE_NOTIFY_CHANGE_FILE_NAME
            | FILE_NOTIFY_CHANGE_DIR_NAME
            | FILE_NOTIFY_CHANGE_ATTRIBUTES
            | FILE_NOTIFY_CHANGE_SIZE
            | FILE_NOTIFY_CHANGE_LAST_WRITE
            | FILE_NOTIFY_CHANGE_SECURITY;
        let h = unsafe { FindFirstChangeNotificationW(PCWSTR(wide.as_ptr()), true, filter) }.ok()?;
        (!h.is_invalid()).then(|| Watch(h.0 as isize))
    }

    /// Nothing changed since it was armed. A wait that fails counts as a
    /// change, which only costs a read.
    fn quiet(&self) -> bool {
        use windows::Win32::Foundation::WAIT_TIMEOUT;
        use windows::Win32::System::Threading::WaitForSingleObject;
        unsafe { WaitForSingleObject(HANDLE(self.0 as *mut c_void), 0) == WAIT_TIMEOUT }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        use windows::Win32::Storage::FileSystem::FindCloseChangeNotification;
        let _ = unsafe { FindCloseChangeNotification(HANDLE(self.0 as *mut c_void)) };
    }
}

/// Tree hashes for the app's life: a package of 250 MB is not read again
/// each run, only after something in its folder changed.
static TREES: Mutex<Option<HashMap<String, Cached>>> = Mutex::new(None);

/// [`hash_tree`] through the cache. The watch is armed before the tree is
/// read, so a change while it is read is a change the next run sees.
fn watched_tree(root: &Path, skip: Option<&str>) -> Option<String> {
    let key = format!("{}|{}", spelled(root), skip.unwrap_or_default());
    let mut trees = TREES.lock().unwrap_or_else(|e| e.into_inner());
    let trees = trees.get_or_insert_with(HashMap::new);
    if let Some(c) = trees.get(&key) {
        if c.watch.quiet() && file_id(root) == Some(c.id) {
            return Some(c.hash.clone());
        }
        trees.remove(&key);
    }
    // The id first, then the watch, then the read: a swap between any two
    // of them leaves an id or a signal that does not match the next time.
    let id = file_id(root);
    let watch = Watch::on(root);
    let hash = hash_tree(root, skip)?;
    if let (Some(watch), Some(id)) = (watch, id) {
        if file_id(root) == Some(id) {
            trees.insert(key, Cached { hash: hash.clone(), watch, id });
        }
    }
    Some(hash)
}

/// SHA-256 over every folder and file under `root`, by sorted relative
/// path, each file by its own SHA-256. Nothing inside is skipped but
/// files named `skip`; a link anywhere fails it, since what it points at
/// is not pinned.
fn hash_tree(root: &Path, skip: Option<&str>) -> Option<String> {
    let m = std::fs::symlink_metadata(root).ok()?;
    if !m.is_dir() || reparse(&m) {
        return None;
    }
    let mut lines: Vec<String> = Vec::new();
    let mut todo = vec![PathBuf::new()];
    while let Some(rel) = todo.pop() {
        for entry in std::fs::read_dir(root.join(&rel)).ok()? {
            let entry = entry.ok()?;
            let m = std::fs::symlink_metadata(entry.path()).ok()?;
            if reparse(&m) {
                return None;
            }
            let child = rel.join(entry.file_name());
            let shown = child.to_string_lossy().replace('\\', "/");
            if m.is_dir() {
                lines.push(format!("d {shown}"));
                todo.push(child);
            } else if skip.is_some_and(|s| entry.file_name().to_string_lossy().eq_ignore_ascii_case(s)) {
                continue;
            } else {
                let h = hash(&mut File::open(entry.path()).ok()?)?;
                lines.push(format!("f {shown} {h}"));
            }
        }
    }
    lines.sort();
    hash(&mut lines.join("\n").as_bytes())
}

/// SHA-256 of the names at the top of `dir`, each marked folder or file.
fn hash_names(dir: &Path) -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .map(|e| {
            let e = e.ok()?;
            let kind = if e.file_type().ok()?.is_dir() { 'd' } else { 'f' };
            Some(format!("{kind} {}", e.file_name().to_string_lossy().to_lowercase()))
        })
        .collect::<Option<_>>()?;
    names.sort();
    hash(&mut names.join("\n").as_bytes())
}

/// Why Always is not offered, None when it is.
fn once_why(pinned: &Pinned, runs: &Runs, place: &Place) -> Option<String> {
    let path = &pinned.path;
    // A shim is judged by the program it runs: `npm.cmd` is npm.
    let listed = match &runs.shim {
        Some(_) => once_only(&runs.stem),
        None => once_only(&path.to_string_lossy()),
    };
    if listed {
        return Some("it can run code the agent writes".into());
    }
    if let Some(why) = &runs.refused {
        return Some(why.clone());
    }
    if runs.pinned && !git_takes_attr_source() {
        return Some("the git on PATH is older than 2.40, too old to be kept from the project's attributes".into());
    }
    if runs.pinned && runs.fingerprint.is_none() {
        return Some("a file that decides what it runs could not be read through, or is a link".into());
    }
    if pinned.remote {
        return Some("it is on a network share".into());
    }
    let mut ran = vec![path.as_path()];
    if let Some(s) = &runs.shim {
        ran.extend([s.package.as_path(), s.node.as_path()]);
    }
    let agents = [place.dir.as_path(), place.cwd.as_path(), &std::env::temp_dir()];
    if ran.iter().any(|p| agents.iter().any(|d| inside(p, d))) {
        return Some("it is inside the project or the temp folder, where the agent writes".into());
    }
    // A shim's package is pinned by content, so a global npm folder the
    // user can write to may still have Always; the node it runs must not
    // be, as a library planted beside it would load.
    let own = match &runs.shim {
        Some(s) => s.node.as_path(),
        None => path.as_path(),
    };
    if changeable(own) {
        return Some("you can change it or its folder without elevation, so the agent could too".into());
    }
    None
}

/// The secret run route: checks the session and its key, then on a worker
/// thread finds the program and the secrets, asks the human where needed,
/// and starts the program, streaming its masked output back. Call it on
/// the UI thread; it returns at once and touches no file there.
pub fn serve(place: Option<Place>, req: SecretRun) {
    if req.session.is_empty() || !key_ok(&req.session, &req.key) {
        req.out
            .fail("Refused: this is not a live Horadric session with its own key.");
        return;
    }
    let Some(place) = place else {
        req.out
            .fail("Refused: Horadric does not know this session's project.");
        return;
    };
    waker();
    let left = Arc::new(AtomicBool::new(false));
    req.out.watch_gone(Arc::clone(&left));
    thread::spawn(move || prepare(place, req, left));
}

fn prepare(place: Place, req: SecretRun, left: Arc<AtomicBool>) {
    let SecretRun {
        session,
        names,
        stdin,
        program,
        args,
        cwd,
        out,
        ..
    } = req;
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
    if remote_or_device(&cwd) {
        out.fail("Refused: run it from the session's project folder on this PC, not a share.");
        return;
    }
    let cwd = PathBuf::from(&cwd);
    if !cwd.is_dir() || !(inside(&cwd, &place.dir) || inside(&cwd, &place.cwd)) {
        out.fail("Refused: run it from inside the session's project folder.");
        return;
    }
    let pinned = match program_path(&program).and_then(|p| pin(&p)) {
        Ok(p) => p,
        Err(why) => {
            out.fail(&why);
            return;
        }
    };
    let shown = plainly(&horadric_pty::command_line(&pinned.path, &args));
    if shown.chars().count() > SHOWN_MAX {
        out.fail(&format!(
            "Refused: the command line is {} characters, longer than the {SHOWN_MAX} the human can be shown. Put the arguments in a file the program reads.",
            shown.chars().count()
        ));
        return;
    }
    if let Some(why) = store::problem() {
        out.fail(&why);
        return;
    }
    let program_lower = pinned.path.to_string_lossy().to_ascii_lowercase();
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
    let env = super::identity::session_env(&place.dir);
    let runs = Runs::of(&pinned, &env, &args, &cwd);
    let bound = runs.bound(&program_lower, &stdin);
    // A pin that could not be read matches no Always, and gets none.
    let held_to = runs.fingerprint.clone().unwrap_or_else(|| "unreadable".into());
    // A library planted where the program runs voids every Always: each
    // secret is asked, Once only.
    let dll = loose_dll(&cwd);
    let unasked: Vec<String> = secrets
        .iter()
        .filter(|s| dll.is_some() || !approved(&s.allowed, &place.key, &bound, &held_to))
        .map(|s| s.name.clone())
        .collect();
    let mut held = None;
    let asked = if unasked.is_empty() {
        None
    } else {
        let Some(c) = claim(&session) else {
            out.fail(ALREADY_ASKING);
            return;
        };
        held = Some(c);
        let why_once = match &dll {
            Some(_) => Some("the folder it runs in holds a .dll, which it could load".to_string()),
            None => once_why(&pinned, &runs, &place),
        };
        let stem = &runs.stem;
        let whose: Vec<String> = secrets
            .iter()
            .filter(|s| unasked.contains(&s.name))
            .map(|s| owned(&s.name, &s.owner))
            .collect();
        let what = match runs.command.is_empty() {
            true => stem.clone(),
            false => format!("{stem} {}", runs.command),
        };
        let title = format!("Use {} for `{}`?", unasked.join(", "), plainly(&what));
        let mut text = format!(
            "{} asks to run this with {} in its environment:\n\n{shown}\n\nProgram: {}",
            asker(&session, Some(&crate::project::project_name(&place.key))),
            whose.join(", "),
            plainly(&pinned.path.to_string_lossy()),
        );
        let changed: Vec<String> = secrets
            .iter()
            .filter(|s| unasked.contains(&s.name))
            .flat_map(|s| changed_pins(&s.allowed, &place.key, &bound, &held_to))
            .fold(Vec::new(), |mut all, c| {
                if !all.contains(&c) {
                    all.push(c);
                }
                all
            });
        if !changed.is_empty() {
            text.push_str(&format!(
                "\n\nAsked again: {} changed since you allowed it.",
                changed.join(", ")
            ));
        }
        if runs_builds(stem) {
            text.push_str("\n\nIt may run this project's build scripts.");
        }
        if why_once.is_none() && (runs_builds(stem) || stem.eq_ignore_ascii_case("gh")) {
            text.push_str(
                "\n\nAlways runs it with a PATH of only the folders you cannot write to, so a tool it starts from one you can is not found.",
            );
        }
        let always = why_once.is_none();
        if let Some(why) = why_once {
            text.push_str(&format!("\n\nOnly Once is offered: {why}."));
        }
        Some(ask(&session, &title, &text, always))
    };
    let job = Job {
        session,
        project: place.key,
        pinned,
        program_lower,
        bound,
        args,
        cwd,
        env,
        runs,
        held_to,
        dir: place.dir,
        always: false,
        secrets,
        stdin,
        unasked,
        at,
    };
    job.run(asked, held, out, left);
}

struct Job {
    session: String,
    project: String,
    pinned: Pinned,
    program_lower: String,
    /// The program and subcommand an Always is kept for.
    bound: String,
    args: Vec<String>,
    cwd: PathBuf,
    env: super::identity::SessionEnv,
    runs: Runs,
    /// The fingerprint the question was asked about, which an Always keeps.
    held_to: String,
    /// The project's main folder, kept off an Always run's PATH.
    dir: PathBuf,
    /// Runs on an Always, given now or before: a locked PATH.
    always: bool,
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

    fn run(mut self, asked: Option<Asked>, held: Option<Claim>, out: SecretStream, left: Arc<AtomicBool>) {
        let answer = match asked {
            None => Answer::Once,
            Some(a) => {
                let always = a.always;
                match a.until(WAIT, Some(&left)) {
                    Waited::Pressed(p) => answer(p, always),
                    Waited::Left => {
                        for n in &self.unasked {
                            self.log(n, "asker left");
                        }
                        out.fail("The command went away before the human answered.");
                        return;
                    }
                    Waited::Unanswered => {
                        for n in &self.unasked {
                            self.log(n, "unanswered");
                        }
                        let n = &self.unasked[0];
                        out.fail(&format!(
                            "Nobody answered whether {n} may go to {}. Ask the human to allow it, or block the quest with --on-secret {n}.",
                            self.pinned.path.display()
                        ));
                        return;
                    }
                }
            }
        };
        drop(held);
        self.always = self.unasked.is_empty() || answer == Answer::Always;
        // Checked again just before the start, as a question can wait a
        // minute. A library planted between this and the program loading
        // it still gets in; the window is the start itself.
        if self.always {
            if let Some(dll) = loose_dll(&self.cwd) {
                for s in &self.secrets {
                    self.log(&s.name, "refused: a .dll beside it");
                }
                out.fail(&format!(
                    "Refused: {dll} is in the folder it would run in, which it could load. Run it from a folder without one."
                ));
                return;
            }
        }
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
                self.pinned.path.display()
            ));
            return;
        }
        let at = self.at;
        let what = self
            .pinned
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        // What is pinned is not held open as the program is, so it is read
        // again after a question: a change while the human looked is a
        // change they did not see. It can still change between this and
        // the program reading it; that window is the run's start, not the
        // minute a question can wait.
        let asked_about = !self.unasked.is_empty();
        if asked_about && self.runs.pinned {
            // Found again from the start, not only hashed again: a swapped
            // commondir or .git file names other paths with the same names.
            let now = Runs::of(&self.pinned, &self.env, &self.args, &self.cwd);
            if now.pins != self.runs.pins
                || now.fingerprint.as_deref() != self.runs.fingerprint.as_deref()
                || now.refused != self.runs.refused
            {
                for n in &self.unasked {
                    self.log(n, "changed while asked");
                }
                out.fail("A file that decides what the program runs changed while the human was asked. Run it again.");
                return;
            }
        }
        let granted = approval(&self.project, &self.bound, &self.held_to);
        // An Always for this program in this project from before it changed
        // is spent: the new one replaces it.
        let stale = format!("|{}|{}", self.project, self.bound);
        let _ = store::change(|v| {
            for kept in &self.secrets {
                let Some(s) = v
                    .secrets
                    .iter_mut()
                    .find(|s| s.name == kept.name && s.owner == kept.owner)
                else {
                    continue;
                };
                if answer == Answer::Always && self.unasked.contains(&s.name) {
                    s.allowed.retain(|a| !a.ends_with(&stale));
                    s.allowed.push(granted.clone());
                }
                s.last_used = Some(store::Used {
                    at,
                    session: self.session.clone(),
                    what: what.clone(),
                });
            }
        });
        crate::settings::vault_changed();
        self.start(out, left);
    }

    /// What an Always run of a pinned CLI gets so the git, pager, editor
    /// and browser it may start run nothing from the project or the
    /// agent: no hooks, no fsmonitor, no bare repository found by itself,
    /// no program named by the environment. What git's own config still
    /// runs (filters, drivers, aliases) is pinned instead.
    fn quiet_git(&self, c: &mut Command, hooks: &Path, attributes: &Path) {
        // Only an identity's own settings are kept; the app's environment
        // could have been given any with `setx`.
        let own = self
            .env
            .set
            .iter()
            .rev()
            .find(|(k, _)| k.eq_ignore_ascii_case("GIT_CONFIG_PARAMETERS"))
            .map(|(_, v)| v.clone());
        let hooks = hooks.to_string_lossy().replace('\\', "/");
        let attributes = attributes.to_string_lossy().replace('\\', "/");
        c.env(
            "GIT_CONFIG_PARAMETERS",
            git_config_parameters(
                own.as_deref(),
                &[
                    ("core.hooksPath", &hooks),
                    ("core.fsmonitor", "false"),
                    ("safe.bareRepository", "explicit"),
                    ("core.attributesFile", &attributes),
                    // A repository added inside the project is a submodule
                    // to git, with a config and attributes of its own that
                    // nothing here pins: git is kept out of them.
                    ("diff.ignoreSubmodules", "all"),
                    ("submodule.recurse", "false"),
                    ("fetch.recurseSubmodules", "false"),
                    ("status.submoduleSummary", "false"),
                ],
            ),
        );
        c.env("GIT_ATTR_SOURCE", EMPTY_TREE);
        let (set, remove) = git_place(self.runs.git_repo.as_ref(), &self.cwd);
        for k in remove {
            c.env_remove(k);
        }
        for (k, v) in set {
            c.env(k, v);
        }
        for name in QUIET_REMOVED {
            c.env_remove(name);
        }
        // Traces, and settings by number from the app's environment, a
        // `setx` away; an identity's own numbered settings stay.
        let names = std::env::vars_os()
            .filter_map(|(k, _)| k.into_string().ok())
            .chain(self.env.set.iter().map(|(k, _)| k.clone()));
        for k in names {
            let identity = self.env.set.iter().any(|(s, _)| s.eq_ignore_ascii_case(&k));
            if quiet_removed(&k) && !(identity && numbered_git_config(&k)) {
                c.env_remove(&k);
            }
        }
        for (name, value) in [
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_EDITOR", ":"),
            ("GIT_SEQUENCE_EDITOR", ":"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_PAGER", ""),
            ("GH_PAGER", ""),
            ("PAGER", ""),
            ("GH_BROWSER", ""),
            ("BROWSER", ""),
            ("GH_EDITOR", ""),
            ("EDITOR", ""),
            ("VISUAL", ""),
            ("GH_PROMPT_DISABLED", "1"),
            ("GH_NO_UPDATE_NOTIFIER", "1"),
        ] {
            c.env(name, value);
        }
        // Another host for gh is another place the token goes; a secret
        // run on purpose sets its own after this.
        if self.runs.stem.eq_ignore_ascii_case("gh") {
            let names = std::env::vars_os()
                .filter_map(|(k, _)| k.into_string().ok())
                .chain(self.env.set.iter().map(|(k, _)| k.clone()));
            for k in names {
                let up = k.to_ascii_uppercase();
                if up == "GH_HOST" || up.starts_with("GH_ENTERPRISE_") || up == "GITHUB_ENTERPRISE_TOKEN" {
                    c.env_remove(&k);
                }
            }
        }
    }

    /// The command to start; `path` is the locked PATH of an Always run.
    /// `hooks` is the empty hooks folder an Always run of a pinned CLI
    /// gives the git it runs.
    fn command(&self, path: Option<&OsString>, quiet: Option<&(PathBuf, PathBuf)>) -> Command {
        let program = &self.pinned.path;
        let mut c = if let Some(shim) = &self.runs.shim {
            let mut c = Command::new(&shim.node);
            c.arg(&shim.script).args(&self.args);
            c
        } else if horadric_pty::is_batch(program) {
            // A batch file only runs through cmd, and its arguments are
            // read twice there, so they are quoted the way the pty's
            // launches quote them.
            let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
            let line = horadric_pty::launch_line(program, &self.args);
            let head = horadric_pty::command_line(Path::new(&comspec), &[]);
            let mut c = Command::new(&comspec);
            c.raw_arg(line.strip_prefix(&head).unwrap_or(&line).trim_start());
            c
        } else {
            let mut c = Command::new(program);
            c.args(&self.args);
            c
        };
        // Suspended until it is in the job, so nothing it starts in its
        // first instant escapes the tree that is ended with it.
        c.current_dir(&self.cwd)
            .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        c.env_remove("HORADRIC_SECRET_KEY");
        for name in &self.env.remove {
            c.env_remove(name);
        }
        for (k, v) in &self.env.set {
            c.env(k, v);
        }
        if let Some(path) = path {
            c.env("PATH", path);
        }
        // cmd, which CLIs start for helpers, would otherwise look in the
        // project folder first for a program named without a path.
        c.env("NoDefaultCurrentDirectoryInExePath", "1");
        if self.runs.shim.is_some() {
            // Each of these would have node load code from outside the
            // pinned package: a compile cache, a module path, a --require.
            c.env("NODE_DISABLE_COMPILE_CACHE", "1");
            c.env_remove("NODE_PATH");
            c.env_remove("NODE_OPTIONS");
        }
        if let Some((hooks, attributes)) = quiet {
            self.quiet_git(&mut c, hooks, attributes);
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

    fn start(self, out: SecretStream, left: Arc<AtomicBool>) {
        let Some(tree) = Tree::new() else {
            out.fail("cannot start it: Windows would not make a job to keep it in.");
            return;
        };
        let locked = self.always.then(|| {
            let node = self.runs.shim.as_ref().map(|s| s.node.as_path());
            let own = child_var(&self.env, "PATH");
            let temp = std::env::temp_dir();
            locked_path(own, node, &[&self.dir, &self.cwd, &temp])
        });
        let quiet = (self.always && self.runs.pinned)
            .then(|| -> Result<(PathBuf, PathBuf), String> { Ok((no_hooks()?, no_attributes()?)) });
        let hooks = match quiet {
            Some(Err(why)) => {
                out.fail(&why);
                return;
            }
            Some(Ok(h)) => Some(h),
            None => None,
        };
        let mut child = match self.command(locked.as_ref().map(|(p, _)| p), hooks.as_ref()).spawn() {
            Ok(c) => c,
            Err(e) => {
                out.fail(&format!("cannot start {}: {e}", self.pinned.path.display()));
                return;
            }
        };
        if !tree.adopt(&child) || !resume(child.id()) {
            let _ = child.kill();
            let _ = child.wait();
            out.fail(&format!(
                "cannot start {}: it could not be kept in a job that ends with it.",
                self.pinned.path.display()
            ));
            return;
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
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let mut readers = 0;
        for (i, pipe) in [
            child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
            child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
        ]
        .into_iter()
        .enumerate()
        {
            let Some(pipe) = pipe else { continue };
            let mut out = out.clone();
            let mut mask = Mask::new(values.clone());
            let left = Arc::clone(&left);
            let done = done_tx.clone();
            readers += 1;
            thread::spawn(move || {
                pump(pipe, &mut mask, i == 1, &mut out, &left);
                let _ = done.send(());
            });
        }
        drop(done_tx);
        // A CLI that went away takes the program with it.
        let code = loop {
            if left.load(Ordering::Relaxed) {
                tree.end();
                let _ = child.kill();
            }
            match child.try_wait() {
                Ok(Some(status)) => break status.code().unwrap_or(-1),
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(_) => break -1,
            }
        };
        // Whatever it left running, a daemon holding its output open
        // among them, ends with it, or the CLI would wait on it forever.
        tree.end();
        let until = Instant::now() + DRAIN;
        for _ in 0..readers {
            let rest = until.saturating_duration_since(Instant::now());
            if done_rx.recv_timeout(rest).is_err() {
                break;
            }
        }
        // A program that worked when allowed Once may fail on Always for a
        // tool it could not find; this says where it was not looked for.
        let mut out = out;
        if let Some((_, left_out)) = locked.filter(|(_, l)| code != 0 && !l.is_empty()) {
            let line = format!(
                "horadric: this Always run's PATH left out {}\n",
                left_out.join("; ")
            );
            let _ = out.err(line.as_bytes());
        }
        out.exit(code);
        drop(tree);
        drop(self.pinned);
    }
}

/// Lets a process created suspended run: every thread of it, which is its
/// first one.
fn resume(pid: u32) -> bool {
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) else {
            return false;
        };
        let snap = OwnedHandle::from_raw_handle(snap.0);
        let snap_h = HANDLE(snap.as_raw_handle());
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        let mut resumed = false;
        let mut more = Thread32First(snap_h, &mut entry).is_ok();
        while more {
            if entry.th32OwnerProcessID == pid {
                if let Ok(t) = OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID) {
                    let t = OwnedHandle::from_raw_handle(t.0);
                    resumed |= ResumeThread(HANDLE(t.as_raw_handle())) != u32::MAX;
                }
            }
            more = Thread32Next(snap_h, &mut entry).is_ok();
        }
        resumed
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

    fn adopt(&self, child: &Child) -> bool {
        unsafe {
            AssignProcessToJobObject(HANDLE(self.0.as_raw_handle()), HANDLE(child.as_raw_handle()))
                .is_ok()
        }
    }

    fn end(&self) {
        unsafe {
            let _ = TerminateJobObject(HANDLE(self.0.as_raw_handle()), 1);
        }
    }
}

/// The folder a project's key is made from: the top of the git tree
/// `cwd` is in, else the nearest folder above holding `.git`, else `cwd`.
/// Run in a subfolder, `secret set` would otherwise key the secret to the
/// subfolder, where no session of the project looks.
fn project_top(cwd: &str) -> String {
    let asked = horadric_hooks::no_window(&mut Command::new("git"))
        .args(["--no-optional-locks", "rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|t| !t.is_empty());
    if let Some(top) = asked {
        return top;
    }
    Path::new(cwd)
        .ancestors()
        .find(|d| d.join(".git").exists())
        .map_or_else(|| cwd.to_string(), |d| d.to_string_lossy().into_owned())
}

/// The project a plain terminal's `secret set` keys a secret to: the one
/// sessions run there would look under. `known` holds the app's project
/// keys; the nearest of them at or above `cwd` wins, so a subfolder of a
/// project that is itself inside another repository still lands on the
/// project. Failing that, the nearest folder holding `.horadric`, then
/// the git top.
fn plain_project_key(cwd: &str, known: &HashSet<String>) -> String {
    let own = crate::project::folder_key(cwd);
    if known.contains(&own) {
        return own;
    }
    let path = Path::new(cwd);
    if let Some(key) = path
        .ancestors()
        .map(|d| crate::project::normalise(&d.to_string_lossy()))
        .find(|k| known.contains(k))
    {
        return key;
    }
    match path.ancestors().find(|d| d.join(".horadric").is_dir()) {
        Some(d) => crate::project::folder_key(&d.to_string_lossy()),
        None => crate::project::folder_key(&project_top(cwd)),
    }
}

/// `horadric secret set`: keeps a value once the human says so in the
/// app, since an agent could otherwise swap a secret for a value of its
/// own. Call it on the UI thread; it returns at once, and all the work,
/// git included, is done on a thread of its own. `place` is the calling
/// session's, so the secret goes where that session will look for it;
/// `known` is every project key the app has, for a plain terminal.
pub fn set(place: Option<Place>, known: HashSet<String>, req: SecretSet) {
    waker();
    thread::spawn(move || set_off_ui(place, known, req));
}

fn set_off_ui(place: Option<Place>, known: HashSet<String>, req: SecretSet) {
    let SecretSet {
        session,
        key,
        cwd,
        name,
        value,
        owner,
        reply,
    } = req;
    let mut value = value;
    let error = |why: String| reply.send(json!({ "error": why }));
    if let Some(s) = &session {
        if !key_ok(s, &key) {
            error("Refused: this terminal names a Horadric session but does not hold its key.".into());
            return;
        }
    }
    if !valid_name(&name) {
        error(format!("{name} is not a secret's name: upper case letters, digits and _, a letter first."));
        return;
    }
    if value.is_empty() && session.is_some() {
        error("An empty value is not kept.".into());
        return;
    }
    let (owner, whose) = match owner.as_str() {
        "everywhere" => (Owner::Everywhere, "every project".to_string()),
        "project" | "" => {
            if remote_or_device(&cwd) {
                error("Run it inside a project's folder on this PC, or give --everywhere.".into());
                return;
            }
            let key = match (&session, place) {
                (Some(_), Some(place)) => place.key,
                _ => plain_project_key(&cwd, &known),
            };
            if key.is_empty() {
                error("Run it inside a project's folder, or give --everywhere.".into());
                return;
            }
            let whose = crate::project::project_name(&key);
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
    if let Some(why) = store::problem() {
        error(why);
        return;
    }
    let session = session.unwrap_or_default();
    let Some(held) = claim(&session) else {
        error(ALREADY_ASKING.into());
        return;
    };
    let replaces = store::read()
        .secrets
        .iter()
        .any(|s| s.name == name && s.owner == owner);
    let title = format!("Keep {name}?");
    let replacing = if replaces {
        ", replacing the value kept now and every Always given to it"
    } else {
        ""
    };
    // A terminal with no session key proves nothing about who is in it:
    // an agent could run `secret set` there and name any project by its
    // folder. So its value is not taken; the human types one.
    let (text, buttons, typed) = if session.is_empty() {
        let (tx, rx) = mpsc::channel();
        let text = format!(
            "{} asks to keep {name} for {whose}{replacing}. It may be an agent rather than you, so the value it sent is not used: if you want {name} kept, type the value yourself in the next step.",
            asker(&session, None),
        );
        let prompt = format!("The value of {name} for {whose}, one line. Type or paste it yourself.");
        (text, vec!["Type it in".to_string(), "No".to_string()], Some((prompt, tx, rx)))
    } else {
        let text = format!(
            "{} asks to keep {name} for {whose}{replacing}. Keep it only if you typed it yourself.",
            asker(&session, None),
        );
        (text, vec!["Keep".to_string(), "No".to_string()], None)
    };
    let (masked, typed) = match typed {
        Some((prompt, tx, rx)) => (Some((prompt, tx)), Some(rx)),
        None => (None, None),
    };
    let asked = put_with(&session, &title, &text, buttons, masked);
    let project = match &owner {
        Owner::Project(k) => k.clone(),
        o => o.label(),
    };
    // Typing the value in comes before the answer, so it gets longer.
    let wait = if typed.is_some() { SET_WAIT * 3 } else { SET_WAIT };
    let pressed = match asked.until(wait, None) {
        Waited::Pressed(p) => p,
        _ => None,
    };
    if let (Some(0), Some(typed)) = (pressed, typed) {
        // Sent before the button, so it is there already.
        match typed.recv_timeout(Duration::from_secs(1)).ok().flatten() {
            Some(v) if !v.trim().is_empty() => value = v.trim().to_string(),
            _ => {
                drop(held);
                reply.send(json!({ "error": format!("{name} was not kept: no value was typed in Horadric.") }));
                return;
            }
        }
    }
    drop(held);
    let at = now();
    if pressed != Some(0) {
        store::log(&log_line(at, &project, &session, &name, "-", "set refused"));
        reply.send(json!({ "error": format!("{name} was not kept: the human did not say yes in Horadric.") }));
        return;
    }
    if !reply.wanted() {
        return;
    }
    let kept = store::change(|v| {
        match v.secrets.iter_mut().find(|s| s.name == name && s.owner == owner) {
            Some(s) => {
                // A new value is a new secret: what was allowed for the
                // old one is asked again.
                s.value = value;
                s.allowed.clear();
            }
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
            crate::settings::vault_changed();
            store::log(&log_line(at, &project, &session, &name, "-", "set"));
            reply.send(json!({ "text": format!("{name} is kept for {whose}.") }));
        }
        Err(e) => reply.send(json!({ "error": format!("{name} was not kept: {e}") })),
    }
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
    if let Some(why) = store::problem() {
        req.reply.send(json!({ "error": why }));
        return;
    }
    let identity = identity_slug(&place.dir);
    let (secrets, logins) = store::names(&place.key, identity.as_deref());
    let mut all: Vec<String> = secrets.into_iter().map(|(n, _)| n).collect();
    all.extend(logins.into_iter().map(|(n, _)| n));
    all.sort();
    all.dedup();
    req.reply.send(json!({ "names": all }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_with_a_stray_colon_or_on_a_share_is_refused() {
        assert!(refused_program("gh").is_none());
        assert!(refused_program("gh.exe").is_none());
        assert!(refused_program(r"C:\Program Files\GitHub CLI\gh.exe").is_none());
        assert!(refused_program("C:/tools/gh.exe").is_none());
        assert!(refused_program("C:gh.exe").is_some());
        assert!(refused_program(r"C:\tools\gh.exe:evil").is_some());
        assert!(refused_program("gh.exe:evil").is_some());
        assert!(refused_program(r"\\server\share\gh.exe").is_some());
        assert!(refused_program(r"\\?\UNC\server\share\gh.exe").is_some());
        assert!(refused_program(r"\\.\pipe\x").is_some());
        assert!(refused_program("//server/share/gh.exe").is_some());
    }

    #[test]
    fn a_folder_on_a_share_or_a_device_is_neither_here() {
        assert!(!remote_or_device(r"C:\work\a"));
        assert!(!remote_or_device(r"\\?\C:\work\a"));
        assert!(remote_or_device(r"\\server\share"));
        assert!(remote_or_device("//server/share"));
        assert!(remote_or_device(r"\\?\UNC\server\share"));
        assert!(remote_or_device(r"\\.\C:\work"));
        assert!(remote_or_device(r"\\?\GLOBALROOT\Device\x"));
    }

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("horadric-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn git_pins_follow_a_worktree_to_its_repository() {
        let root = scratch("git-pins");
        let home = root.join("home");
        let main = root.join("main");
        let wt = root.join("wt");
        let common = main.join(".git");
        let gitdir = common.join("worktrees").join("wt");
        for d in [&home, &gitdir, &wt.join("sub")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        std::fs::write(common.join("config"), "[core]\n\thooksPath = .githooks\n").unwrap();
        std::fs::write(home.join(".gitconfig"), "[include]\n\tpath = ~/more.gitconfig\n").unwrap();
        std::fs::write(home.join("more.gitconfig"), "[alias]\n\tx = !sh\n").unwrap();
        let h = home.to_string_lossy().into_owned();
        let var = |k: &str| (k == "USERPROFILE").then(|| h.clone());
        std::fs::write(gitdir.join("HEAD"), "ref: refs/heads/wt\n").unwrap();
        for d in ["objects", "refs"] {
            std::fs::create_dir_all(common.join(d)).unwrap();
        }
        let (pins, repo) = git_pins(&wt.join("sub"), &var).unwrap();
        assert_eq!(repo, Some(GitRepo { dir: gitdir.clone(), top: wt.clone() }));
        let path_of = |what: &str| pins.iter().find(|p| p.what == what).map(|p| PathBuf::from(&p.path));
        assert_eq!(path_of("this repository's git config"), Some(gitdir.join("../..").join("config")));
        assert_eq!(path_of("this worktree's git config"), Some(gitdir.join("config.worktree")));
        assert_eq!(path_of("this repository's git hooks"), Some(wt.join(".githooks")));
        assert_eq!(path_of("a git config included (1)"), Some(home.join("more.gitconfig")));
        assert_eq!(path_of("this repository's .gitattributes"), Some(wt.join(".gitattributes")));
        assert!(path_of("your global git config").is_some());
        let mut whats: Vec<&str> = pins.iter().map(|p| p.what.as_str()).collect();
        whats.sort();
        whats.dedup();
        assert_eq!(whats.len(), pins.len(), "each pin is named once");
        // An include that includes again is not followed.
        std::fs::write(home.join("more.gitconfig"), "[include]\n\tpath = deeper\n").unwrap();
        assert!(git_pins(&wt, &var).is_err());
        // Outside a repository only the user's own config counts.
        std::fs::write(home.join("more.gitconfig"), "").unwrap();
        let (outside, none) = git_pins(&home, &var).unwrap();
        assert_eq!(none, None);
        assert!(outside.iter().all(|p| !p.what.starts_with("this ")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn gh_extensions_skip_state_and_names_see_only_names() {
        let root = scratch("pin-kinds");
        std::fs::create_dir_all(root.join("ext/gh-x")).unwrap();
        std::fs::write(root.join("ext/gh-x/gh-x.exe"), "one").unwrap();
        std::fs::write(root.join("ext/gh-x/state.yml"), "a").unwrap();
        let ext = root.join("ext").to_string_lossy().into_owned();
        let pins = [
            Pin { what: "e".into(), path: ext.clone(), kind: PinKind::TreeBut("state.yml") },
            Pin { what: "n".into(), path: ext, kind: PinKind::Names },
        ];
        let first = pin_hashes(&pins).unwrap();
        std::fs::write(root.join("ext/gh-x/state.yml"), "b").unwrap();
        std::fs::write(root.join("ext/gh-x/gh-x.exe"), "one").unwrap();
        assert_eq!(pin_hashes(&pins).unwrap(), first, "state.yml and same content change nothing");
        std::fs::write(root.join("ext/gh-x/gh-x.exe"), "two").unwrap();
        let second = pin_hashes(&pins).unwrap();
        assert_ne!(second[0], first[0], "the cached hash went with the change");
        assert_eq!(second[1], first[1], "names alone did not change");
        std::fs::create_dir_all(root.join("ext/encoding")).unwrap();
        assert_ne!(pin_hashes(&pins).unwrap()[1], first[1], "a new entry is a new name");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_locked_path_says_what_it_left_out() {
        let root = scratch("locked-path");
        let project = root.join("project");
        std::fs::create_dir_all(project.join("bin")).unwrap();
        let own = std::env::join_paths([project.join("bin"), PathBuf::from("rel"), PathBuf::from(r"C:\Windows")])
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let (path, left) = locked_path(Some(own), None, &[&project]);
        let kept: Vec<PathBuf> = std::env::split_paths(&path).collect();
        assert!(!kept.iter().any(|k| k.starts_with(&project)));
        assert!(left.iter().any(|l| l.contains("inside the project")), "{left:?}");
        assert!(left.iter().any(|l| l.contains("relative")), "{left:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pinned_folder_swapped_by_a_rename_is_read_again() {
        let root = scratch("swap");
        let ext = root.join("ext");
        std::fs::create_dir_all(&ext).unwrap();
        std::fs::write(ext.join("a.js"), "one").unwrap();
        let first = watched_tree(&ext, None).unwrap();
        assert_eq!(watched_tree(&ext, None).unwrap(), first);
        std::fs::rename(&ext, root.join("old")).unwrap();
        std::fs::create_dir_all(&ext).unwrap();
        std::fs::write(ext.join("a.js"), "two").unwrap();
        assert_ne!(watched_tree(&ext, None).unwrap(), first);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn git_pins_refuse_a_repository_folder_and_an_odd_config() {
        let root = scratch("git-odd");
        let bare = root.join("bare.git");
        for d in ["objects", "refs", "sub"] {
            std::fs::create_dir_all(bare.join(d)).unwrap();
        }
        std::fs::write(bare.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let none = |_: &str| None;
        assert!(git_pins(&bare.join("sub"), &none).is_err());
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        // A `.git` that is no repository is passed over by git, so it is
        // refused rather than taken.
        assert!(git_pins(&repo, &none).is_err());
        for d in ["objects", "refs"] {
            std::fs::create_dir_all(repo.join(".git").join(d)).unwrap();
        }
        std::fs::write(repo.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(repo.join(".git").join("config"), "[core]\n\thooksPath = x ; y\n").unwrap();
        assert!(git_pins(&repo, &none).is_err());
        std::fs::write(repo.join(".git").join("config"), "[core]\n\tbare = false\n").unwrap();
        assert!(git_pins(&repo, &none).is_ok());
        // Submodules, declared or already cloned, have configs of their own.
        std::fs::write(repo.join(".gitmodules"), "").unwrap();
        assert!(git_pins(&repo, &none).is_err());
        std::fs::remove_file(repo.join(".gitmodules")).unwrap();
        std::fs::create_dir_all(repo.join(".git").join("modules")).unwrap();
        assert!(git_pins(&repo, &none).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn git_finds_only_the_pinned_repository() {
        let Some(git) = horadric_pty::find_program("git", &app_path(), &[".exe"], Path::is_file) else {
            return;
        };
        let root = scratch("git-place");
        let outer = root.join("outer");
        let inner = outer.join("inner");
        let run = |cwd: &Path, args: &[&str], env: &(Vec<(&'static str, String)>, Vec<&'static str>)| {
            let mut c = Command::new(&git);
            c.args(args).current_dir(cwd).env("GIT_CONFIG_NOSYSTEM", "1");
            for k in &env.1 {
                c.env_remove(k);
            }
            for (k, v) in &env.0 {
                c.env(k, v);
            }
            c.output().unwrap()
        };
        std::fs::create_dir_all(inner.join("sub")).unwrap();
        assert!(run(&outer, &["init", "-q"], &(vec![], vec![])).status.success());
        assert!(run(&inner, &["init", "-q"], &(vec![], vec![])).status.success());
        let none = |_: &str| None;
        let (_, repo) = git_pins(&inner.join("sub"), &none).unwrap();
        let repo = repo.unwrap();
        let env = git_place(Some(&repo), &inner.join("sub"));
        let top = run(&inner.join("sub"), &["rev-parse", "--show-toplevel"], &env);
        assert!(top.status.success());
        assert_eq!(
            spelled(Path::new(String::from_utf8_lossy(&top.stdout).trim())),
            spelled(&inner)
        );
        assert!(run(&inner.join("sub"), &["status", "--short"], &env).status.success());
        // A `.git` git would pass over leaves only Once, and with no
        // repository pinned git finds none, not the one above.
        std::fs::remove_dir_all(inner.join(".git")).unwrap();
        std::fs::create_dir_all(inner.join(".git")).unwrap();
        assert!(git_pins(&inner, &none).is_err());
        let found = run(&inner, &["rev-parse", "--show-toplevel"], &(vec![], vec![]));
        assert_eq!(spelled(Path::new(String::from_utf8_lossy(&found.stdout).trim())), spelled(&outer));
        let env = git_place(None, &inner);
        assert!(!run(&inner, &["rev-parse", "--show-toplevel"], &env).status.success());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_git_setting_that_runs_a_program_leaves_only_once() {
        let root = scratch("git-keys");
        let repo = root.join("repo");
        let git = repo.join(".git");
        for d in ["objects", "refs"] {
            std::fs::create_dir_all(git.join(d)).unwrap();
        }
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(git.join("config"), "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = https://x/y\n").unwrap();
        let none = |_: &str| None;
        let (pins, _) = git_pins(&repo, &none).unwrap();
        assert!(pins.iter().any(|p| p.what == "this worktree's commondir"));
        std::fs::write(git.join("config"), "[core]\n\tbare = false\n[filter \"x\"]\n\tclean = sh -c evil\n").unwrap();
        let why = git_pins(&repo, &none).unwrap_err();
        assert!(why.contains("filter.x.clean") && why.contains("config"), "{why}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn proxies_and_git_variables_go_into_the_fingerprint() {
        let env = super::super::identity::SessionEnv {
            set: vec![
                ("HTTPS_PROXY".into(), "http://a".into()),
                ("GIT_TRACE".into(), "1".into()),
                ("GIT_SSL_CAINFO".into(), "x".into()),
                ("NODE_EXTRA_CA_CERTS".into(), "c.pem".into()),
            ],
            remove: Vec::new(),
        };
        let watched: Vec<String> = watched_env(&env).into_iter().map(|(w, _)| w).collect();
        assert!(watched.contains(&"the variable HTTPS_PROXY".to_string()), "{watched:?}");
        assert!(watched.contains(&"the variable NODE_EXTRA_CA_CERTS".to_string()));
        // What the run never gets is not watched.
        assert!(!watched.iter().any(|w| w.contains("GIT_TRACE") || w.contains("GIT_SSL_CAINFO")));
        let other = super::super::identity::SessionEnv {
            set: vec![("HTTPS_PROXY".into(), "http://b".into())],
            remove: Vec::new(),
        };
        let a = watched_env(&env).into_iter().find(|(w, _)| w.ends_with("HTTPS_PROXY"));
        let b = watched_env(&other).into_iter().find(|(w, _)| w.ends_with("HTTPS_PROXY"));
        assert_ne!(a, b);
    }

    #[test]
    fn a_scoped_package_with_its_own_scope_modules_is_refused() {
        let root = scratch("scoped");
        let modules = root.join("npm").join("node_modules");
        let package = modules.join("@netlify").join("cli");
        std::fs::create_dir_all(&package).unwrap();
        let shim = ShimRun {
            node: root.join("node.exe"),
            script: package.join("bin.js"),
            package: package.clone(),
            modules: modules.clone(),
        };
        let none = |_: &str| None;
        assert_eq!(node_escape(&shim, &none), None);
        std::fs::create_dir_all(modules.join("@netlify").join("node_modules")).unwrap();
        assert!(node_escape(&shim, &none).is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pinned_tree_changes_with_any_file_in_it() {
        let root = std::env::temp_dir().join(format!("horadric-pin-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("ext/deep")).unwrap();
        std::fs::write(root.join("ext/deep/a.js"), "one").unwrap();
        std::fs::write(root.join("config.yml"), "aliases: {}").unwrap();
        let pins = vec![
            Pin { what: "c".into(), path: root.join("config.yml").to_string_lossy().into(), kind: PinKind::File },
            Pin { what: "e".into(), path: root.join("ext").to_string_lossy().into(), kind: PinKind::Tree },
            Pin { what: "m".into(), path: root.join("missing").to_string_lossy().into(), kind: PinKind::Tree },
        ];
        let first = pin_hashes(&pins).unwrap();
        assert_eq!(first[2].1, "absent");
        assert_eq!(pin_hashes(&pins).unwrap(), first);
        std::fs::write(root.join("ext/deep/a.js"), "two").unwrap();
        let second = pin_hashes(&pins).unwrap();
        assert_ne!(second[1], first[1]);
        assert_eq!(second[0], first[0]);
        std::fs::write(root.join("ext/new.js"), "").unwrap();
        assert_ne!(pin_hashes(&pins).unwrap()[1], second[1], "a new empty file counts");
        // A file pin that is a folder cannot be read as one.
        let odd = [Pin { what: "x".into(), path: root.join("ext").to_string_lossy().into(), kind: PinKind::File }];
        assert!(pin_hashes(&odd).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_question_per_session_at_a_time() {
        let first = claim("test-session-a").expect("free");
        assert!(claim("test-session-a").is_none());
        let other = claim("test-session-b");
        assert!(other.is_some());
        drop(first);
        assert!(claim("test-session-a").is_some());
    }

    #[test]
    fn owners_are_named_in_the_question() {
        assert_eq!(owned("T", &Owner::Everywhere), "T (kept for every project)");
        assert_eq!(owned("T", &Owner::Project("c:/work/optipeople".into())), "optipeople's T");
    }
}
