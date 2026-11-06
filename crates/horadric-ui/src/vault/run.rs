//! `horadric secret run`, `set` and `list` in the app: the key each session
//! is born with, the question to the human, and the program started with
//! the values in its environment and its output masked on the way back.
//! The values never leave the app. See docs/plan/vault.md.

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
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
    approval, approved, log_line, missing_line, once_only, valid_name, Mask, Owner,
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

/// The keys, read from `keys.dat` the first time. A file that will not
/// open costs only a restart of the sessions it held, so it is read as
/// none and written over.
fn with_keys<R>(f: impl FnOnce(&mut HashMap<String, String>) -> R) -> R {
    let mut keys = KEYS.lock().unwrap_or_else(|e| e.into_inner());
    let keys = keys.get_or_insert_with(|| {
        keys_path()
            .and_then(|p| store::read_sealed(&p).ok().flatten())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    });
    f(keys)
}

fn save_keys(keys: &HashMap<String, String>) {
    if let (Some(path), Ok(bytes)) = (keys_path(), serde_json::to_vec(keys)) {
        let _ = store::write_sealed(&path, &bytes);
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

fn put(_session: &str, title: &str, text: &str, buttons: Vec<String>) -> Asked {
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
fn hash(file: &mut File) -> Option<String> {
    let mut h = BCRYPT_HASH_HANDLE::default();
    unsafe { BCryptCreateHash(BCRYPT_SHA256_ALG_HANDLE, &mut h, None, None, 0) }
        .ok()
        .ok()?;
    let mut buf = vec![0u8; 1 << 20];
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
    const FILE_ADD_FILE: u32 = 0x2;
    const FILE_ADD_SUBDIRECTORY: u32 = 0x4;
    const FILE_DELETE_CHILD: u32 = 0x40;
    let file = FILE_WRITE_DATA | FILE_APPEND_DATA | DELETE | WRITE_DAC | WRITE_OWNER;
    let folder = FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY | FILE_DELETE_CHILD | WRITE_DAC | WRITE_OWNER;
    may(program, file) || program.parent().is_none_or(|dir| may(dir, folder))
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

/// Why Always is not offered, None when it is.
fn once_why(pinned: &Pinned, place: &Place) -> Option<&'static str> {
    let path = &pinned.path;
    if once_only(&path.to_string_lossy()) {
        return Some("it can run code the agent writes");
    }
    if pinned.remote {
        return Some("it is on a network share");
    }
    if inside(path, &place.dir) || inside(path, &place.cwd) || inside(path, &std::env::temp_dir()) {
        return Some("it is inside the project or the temp folder, where the agent writes");
    }
    if changeable(path) {
        return Some("you can change it or its folder without elevation, so the agent could too");
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
    let shown = horadric_pty::command_line(&pinned.path, &args);
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
    let unasked: Vec<String> = secrets
        .iter()
        .filter(|s| !approved(&s.allowed, &place.key, &program_lower, &pinned.sha256))
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
        let why_once = once_why(&pinned, &place);
        let stem = pinned
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let whose: Vec<String> = secrets
            .iter()
            .filter(|s| unasked.contains(&s.name))
            .map(|s| owned(&s.name, &s.owner))
            .collect();
        let title = format!("Use {} for `{stem}`?", unasked.join(", "));
        let mut text = format!(
            "{} asks to run this with {} in its environment:\n\n{shown}\n\nProgram: {}",
            asker(&session, Some(&crate::project::project_name(&place.key))),
            whose.join(", "),
            pinned.path.display(),
        );
        if let Some(why) = why_once {
            text.push_str(&format!("\n\nOnly Once is offered: {why}."));
        }
        Some(ask(&session, &title, &text, why_once.is_none()))
    };
    let job = Job {
        session,
        project: place.key,
        pinned,
        program_lower,
        args,
        cwd,
        env: super::identity::session_env(&place.dir),
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

    fn run(self, asked: Option<Asked>, held: Option<Claim>, out: SecretStream, left: Arc<AtomicBool>) {
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
        let granted = approval(&self.project, &self.program_lower, &self.pinned.sha256);
        // An Always for this program in this project from before it changed
        // is spent: the new one replaces it.
        let stale = format!("|{}|{}", self.project, self.program_lower);
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

    fn command(&self) -> Command {
        let program = &self.pinned.path;
        let mut c = if horadric_pty::is_batch(program) {
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
        let mut child = match self.command().spawn() {
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

/// `horadric secret set`: keeps a value once the human says so in the
/// app, since an agent could otherwise swap a secret for a value of its
/// own. Call it on the UI thread; it returns at once, and all the work,
/// git included, is done on a thread of its own.
pub fn set(req: SecretSet) {
    waker();
    thread::spawn(move || set_off_ui(req));
}

fn set_off_ui(req: SecretSet) {
    let SecretSet {
        session,
        key,
        cwd,
        name,
        value,
        owner,
        reply,
    } = req;
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
    if value.is_empty() {
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
            let key = crate::project::folder_key(&project_top(&cwd));
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
    let text = format!(
        "{} asks to keep {name} for {whose}{}. Keep it only if you typed it yourself.",
        asker(&session, None),
        if replaces {
            ", replacing the value kept now and every Always given to it"
        } else {
            ""
        }
    );
    let asked = put(&session, &title, &text, vec!["Keep".into(), "No".into()]);
    let project = match &owner {
        Owner::Project(k) => k.clone(),
        o => o.label(),
    };
    let pressed = match asked.until(SET_WAIT, None) {
        Waited::Pressed(p) => p,
        _ => None,
    };
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
