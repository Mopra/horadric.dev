//! The desktop tools' reach into programs that run as administrator.
//!
//! Windows shows a session that is not elevated only the title bar of an
//! elevated program and drops the input it sends there. Some programs,
//! engineering tools among them, restart themselves as administrator however
//! they are started, so the user can not simply run them without. For a
//! window like that `horadric mcp` starts `horadric takeover-helper` as
//! administrator (Windows asks the user, unless its settings elevate
//! without asking) and hands it the whole desktop call over a named pipe.
//!
//! The session at the other end of that pipe is not elevated, and any
//! program of the user can start the helper and stand in for the session,
//! or for the app the session asks. So the helper takes nobody's word for
//! what it may do:
//!
//! - It is bound, when it starts, to one program: the full path of its
//!   file, on the command line Windows shows the human before the yes. It
//!   acts only in windows whose process it finds, by its own lookup, to
//!   run that file. Another elevated program takes another helper and
//!   another yes.
//! - Before it first operates that program, and before every risky
//!   action, it asks the human itself, in a window of its own elevated
//!   process ([`ask`]). Only the administrators' list, which a program
//!   that is not elevated can not write, answers in the human's place.
//! - What is risky it judges itself, from what its own engine read of the
//!   element, the window and the dialog ([`own_risk`]). Anything in a
//!   file dialog is: from there other programs can be started with the
//!   program's rights, so operating one elevated program can not be
//!   contained to it, and the first question says so.
//! - Bypass, which the human turns on in the app to watch a session work
//!   unasked, reaches it only as a claim: before a question, the helper
//!   asks the app (`desktop.bypass`) when bypass ends for this session.
//!   A claim is no answer. The first time one would spare a question,
//!   the helper asks the human in its own window whether this bypass
//!   holds in its program too, until the end the claim names, and says
//!   what that lets in. Only after that yes does it let actions through
//!   unasked, and only while the claim still stands, names the same end,
//!   and its own clock is before that end and at most
//!   [`listed::BYPASS_MOST`] after the yes. So a program that forges a
//!   claim gets no further than a question the human can refuse: a no
//!   means each action is asked as before for that bypass, and a new end
//!   is a new bypass and a new question. What a forger can do is keep
//!   claiming a bypass the human did say yes to after the app ended it,
//!   up to that end, as the session itself could while it lasted.
//!
//! The app is still asked, and told how each action went, through the MCP
//! process, so the human watches it as any other program: its no still
//! refuses, its yes adds nothing to the helper's own.
//!
//! The pipes take one client each, never a remote one, and each end checks
//! the other is the process it expects. There are two, one each way: the session reads the
//! helper's lines on a thread of its own, so a call can time out, and a
//! synchronous pipe serialises everything on one file object, so a write
//! sharing it with that waiting read would wait for ever.
//!
//! Windows' question is the human's to answer, so it is announced first
//! (the app shows why it comes) and never holds the session hostage: the
//! start runs on a thread of its own, a call waits for it only so long,
//! and a later call picks up the helper that connected meanwhile. A no is
//! remembered for a while, and so is a helper that failed or was lost,
//! so the agent can not have Windows ask the human again and again. The
//! helper is stopped when it goes silent past any call's length, and it
//! ends by itself when the session that started it does.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use horadric_core::takeover_programs::{self as listed, Act, Level, Question, Refusals, Source};
use serde_json::{json, Value};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_CANCELLED, ERROR_PIPE_CONNECTED, HANDLE, HWND, LPARAM,
};
use windows::Win32::Security::{
    AddAccessAllowedAce, CopySid, CreateWellKnownSid, GetLengthSid, GetTokenInformation,
    InitializeAcl, InitializeSecurityDescriptor, SetSecurityDescriptorDacl, TokenUser,
    WinBuiltinAdministratorsSid, ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, PSECURITY_DESCRIPTOR, PSID,
    SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND,
    PIPE_ACCESS_OUTBOUND, SECURITY_IDENTIFICATION,
};
use windows::Win32::System::Com::{
    CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetProcessId, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    TerminateProcess, WaitForSingleObject, INFINITE, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GetClassNameW, GetWindowTextW, GetWindowThreadProcessId,
    IsWindowVisible, SW_HIDE,
};

use crate::takeover::{
    file_name_field, list_windows, looks_risky, pick_window, typed_risk, Takeover,
};
use crate::takeover_programs::takeover_admin;

/// How long the helper may take to connect once Windows has started it.
/// The user's answer to Windows' question comes before, and is timed by
/// the call instead, see [`Starting`].
const START_WAIT: Duration = Duration::from_secs(60);

/// How long a call waits for the human to answer Windows' question before
/// it tells the agent the question is still open.
pub const ASK_WAIT: Duration = Duration::from_secs(90);
/// The same for a later call, while the question asked before is open.
pub const ASK_AGAIN_WAIT: Duration = Duration::from_secs(20);
/// How long after a no, or after asking for a helper that then failed or
/// was lost, before Windows may ask the human again.
pub const ASK_AGAIN_AFTER: Duration = Duration::from_secs(120);
/// How long the helper may stay silent in a call before it is taken for
/// hung and stopped: past the longest wait any desktop tool does, a
/// minute, with room for a slow program's snapshot.
pub const SILENCE: Duration = Duration::from_secs(150);
/// Given to the human to see the app's word before Windows dims the
/// screen for its question.
pub const ANNOUNCE_PAUSE: Duration = Duration::from_millis(800);

/// The helper's own call for an unnamed `takeover_wait`: what changed in its
/// programs since a mark, and whether what is waited for holds there. Not
/// a `desktop_` name, so an agent can not call it.
pub const PEEK: &str = "helper_peek";

/// One line from the helper to the MCP process.
#[derive(Debug, PartialEq)]
pub enum FromHelper {
    /// Send the app this body, an `allow` or a `done`, and hand its
    /// answer back.
    App(Value),
    /// The call is answered with this content.
    Done(Vec<Value>),
    /// The call failed, saying why.
    Fail(String),
}

/// Reads one line the helper wrote.
pub fn from_helper(line: &str) -> Result<FromHelper, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("the helper said {e}"))?;
    if let Some(body) = v.get("app") {
        return Ok(FromHelper::App(body.clone()));
    }
    if let Some(content) = v.get("done").and_then(Value::as_array) {
        return Ok(FromHelper::Done(content.clone()));
    }
    if let Some(why) = v.get("fail").and_then(Value::as_str) {
        return Ok(FromHelper::Fail(why.to_string()));
    }
    Err(format!("the helper said something unknown: {line}"))
}

/// The app's answer, as the line sent back to the helper.
pub fn answer_line(answer: &Result<Value, String>) -> String {
    match answer {
        Ok(v) => json!({ "ok": v }).to_string(),
        Err(why) => json!({ "err": why }).to_string(),
    }
}

/// The app's answer as the helper reads it back.
pub fn answer_of(line: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("Horadric said {e}"))?;
    match (v.get("ok"), v.get("err").and_then(Value::as_str)) {
        (Some(ok), _) => Ok(ok.clone()),
        (None, Some(why)) => Err(why.to_string()),
        (None, None) => Err(format!("Horadric said something unknown: {line}")),
    }
}

/// An `allow` or `done` body for a window reached through the helper,
/// marked as a program that runs as administrator. A field of its own:
/// the program's name stays its name, since a name is something a file
/// can be given.
pub fn as_administrator(mut body: Value) -> Value {
    body["administrator"] = json!(true);
    body
}

/// Why the helper did not start.
#[derive(Debug)]
pub enum HelperStart {
    /// The user said no to Windows.
    Declined(String),
    /// Anything else.
    Failed(String),
}

/// What last kept the helper from the session.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Setback {
    /// The human said no to Windows' question.
    Declined,
    /// It did not start, though not for a no.
    Failed,
    /// It was running and went away, or was stopped for going silent.
    Lost,
}

/// How long before Windows may ask the human again to start the helper,
/// none when it may now. A no or a failed start holds for
/// [`ASK_AGAIN_AFTER`] from when it happened, however long the human took
/// to answer; a loss for as long from when the human was last asked, so a
/// helper that ran a good while is started again at once and one that
/// dies as it starts is not asked for over and over.
pub fn hold_left(
    setback: Setback,
    since_setback: Duration,
    since_asked: Duration,
) -> Option<Duration> {
    let since = match setback {
        Setback::Declined | Setback::Failed => since_setback,
        Setback::Lost => since_asked,
    };
    ASK_AGAIN_AFTER
        .checked_sub(since)
        .filter(|left| !left.is_zero())
}

/// What the agent is told while a setback holds the helper back.
pub fn held_text(setback: Setback, why: &str, left: Duration) -> String {
    let secs = left.as_secs().max(1);
    match setback {
        Setback::Declined => format!(
            "the human said no when Windows asked to let Horadric's helper run as administrator, \
             and Windows is not asked again for {secs} s. Ask the human in the chat whether they \
             want you to reach this program before you try again"
        ),
        Setback::Failed | Setback::Lost => format!(
            "Horadric's helper for programs that run as administrator is not running ({why}); \
             Windows is asked to start it again in {secs} s, not sooner, so the human is not \
             asked over and over"
        ),
    }
}

/// How long after Windows last asked the human for a helper before a
/// call for another elevated program may have it asked again. Each
/// elevated program takes a helper of its own, one at a time, and an agent
/// going back and forth between two must not have Windows ask each time.
pub const SWITCH_AFTER: Duration = Duration::from_secs(60);

/// How long a call for another elevated program still waits before
/// Windows may be asked for its helper, none when it may now.
pub fn switch_left(since_asked: Duration) -> Option<Duration> {
    SWITCH_AFTER
        .checked_sub(since_asked)
        .filter(|left| !left.is_zero())
}

/// What the agent is told when the helper it has serves another program.
pub fn switch_text(serves: &str, wanted: &str, left: Duration) -> String {
    format!(
        "Horadric's helper runs for {serves} alone, and {wanted} takes a helper of its own, \
         which ends that one and has Windows ask the human again. That is not done for another \
         {} s, so the human is not asked over and over",
        left.as_secs().max(1)
    )
}

/// What the agent is told when Windows still asks about a helper for
/// another program than the one it now calls.
pub fn asking_other_text(asked_for: &str, wanted: &str) -> String {
    format!(
        "Windows is still asking the human about Horadric's helper for {asked_for}, so none is \
         asked for {wanted} yet. Nothing was done. Call again once they have answered"
    )
}

/// What the agent is told while Windows' question is still open.
pub fn still_asking_text(program: &str, asked: Duration) -> String {
    format!(
        "Windows is still asking the human whether Horadric's helper may run as administrator, \
         to reach {program} (asked {} s ago). Nothing was done. Call again once they have \
         answered: the helper is picked up then, without asking again",
        asked.as_secs()
    )
}

/// What the app is told just before Windows asks the human to let the
/// helper run as administrator, so the human knows why it asks, and which
/// file Windows is about to start when that is not the installed
/// Horadric.
pub fn elevating_body(window: Value) -> Value {
    let mut body = json!({ "op": "desktop.elevating", "window": window });
    if let Ok(exe) = helper_exe() {
        if dev_build(&exe, horadric_hooks::dev()) {
            body["dev_helper"] = json!(exe.display().to_string());
        }
    }
    body
}

/// The file started as the helper: this program's own.
fn helper_exe() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // A test binary sits in target\<profile>\deps, beside no
    // `takeover-helper` command, so the by hand tests use the built one.
    #[cfg(test)]
    let exe = exe
        .parent()
        .and_then(|deps| deps.parent())
        .map(|dir| dir.join("horadric.exe"))
        .unwrap_or(exe);
    Ok(exe)
}

/// Whether `exe` is a build from a source tree, or this is a dev
/// instance: then the human is told which file asks for administrator
/// rights, since it is not the Horadric they installed.
pub fn dev_build(exe: &std::path::Path, dev_instance: bool) -> bool {
    dev_instance
        || exe
            .components()
            .any(|c| c.as_os_str().eq_ignore_ascii_case("target"))
}

/// A start of the helper on its own thread, Windows' question and all.
pub struct Starting {
    /// How the start ended, and when.
    result: Receiver<(Result<Helper, HelperStart>, Instant)>,
    /// When Windows was asked.
    pub asked: Instant,
    /// The program the helper is asked for.
    pub program: String,
}

impl Starting {
    /// Asks Windows to start the helper for `program`, the full path of
    /// the one program it will act in, and returns while it asks.
    pub fn begin(program: &str) -> Starting {
        let (tx, rx) = mpsc::channel();
        let bound = program.to_string();
        thread::spawn(move || {
            // ShellExecuteEx wants COM, apartment threaded, on its thread.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
            }
            let started = Helper::start(&bound);
            let _ = tx.send((started, Instant::now()));
        });
        Starting {
            result: rx,
            asked: Instant::now(),
            program: program.to_string(),
        }
    }

    /// The helper, or why there is none, and when the start ended, once
    /// it is over; none while Windows still asks after waiting `most`.
    pub fn wait(&self, most: Duration) -> Option<(Result<Helper, HelperStart>, Instant)> {
        match self.result.recv_timeout(most) {
            Ok(r) => Some(r),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some((
                Err(HelperStart::Failed(
                    "the helper's start ended without a word".into(),
                )),
                Instant::now(),
            )),
        }
    }
}

/// A desktop call as the MCP process sends it.
pub fn request_line(name: &str, args: &Value, settle: Duration) -> String {
    json!({ "name": name, "args": args, "settle_ms": settle.as_millis() as u64 }).to_string()
}

/// The names of the two pipes to the helper: the one the session writes
/// down and the one the helper writes up.
pub fn pipe_names(base: &str) -> (String, String) {
    (format!("{base}-down"), format!("{base}-up"))
}

/// Every access to a pipe.
const PIPE_ALL_ACCESS: u32 = 0x001F_01FF;
const SECURITY_DESCRIPTOR_REVISION: u32 = 1;

/// A security descriptor letting only this user and elevated
/// administrators open a pipe: not other users' programs, nor anonymous
/// ones, which a pipe's default lets read. Administrators, because
/// Windows may run the helper as another account when a standard user
/// types an administrator's password, and an elevated administrator can
/// do anything a pipe guards anyway. Boxed, as the pointers inside point
/// into it.
struct OwnersOnly {
    sd: SECURITY_DESCRIPTOR,
    _acl: Vec<u64>,
    _sids: [Vec<u64>; 2],
}

impl OwnersOnly {
    fn new() -> Result<Box<OwnersOnly>, String> {
        unsafe {
            let user = Self::user_sid()?;
            let admins = Self::admins_sid()?;
            let sid = |b: &Vec<u64>| PSID(b.as_ptr() as *mut _);
            let len = std::mem::size_of::<ACL>()
                + 2 * std::mem::size_of::<ACCESS_ALLOWED_ACE>()
                + GetLengthSid(sid(&user)) as usize
                + GetLengthSid(sid(&admins)) as usize;
            let mut acl = vec![0u64; len.div_ceil(8)];
            let pacl = acl.as_mut_ptr() as *mut ACL;
            let e = |e: windows::core::Error| format!("could not secure the helper's pipes: {e}");
            InitializeAcl(pacl, len as u32, ACL_REVISION).map_err(e)?;
            AddAccessAllowedAce(pacl, ACL_REVISION, PIPE_ALL_ACCESS, sid(&user)).map_err(e)?;
            AddAccessAllowedAce(pacl, ACL_REVISION, PIPE_ALL_ACCESS, sid(&admins)).map_err(e)?;
            let mut me = Box::new(OwnersOnly {
                sd: SECURITY_DESCRIPTOR::default(),
                _acl: acl,
                _sids: [user, admins],
            });
            let psd = PSECURITY_DESCRIPTOR(&mut me.sd as *mut _ as *mut _);
            InitializeSecurityDescriptor(psd, SECURITY_DESCRIPTOR_REVISION).map_err(e)?;
            SetSecurityDescriptorDacl(psd, true, Some(pacl), false).map_err(e)?;
            Ok(me)
        }
    }

    /// This process's user's SID, in a buffer of its own.
    unsafe fn user_sid() -> Result<Vec<u64>, String> {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|e| format!("could not read who runs this session: {e}"))?;
        let mut len = 0;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        let read = GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            len,
            &mut len,
        );
        let _ = CloseHandle(token);
        read.map_err(|e| format!("could not read who runs this session: {e}"))?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let sid = user.User.Sid;
        let n = GetLengthSid(sid) as usize;
        let mut own = vec![0u64; n.div_ceil(8)];
        CopySid(n as u32, PSID(own.as_mut_ptr() as *mut _), sid)
            .map_err(|e| format!("could not read who runs this session: {e}"))?;
        Ok(own)
    }

    /// The SID of the local Administrators group.
    unsafe fn admins_sid() -> Result<Vec<u64>, String> {
        let mut len = 0u32;
        let _ = CreateWellKnownSid(WinBuiltinAdministratorsSid, None, None, &mut len);
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        CreateWellKnownSid(
            WinBuiltinAdministratorsSid,
            None,
            Some(PSID(buf.as_mut_ptr() as *mut _)),
            &mut len,
        )
        .map_err(|e| format!("could not name the administrators: {e}"))?;
        Ok(buf)
    }
}

/// The helper's end of a pipe, `write` for the pipe up. It lets the
/// session only identify it, not act as it: the helper is elevated, and
/// a session that could impersonate it would be too.
fn open_client(name: &str, write: bool) -> std::io::Result<File> {
    OpenOptions::new()
        .read(!write)
        .write(write)
        .security_qos_flags(SECURITY_IDENTIFICATION.0)
        .open(name)
}

/// The session's end of a one way pipe, `inbound` when the session reads.
fn serve(name: &str, inbound: bool) -> Result<File, String> {
    let access: FILE_FLAGS_AND_ATTRIBUTES = if inbound {
        PIPE_ACCESS_INBOUND
    } else {
        PIPE_ACCESS_OUTBOUND
    };
    let owners = OwnersOnly::new()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: &owners.sd as *const _ as *mut _,
        bInheritHandle: false.into(),
    };
    let pipe = unsafe {
        CreateNamedPipeW(
            &HSTRING::from(name),
            access | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            1 << 16,
            1 << 16,
            0,
            Some(&attributes),
        )
    };
    if pipe.is_invalid() {
        return Err(format!(
            "could not open a pipe for the helper: {}",
            windows::core::Error::from_thread()
        ));
    }
    Ok(unsafe { File::from_raw_handle(pipe.0 as _) })
}

/// Waits for the client of the pipe the raw handle `raw` names.
fn connect(raw: usize) -> bool {
    match unsafe { ConnectNamedPipe(HANDLE(raw as _), None) } {
        Ok(()) => true,
        Err(e) => e.code() == ERROR_PIPE_CONNECTED.to_hresult(),
    }
}

/// The helper, running as administrator, with the pipes to it.
pub struct Helper {
    /// Its lines, read on a thread of their own so a call can stop
    /// waiting for a helper that hangs.
    lines: Receiver<std::io::Result<String>>,
    /// The pipe down, its only handle: dropping it ends the helper.
    writer: File,
    process: HANDLE,
    /// The one program it was started for and acts in.
    program: String,
}

// The process handle is only waited on, stopped and closed, which any
// thread may do, and the helper is made on the thread that starts it.
unsafe impl Send for Helper {}

impl Drop for Helper {
    fn drop(&mut self) {
        // The pipe down closes with `writer`, which ends the helper once it
        // is back reading it, and nothing waits on that pipe, so closing
        // never waits. The reading thread ends when the helper does.
        unsafe {
            let _ = CloseHandle(self.process);
        }
    }
}

impl Helper {
    /// The program this helper acts in.
    pub fn program(&self) -> &str {
        &self.program
    }

    /// Whether this helper is the one for the program at `path`.
    pub fn serves(&self, path: &str) -> bool {
        same_program(&self.program, path)
    }

    /// Starts the helper as administrator for `program` and waits for it
    /// to connect.
    pub fn start(program: &str) -> Result<Helper, HelperStart> {
        Self::start_inner(program).map_err(|e| match e {
            Ok(declined) => HelperStart::Declined(declined),
            Err(failed) => HelperStart::Failed(failed),
        })
    }

    /// The outer error is `Ok` when the user declined.
    fn start_inner(program: &str) -> Result<Helper, Result<String, String>> {
        if let Some(why) = unbindable(program) {
            return Err(Err(why));
        }
        let me = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let name = format!(r"\\.\pipe\horadric-desktop-{me}-{nanos}");
        let (down_name, up_name) = pipe_names(&name);
        // Owned from here, so every way out closes them.
        let down = serve(&down_name, false).map_err(Err)?;
        let up = serve(&up_name, true).map_err(Err)?;

        let exe = HSTRING::from(helper_exe().map_err(Err)?.as_os_str());
        // The program goes on the command line, which Windows shows the
        // human under "Show more details" before the yes, and which is all
        // the helper will ever act in. No path holds a quote, so quoting
        // it is enough.
        let params = HSTRING::from(format!(
            "takeover-helper {name} {me} \"{}\"",
            program.trim()
        ));
        let verb = HSTRING::from("runas");
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(exe.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        // Returns only once the human has answered Windows' question,
        // which is why this runs on a thread of its own.
        if let Err(e) = unsafe { ShellExecuteExW(&mut info) } {
            return Err(if e.code() == ERROR_CANCELLED.to_hresult() {
                Ok("the user declined to let the helper run as administrator".into())
            } else {
                Err(format!("could not start the helper as administrator: {e}"))
            });
        }
        let process = info.hProcess;
        let helper_pid = unsafe { GetProcessId(process) };

        let raws = (down.as_raw_handle() as usize, up.as_raw_handle() as usize);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(connect(raws.0) && connect(raws.1));
        });
        let started = Instant::now();
        let connected = loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(ok) => break ok,
                Err(mpsc::RecvTimeoutError::Disconnected) => break false,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let gone = unsafe { WaitForSingleObject(process, 0) }.0 == 0;
            if gone || started.elapsed() > START_WAIT {
                // Connecting ourselves frees the thread waiting on the pipes.
                let _ = open_client(&down_name, false);
                let _ = open_client(&up_name, true);
                let _ = rx.recv_timeout(Duration::from_secs(1));
                unsafe {
                    let _ = CloseHandle(process);
                }
                return Err(Err(if gone {
                    "the helper ended before it connected".into()
                } else {
                    "the helper did not connect in time".into()
                }));
            }
        };
        let client = |f: &File| {
            let mut pid = 0u32;
            unsafe { GetNamedPipeClientProcessId(HANDLE(f.as_raw_handle()), &mut pid) }
                .ok()
                .map(|()| pid)
        };
        if !connected || client(&down) != Some(helper_pid) || client(&up) != Some(helper_pid) {
            unsafe {
                let _ = CloseHandle(process);
            }
            return Err(Err(
                "something other than the helper answered on its pipe".into()
            ));
        }
        Ok(Helper::over(up, down, process, program.trim()))
    }

    /// The helper on its connected pipes, `up` read on a thread of its
    /// own.
    fn over(up: File, down: File, process: HANDLE, program: &str) -> Helper {
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(up);
            loop {
                let mut line = String::new();
                let read = reader.read_line(&mut line);
                let end = !matches!(read, Ok(n) if n > 0);
                if tx.send(read.map(|_| line)).is_err() || end {
                    break;
                }
            }
        });
        Helper {
            lines,
            writer: down,
            process,
            program: program.to_string(),
        }
    }

    /// Stops a helper that went silent. Should Windows not let this
    /// session stop it, closing the pipe down on drop ends it as soon as
    /// it is out of what it was doing.
    fn stop(&self) {
        unsafe {
            let _ = TerminateProcess(self.process, 1);
        }
    }

    /// Runs one desktop call in the helper. An error that is not the
    /// helper's own answer means the helper is gone.
    pub fn call<A: FnMut(Value) -> Result<Value, String>>(
        &mut self,
        name: &str,
        args: &Value,
        app: &mut A,
        settle: Duration,
    ) -> Result<Result<Vec<Value>, String>, String> {
        self.call_within(name, args, app, settle, SILENCE)
    }

    /// As [`Helper::call`], for a call the helper answers at once (a look
    /// at its own ring), which gives it `silence` and not the long limit
    /// of a call that operates: a wait must not last minutes on a helper
    /// that hangs.
    pub fn call_within<A: FnMut(Value) -> Result<Value, String>>(
        &mut self,
        name: &str,
        args: &Value,
        app: &mut A,
        settle: Duration,
        silence: Duration,
    ) -> Result<Result<Vec<Value>, String>, String> {
        let lost = |e: std::io::Error| format!("lost the helper: {e}");
        writeln!(self.writer, "{}", request_line(name, args, settle)).map_err(lost)?;
        loop {
            // The clock runs only while the helper has the call: the app's
            // questions to the human are waited for here, not there.
            let line = match self.lines.recv_timeout(silence) {
                Ok(Ok(line)) if !line.is_empty() => line,
                Ok(Ok(_)) | Err(RecvTimeoutError::Disconnected) => {
                    return Err("the helper ended".into())
                }
                Ok(Err(e)) => return Err(lost(e)),
                Err(RecvTimeoutError::Timeout) => {
                    self.stop();
                    return Err(format!(
                        "the helper did not answer in {} s, so it was stopped",
                        silence.as_secs()
                    ));
                }
            };
            match from_helper(line.trim_end())? {
                FromHelper::App(body) => {
                    let answer = app(body);
                    writeln!(self.writer, "{}", answer_line(&answer)).map_err(lost)?;
                }
                FromHelper::Done(content) => return Ok(Ok(content)),
                FromHelper::Fail(why) => return Ok(Err(why)),
            }
        }
    }
}

/// `horadric takeover-helper <pipe> <pid> <program>`: serves desktop calls
/// from the MCP process `pid`, in the windows of `program` alone, until
/// the session closes the pipe.
pub fn run(args: &[String]) -> Result<(), String> {
    let (name, parent, program) = match args {
        [name, pid, program] => (
            name,
            pid.parse::<u32>().map_err(|e| e.to_string())?,
            program.trim().to_string(),
        ),
        _ => {
            return Err("usage: horadric takeover-helper <pipe> <pid> <program's full path>".into())
        }
    };
    if !name.starts_with(r"\\.\pipe\horadric-desktop-") {
        return Err("not a desktop helper pipe".into());
    }
    if let Some(why) = unbindable(&program) {
        return Err(why);
    }
    // Held from before the pipe's check, so the process watched is the
    // one checked, not another given its id later.
    let session = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, parent) }
        .map_err(|e| format!("could not watch the session that started this helper: {e}"))?;
    let (down_name, up_name) = pipe_names(name);
    let down =
        open_client(&down_name, false).map_err(|e| format!("could not open {down_name}: {e}"))?;
    let up = open_client(&up_name, true).map_err(|e| format!("could not open {up_name}: {e}"))?;
    for pipe in [&down, &up] {
        let mut server = 0u32;
        unsafe { GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &mut server) }
            .map_err(|e| e.to_string())?;
        if server != parent {
            return Err("the pipe is not held by the session that started this helper".into());
        }
    }
    // The pipe closing ends the helper only once it is back reading it,
    // and an elevated process must not linger past its session in the
    // middle of a long call either.
    let raw = session.0 as usize;
    thread::spawn(move || {
        unsafe { WaitForSingleObject(HANDLE(raw as _), INFINITE) };
        std::process::exit(0);
    });
    // Its own question is laid out in pixels, whichever call comes first.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let mut writer = up;
    let mut reader = BufReader::new(down);
    let mut takeover = Takeover::for_helper();
    let mut gate = Gate::new(program, parent);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Ok(());
        }
        let req: Value = serde_json::from_str(line.trim_end()).map_err(|e| e.to_string())?;
        let name = req.get("name").and_then(Value::as_str).unwrap_or_default();
        let call_args = req.get("args").cloned().unwrap_or(Value::Null);
        let settle =
            Duration::from_millis(req.get("settle_ms").and_then(Value::as_u64).unwrap_or(0));
        // The engine reads a window before it asks to act in it, so one
        // that is not the helper's program is turned away before that.
        // The gate checks again, by the window acted in, at every allow.
        let stray = match call_args.get("window") {
            Some(Value::String(q)) => Some(q.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        }
        .and_then(|query| pick_window(&list_windows(), &query).cloned())
        .filter(|w| !same_program(&w.path, &gate.program));
        if let Some(w) = stray {
            let why = not_bound_text(&gate.program, &w.path);
            gate.note(&format!("refused by the helper: {why}"), name);
            let out = json!({ "fail": why });
            writeln!(writer, "{out}").map_err(|e| e.to_string())?;
            continue;
        }
        let mut broken = None;
        let mut peer = |body: Value| -> Result<Value, String> {
            writeln!(writer, "{}", json!({ "app": body })).map_err(|e| e.to_string())?;
            let mut answer = String::new();
            match reader.read_line(&mut answer) {
                Ok(n) if n > 0 => answer_of(answer.trim_end()),
                Ok(_) => {
                    broken = Some("the session closed the pipe".to_string());
                    Err("the session is gone".into())
                }
                Err(e) => {
                    broken = Some(e.to_string());
                    Err("the session is gone".into())
                }
            }
        };
        let mut app = |body: Value| gate.through(body, &mut peer);
        let result = takeover.tool(name, &call_args, &mut app, settle);
        if let Some(why) = broken {
            return Err(why);
        }
        let out = match result {
            Ok(content) => json!({ "done": content }),
            Err(why) => json!({ "fail": why }),
        };
        writeln!(writer, "{out}").map_err(|e| e.to_string())?;
    }
}

/// What the agent is told when a helper for one program is asked to act
/// in another.
pub fn not_bound_text(bound: &str, other: &str) -> String {
    format!(
        "this helper was started for {bound} alone, and that window belongs to {other}. Nothing \
         was done. Another program that runs as administrator takes a helper of its own, which \
         Windows asks the human about"
    )
}

/// Whether two paths name the same file as written: case and the way the
/// slashes lean aside. Two spellings of one file (a short name, a mapped
/// drive) do not match, which only ever refuses.
pub fn same_program(a: &str, b: &str) -> bool {
    let flat = |p: &str| p.trim().replace('/', "\\").to_lowercase();
    !a.trim().is_empty() && flat(a) == flat(b)
}

/// Why a program can not have a helper bound to it, if it can not.
pub fn unbindable(program: &str) -> Option<String> {
    let p = program.trim();
    let absolute = std::path::Path::new(p).is_absolute();
    if p.is_empty() || !absolute || p.contains('"') {
        return Some(
            "Windows would not tell which program that window belongs to, so no helper can be \
             bound to it and nothing was done"
                .into(),
        );
    }
    listed::is_horadric(p).then(|| {
        "Horadric's own windows are never operated: a session that could would answer its own \
         questions"
            .into()
    })
}

/// The names of the buttons that end a file dialog, in the languages the
/// machines here run, without the access key's mark.
const FILE_DIALOG_BUTTONS: &[&str] = &["open", "save", "åbn", "gem", "öffnen", "speichern"];

/// The window classes Windows builds its Open, Save and folder dialogs
/// from: the shell's own view of a folder, in whatever window holds it.
const FILE_DIALOG_CLASSES: &[&str] = &[
    "DUIViewWndClassName",
    "SHELLDLL_DefView",
    "SHBrowseForFolder ShellNameSpace Control",
];

/// Whether a window class is part of a file dialog.
pub fn file_dialog_class(class: &str) -> bool {
    FILE_DIALOG_CLASSES.contains(&class)
}

/// Why the helper takes an action for risky, if it does. It judges from
/// what it read itself: the parts of the action its own engine built from
/// the element, the window and a small dialog's text, and whether a file
/// dialog of the program is open. The session's word can only add to it
/// (`said`), never take from it.
///
/// Anything while a file dialog is open is risky, always: from there a
/// program can be started with this one's administrator rights (a path
/// typed into the name box, a right click in the file list), so that is
/// where operating one program stops being only that program.
pub fn own_risk(said: bool, act: &Act, file_dialog: bool) -> Option<&'static str> {
    let plain: String = act
        .name
        .chars()
        .filter(|c| *c != '&' && *c != '_')
        .collect::<String>()
        .trim()
        .trim_end_matches("...")
        .to_lowercase();
    let ends_dialog = act.control == "Button" && FILE_DIALOG_BUTTONS.contains(&plain.as_str());
    let name_box = ["Edit", "ComboBox"]
        .iter()
        .any(|control| file_name_field(control, &act.name));
    if file_dialog || ends_dialog || name_box {
        return Some(
            "it is in an Open or Save dialog, or on its button, and such a dialog can start \
             other programs with this program's administrator rights",
        );
    }
    // Typed into a program with administrator rights, a path or a command
    // line is a program started with them wherever the field is.
    if act.kind == Some(listed::Kind::Type) && typed_risk(&act.typed, true).is_some() {
        return Some(
            "the text names a path, a program or a command, and typed into a program that runs \
             as administrator it can start another with its rights",
        );
    }
    let mut texts = vec![
        act.name.as_str(),
        act.window.as_str(),
        act.key.as_str(),
        act.typed.as_str(),
    ];
    texts.extend(act.texts.iter().map(String::as_str));
    (said || looks_risky(&texts))
        .then_some("it may change the machine, the project or files, and can not be taken back")
}

/// The parts of an operate action, read from the body the helper's own
/// engine built.
fn act_of(v: Option<&Value>) -> Act {
    let Some(v) = v else {
        return Act::default();
    };
    let text = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Act {
        kind: v
            .get("kind")
            .and_then(Value::as_str)
            .and_then(|k| listed::Kind::parse(k).ok()),
        window: text("window"),
        control: text("control"),
        name: text("name"),
        key: text("key"),
        texts: v
            .get("texts")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        typed: text("typed"),
    }
}

/// The helper's first question, before it operates its program at all.
pub fn first_question(program: &str, what: &str, within: u64) -> String {
    format!(
        "An AI agent in a Horadric session asks to operate this program, which runs as \
         administrator:\n\n{program}\n\nFirst it wants to {what}.\n\nOperating is clicking and \
         typing in the program and its dialogs, with administrator rights. It can not be limited \
         to this program: from its Open and Save dialogs, and from whatever else the program \
         offers, other programs can be started with the same rights. Risky actions and \
         everything in an Open or Save dialog are asked here one by one.\n\nYes lasts until this \
         helper ends with its session. This window answers to the mouse only. Unanswered in \
         {within} s, it is refused."
    )
}

/// Why a call was not asked about in the helper's window: the question,
/// with the whole text a call types, is taller than the screen.
pub const TOO_TALL: &str = "the helper's question, with the whole text to type, does not fit on \
    this screen, so it was not asked and nothing was done; type the text in smaller parts";

/// The helper's question before one risky action.
pub fn risky_question(program: &str, window: &str, what: &str, why: &str, within: u64) -> String {
    let at = match window.trim() {
        "" => String::new(),
        w => format!(" in the window \"{w}\""),
    };
    format!(
        "An AI agent in a Horadric session asks to do this in a program that runs as \
         administrator:\n\n{what}{at}\n\n{program}\n\nIt is asked because {why}.\n\nThis window \
         answers to the mouse only. Unanswered in {within} s, it is refused."
    )
}

/// What the helper holds of the bypasses the app told it of.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BypassHeld {
    /// The one the human let in: the end the app claimed, which tells
    /// this bypass from a later one, and the end the helper keeps to.
    pub yes: Option<(u64, u64)>,
    /// The end the app claimed of the one the human refused.
    pub no: Option<u64>,
}

/// What becomes of a question the helper would ask, with bypass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Bypassing {
    /// Ask it, as without bypass.
    Ask,
    /// Ask first whether this bypass holds here, until `until`.
    AskBypass { claim: u64, until: u64 },
    /// The human let this bypass in, and it has not ended.
    Allow,
}

/// What a bypass the app `claim`s to end then (seconds since 1970) makes
/// of a question at `now`. Without a claim that still stands, or once
/// the human refused this bypass, the question is asked. A yes holds only
/// for the same claimed end and until the end kept with it, which is
/// never past [`listed::BYPASS_MOST`] from when it was asked.
pub fn bypassing(claim: Option<u64>, held: BypassHeld, now: u64) -> Bypassing {
    let Some(claim) = claim.filter(|c| *c > now) else {
        return Bypassing::Ask;
    };
    match held.yes {
        Some((said, until)) if said == claim => {
            return match now < until {
                true => Bypassing::Allow,
                false => Bypassing::Ask,
            };
        }
        _ => {}
    }
    if held.no == Some(claim) {
        return Bypassing::Ask;
    }
    Bypassing::AskBypass {
        claim,
        until: claim.min(now + listed::BYPASS_MOST.as_secs()),
    }
}

/// The local clock `ahead` seconds after `now`, as hours and minutes,
/// `now` being seconds into the local day. Cut to the minute, as the
/// plate cuts the bypass end it shows, so the two read the same.
pub fn clock_after(now: u64, ahead: u64) -> String {
    let minutes = (now + ahead) / 60;
    format!("{:02}:{:02}", minutes / 60 % 24, minutes % 60)
}

/// Seconds into the local day now.
fn local_seconds() -> u64 {
    let t = unsafe { GetLocalTime() };
    u64::from(t.wHour) * 3600 + u64::from(t.wMinute) * 60 + u64::from(t.wSecond)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The helper's question when the app says bypass is on: once per bypass,
/// whether it holds in this program too.
pub fn bypass_question(program: &str, what: &str, until: &str, within: u64) -> String {
    format!(
        "Horadric says you turned bypass on until {until}, for the AI agent's session that asks \
         to operate this program, which runs as administrator:\n\n{program}\n\nNow it wants \
         to {what}.\n\nYes lets the agent do everything in this program without asking here, \
         its Open and Save dialogs included, until {until} or until bypass ends, whichever comes \
         first. It can not be limited to this program: from its Open and Save dialogs, and from \
         whatever else the program offers, other programs can be started with the same \
         rights.\n\nAsk each time asks about every risky action here as before, for as long as \
         this bypass lasts. This window answers to the mouse only. Unanswered in {within} s, the \
         action is refused."
    )
}

/// What the agent is told when nobody answered one of the helper's
/// questions.
fn unanswered_text(name: &str, within: u64) -> String {
    format!(
        "nobody answered the helper's question about {name} within {within} s, so nothing was \
         done, and it is not asked again for {} s. The helper's questions are answered with the \
         mouse at this machine. If this is safe to do without asking, an administrator can list \
         it from an elevated shell (horadric takeover allow --elevated, horadric takeover \
         approve --elevated)",
        listed::REFUSED_FOR.as_secs()
    )
}

/// How long a question may wait for the human, of `within`, once `since`
/// is that far behind: the helper's questions in one call all share
/// [`ASKING_MOST`].
fn time_left(within: u64, since: Instant) -> Duration {
    Duration::from_secs(within).min(ASKING_MOST.saturating_sub(since.elapsed()))
}

/// The end of bypass the app claims for this session, if any. An app
/// that does not know the op, or a session gone, claims none.
fn bypass_claim<P: FnMut(Value) -> Result<Value, String>>(peer: &mut P) -> Option<u64> {
    peer(json!({ "op": "desktop.bypass" }))
        .ok()?
        .get("until")
        .and_then(Value::as_u64)
}

/// The file and the process of the window `hwnd`, by this process's own
/// lookup.
fn image_of(hwnd: isize) -> Option<(u32, String)> {
    unsafe {
        let h = HWND(hwnd as *mut _);
        let mut pid = 0u32;
        if hwnd == 0 || GetWindowThreadProcessId(h, Some(&mut pid)) == 0 || pid == 0 {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let named = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(process);
        named.ok()?;
        Some((pid, String::from_utf16_lossy(&buf[..len as usize])))
    }
}

fn title_of(hwnd: isize) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(HWND(hwnd as *mut _), &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

unsafe extern "system" fn collect(h: HWND, list: LPARAM) -> windows::core::BOOL {
    (*(list.0 as *mut Vec<isize>)).push(h.0 as isize);
    true.into()
}

/// Whether process `pid` shows a file dialog now: a visible window of its
/// own that holds the shell's view of a folder.
fn file_dialog_open(pid: u32) -> bool {
    let mut tops: Vec<isize> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut tops as *mut _ as isize));
    }
    tops.into_iter().any(|top| unsafe {
        let h = HWND(top as *mut _);
        let mut owner = 0u32;
        GetWindowThreadProcessId(h, Some(&mut owner));
        if owner != pid || !IsWindowVisible(h).as_bool() {
            return false;
        }
        let mut inside: Vec<isize> = vec![top];
        let _ = EnumChildWindows(
            Some(h),
            Some(collect),
            LPARAM(&mut inside as *mut _ as isize),
        );
        inside.into_iter().any(|child| {
            let mut buf = [0u16; 128];
            let n = GetClassNameW(HWND(child as *mut _), &mut buf);
            file_dialog_class(&String::from_utf16_lossy(&buf[..n.max(0) as usize]))
        })
    })
}

/// The longest the helper stays silent in one call for its questions,
/// under [`SILENCE`], past which the session takes it for hung.
const ASKING_MOST: Duration = Duration::from_secs(135);

/// The helper's own word on what it may do. Everything here is decided in
/// the elevated process, from what it looks up itself and from the
/// administrators' list: the session at the other end of the pipe is not
/// elevated, and any program of the user can stand in for it.
struct Gate {
    /// The one program this helper acts in, by its file's full path, as
    /// Windows showed the human when it asked to start it.
    program: String,
    /// The session's process, for the log.
    parent: u32,
    /// Operating the program was allowed, by the human in this helper's
    /// own question or by the administrators' list.
    operate: bool,
    refused: Refusals<()>,
    /// Why the administrators' list was last not used, logged once.
    list_refused: Option<String>,
    /// The call now decided was refused in a question, whose answer is
    /// its line in the log already.
    answered: bool,
    /// The human's answers to whether a bypass holds here.
    bypass: BypassHeld,
}

impl Gate {
    fn new(program: String, parent: u32) -> Gate {
        Gate {
            program,
            parent,
            operate: false,
            refused: Refusals::default(),
            list_refused: None,
            answered: false,
            bypass: BypassHeld::default(),
        }
    }

    /// A decision in the log, as the app writes its own.
    fn log(&self, source: Source, what: &str) {
        let (utc, session, program) = self.stamp();
        write_log(&listed::log_line(&utc, &session, source, &program, what));
    }

    /// What became of an action, or a refusal no question was asked for.
    fn note(&self, how: &str, what: &str) {
        let (utc, session, program) = self.stamp();
        write_log(&helper_line(&utc, &session, how, &program, what));
    }

    fn stamp(&self) -> (String, String, String) {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        (
            horadric_core::tasks::utc(secs),
            format!("helper for process {}", self.parent),
            format!("{} as administrator", self.program),
        )
    }

    /// The administrators' list as it stands now, read on every decision
    /// so an entry taken off counts at once.
    fn admin_list(&mut self) -> listed::List {
        match takeover_admin::read() {
            takeover_admin::Read::List(list) => {
                self.list_refused = None;
                list
            }
            takeover_admin::Read::Missing => listed::List::default(),
            takeover_admin::Read::Refused(why) => {
                if self.list_refused.as_ref() != Some(&why) {
                    self.log(Source::Unreadable, &why);
                    self.list_refused = Some(why);
                }
                listed::List::default()
            }
        }
    }

    /// Puts one of the two questions to the human, unless the same was
    /// refused a moment ago. An error is the refusal, as the agent reads
    /// it.
    fn ask(
        &mut self,
        question: Question,
        text: &str,
        what: &str,
        within: u64,
        since: Instant,
        near: isize,
    ) -> Result<(), String> {
        let name = listed_name(&self.program);
        if let Some(left) = self.refused.left(&(), question, Instant::now()) {
            return Err(listed::refused_text(question, name, left));
        }
        let risky = question == Question::Risky;
        let time = time_left(within, since);
        let (title, yes) = match question {
            Question::Approve => (
                "Horadric's helper (administrator): operate this program?",
                "Let it operate",
            ),
            Question::Risky => ("Horadric's helper (administrator): do this?", "Do it"),
        };
        let answer = match time.as_secs() {
            0..=4 => ask::Answer::Unanswered,
            _ => ask::ask(title, text, yes, "Refuse", time, near),
        };
        let (source, said) = match (answer, risky) {
            // Not the human's no, and nothing to hold against the next call.
            (ask::Answer::TooTall, _) => return Err(TOO_TALL.into()),
            (ask::Answer::Yes, false) => (Source::Asked(Some(Level::Operate)), None),
            (ask::Answer::Yes, true) => (Source::Risky(true), None),
            (ask::Answer::No, false) => (
                Source::Asked(None),
                Some(format!(
                    "the human refused operating {name} as administrator, in the helper's own \
                     question. They are not asked again for {} s: say in the chat what you need",
                    listed::REFUSED_FOR.as_secs()
                )),
            ),
            (ask::Answer::No, true) => (
                Source::Risky(false),
                Some(format!(
                    "the human refused this action in {name}, in the helper's own question. They \
                     are not asked about a risky action there again for {} s: say in the chat \
                     what you need",
                    listed::REFUSED_FOR.as_secs()
                )),
            ),
            (ask::Answer::Unanswered, risky) => (
                Source::NoAnswer {
                    risky,
                    secs: within,
                },
                Some(unanswered_text(name, within)),
            ),
        };
        self.log(source, what);
        self.answered = said.is_some();
        match said {
            None => {
                self.refused.clear(&(), question);
                Ok(())
            }
            Some(no) => {
                self.refused.note(&(), question, Instant::now());
                Err(no)
            }
        }
    }

    /// Whether a bypass the app claims spares the human `question`,
    /// asking them first, once per bypass, whether it holds here. False
    /// is: ask the question as without bypass. The claim is asked of the
    /// app only when a question is near, and once per call (`claim`).
    #[allow(clippy::too_many_arguments)]
    fn by_bypass<P: FnMut(Value) -> Result<Value, String>>(
        &mut self,
        peer: &mut P,
        claim: &mut Option<Option<u64>>,
        question: Question,
        what: &str,
        within: u64,
        since: Instant,
        near: isize,
    ) -> Result<bool, String> {
        let claimed = *claim.get_or_insert_with(|| bypass_claim(peer));
        let now = unix_now();
        let (claimed, until) = match bypassing(claimed, self.bypass, now) {
            Bypassing::Ask => return Ok(false),
            Bypassing::Allow => {
                self.log(Source::Bypass, what);
                return Ok(true);
            }
            Bypassing::AskBypass { claim, until } => (claim, until),
        };
        // Held after nobody answered: the question it holds says so.
        if self.refused.left(&(), question, Instant::now()).is_some() {
            return Ok(false);
        }
        let shown = clock_after(local_seconds(), until.saturating_sub(now));
        let text = bypass_question(&self.program, what, &shown, within);
        let answer = match time_left(within, since).as_secs() {
            0..=4 => ask::Answer::Unanswered,
            secs => ask::ask(
                "Horadric's helper (administrator): bypass here too?",
                &text,
                &format!("Allow until {shown}"),
                "Ask each time",
                Duration::from_secs(secs),
                near,
            ),
        };
        let asked = |yes| Source::BypassAsked {
            yes,
            until: shown.clone(),
        };
        match answer {
            ask::Answer::TooTall => Err(TOO_TALL.into()),
            ask::Answer::Yes => {
                self.bypass.yes = Some((claimed, until));
                self.log(asked(true), what);
                Ok(true)
            }
            // A no to all of it is not a no to this action, which is
            // asked about next as it would be without bypass.
            ask::Answer::No => {
                self.bypass.no = Some(claimed);
                self.log(asked(false), what);
                Ok(false)
            }
            // Nobody there to answer the next question either.
            ask::Answer::Unanswered => {
                let risky = question == Question::Risky;
                self.log(
                    Source::NoAnswer {
                        risky,
                        secs: within,
                    },
                    what,
                );
                self.refused.note(&(), question, Instant::now());
                self.answered = true;
                Err(unanswered_text(listed_name(&self.program), within))
            }
        }
    }

    /// Whether the helper lets the action of an `allow` body go ahead.
    /// The body was built by this process's own engine, so the window, the
    /// action's parts and its risk are what the helper read; only the
    /// agent's sentence of why is the session's, and it is not shown.
    fn decide<P: FnMut(Value) -> Result<Value, String>>(
        &mut self,
        body: &Value,
        peer: &mut P,
    ) -> Result<(), String> {
        let since = Instant::now();
        let mut claim = None;
        let hwnd = body
            .pointer("/window/hwnd")
            .and_then(Value::as_i64)
            .and_then(|h| isize::try_from(h).ok())
            .unwrap_or(0);
        let (pid, path) = image_of(hwnd).ok_or(
            "the helper could not tell which program that window belongs to, so nothing was done",
        )?;
        if !same_program(&path, &self.program) {
            return Err(not_bound_text(&self.program, &path));
        }
        match body.get("action").and_then(Value::as_str) {
            // Looking is the app's to allow: the human said yes to this
            // helper for this program, and nothing is changed by it.
            Some("see") => return Ok(()),
            Some("operate") => {}
            _ => return Err("the helper does not know what that call wants to do".into()),
        }
        let act = act_of(body.get("act"));
        let what = body
            .get("what")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .unwrap_or("act in it")
            .to_string();
        // The question shows every character typed, the log the short line.
        let shown = format!("{what}{}", listed::typed_block(&act.typed));
        let said = body.get("risky").and_then(Value::as_bool) == Some(true);
        let risk = own_risk(said, &act, file_dialog_open(pid));
        let list = self.admin_list();
        let within = list.answer_within();
        let name = listed_name(&path).to_string();
        if !self.operate {
            let level = listed::find(&list.programs, &path, &name, true).map(|e| e.level);
            if level == Some(Level::Operate) {
                self.log(Source::List(Level::Operate), &what);
            } else if self.by_bypass(
                peer,
                &mut claim,
                Question::Approve,
                &what,
                within,
                since,
                hwnd,
            )? {
                // Bypass covers the whole action, and lets nothing in
                // past its end: operating is asked about again then.
                return Ok(());
            } else {
                let text = first_question(&self.program, &shown, within);
                self.ask(Question::Approve, &text, &what, within, since, hwnd)?;
            }
            self.operate = true;
        }
        let Some(why) = risk else {
            return Ok(());
        };
        if let Some(pattern) = listed::pre_approval(&list.programs, &path, &name, true, &act) {
            self.log(Source::PreApproved(pattern), &what);
            return Ok(());
        }
        let bypassed = self.by_bypass(
            peer,
            &mut claim,
            Question::Risky,
            &what,
            within,
            since,
            hwnd,
        )?;
        if bypassed {
            return Ok(());
        }
        let window = match act.window.trim() {
            "" => title_of(hwnd),
            w => w.to_string(),
        };
        let text = risky_question(&self.program, &window, &shown, why, within);
        self.ask(Question::Risky, &text, &what, within, since, hwnd)
    }

    /// Passes a body of the engine's on to the session, an `allow` only
    /// once the helper itself lets it. The session's yes adds nothing to
    /// the helper's, and its no still refuses: the app may hold the
    /// program for another session, or the human may stop the action on
    /// its way.
    fn through<P: FnMut(Value) -> Result<Value, String>>(
        &mut self,
        body: Value,
        peer: &mut P,
    ) -> Result<Value, String> {
        let what = body
            .get("what")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match body.get("op").and_then(Value::as_str) {
            Some("desktop.allow") => {}
            Some("desktop.done") => {
                let outcome = body.get("outcome").and_then(Value::as_str).unwrap_or("");
                match body.get("ok").and_then(Value::as_bool) {
                    Some(true) => self.note("done", &what),
                    _ => self.note(&format!("failed: {outcome}"), &what),
                }
                return peer(body);
            }
            _ => return peer(body),
        }
        self.answered = false;
        if let Err(why) = self.decide(&body, peer) {
            if !self.answered {
                self.note(&format!("refused by the helper: {why}"), &what);
            }
            // For the history the human watches. What the app makes of it
            // does not matter.
            let _ = peer(json!({
                "op": "desktop.done",
                "window": body.get("window"),
                "what": body.get("what"),
                "ok": false,
                "outcome": "refused by Horadric's helper",
            }));
            return Err(why);
        }
        peer(body)
    }
}

/// Adds a line to the helper's own log. A line that could not be written
/// is dropped: the helper has no window to say so in that the session
/// could not imitate, and the human's yes must not wait on its log.
fn write_log(line: &str) {
    let _ = takeover_admin::log(line);
}

/// The longest a refusal's reason runs in the log: it is the agent's
/// text, written for a chat, and the log keeps one line per action.
const LOG_REASON_MOST: usize = 240;

/// One line of the helper's log for what is not a decision of the
/// list's or the human's: an action done or failed, or one the helper
/// refused before any question. Laid out as [`listed::log_line`] lays out
/// a decision, so the file reads as one.
fn helper_line(utc: &str, session: &str, how: &str, program: &str, what: &str) -> String {
    let flat = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut how = flat(how);
    if let Some((cut, _)) = how.char_indices().nth(LOG_REASON_MOST) {
        how.truncate(cut);
        how.push_str("...");
    }
    let mut line = format!("{utc}  {}  {how}  {}", flat(session), flat(program));
    if !what.trim().is_empty() {
        line.push_str(&format!("  ({})", flat(what)));
    }
    line.push('\n');
    line
}

/// The file name of a path.
fn listed_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// The helper's own question: the app's question window, drawn by this
/// elevated process in the look the human picked in the app.
///
/// It never takes the keyboard (it can not be activated), so no key meant
/// for another window answers it, the agent's or the human's: only a
/// mouse click on one of its buttons does, and not in its first moments,
/// when a click already on its way could land on it. It draws its buttons
/// itself, so UI Automation finds nothing in it to invoke. Windows keeps
/// the input of programs that are not elevated out of it, and the desktop
/// tools leave Horadric's own windows alone.
mod ask {
    use std::time::Duration;

    use horadric_ui::question::{self, Aside, Dialog, Tone};
    use horadric_ui::theme::Theme;
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum Answer {
        Yes,
        No,
        /// Nobody answered in time.
        Unanswered,
        /// It was not asked: even at the screen's width it would be taller
        /// than the screen, so some of it could not be read.
        TooTall,
    }

    /// The look the human picked in the app, read from its state file.
    /// Only the theme's name is taken from that file, which any program
    /// of the user can write, and every name is one of the app's fixed
    /// looks: a wrong or missing one changes the colours and nothing else.
    fn picked() -> Theme {
        let state = horadric_hooks::state_dir()
            .and_then(|dir| std::fs::read(dir.join("state.json")).ok())
            .unwrap_or_default();
        super::theme_of(&state)
    }

    /// Asks, aside from the window `near`, and waits `within` for a click.
    /// The refusing button is the one ringed. A question that can not be
    /// shown is not answered yes.
    pub fn ask(
        title: &str,
        text: &str,
        yes: &str,
        no: &str,
        within: Duration,
        near: isize,
    ) -> Answer {
        asked_in(picked(), title, text, yes, no, within, near)
    }

    /// As [`ask`], in the look `look`.
    pub fn asked_in(
        look: Theme,
        title: &str,
        text: &str,
        yes: &str,
        no: &str,
        within: Duration,
        near: isize,
    ) -> Answer {
        let mut r = RECT::default();
        let avoid = unsafe { GetWindowRect(HWND(near as *mut _), &mut r) }
            .ok()
            .filter(|_| near != 0)
            .map(|_| [r.left, r.top, r.right, r.bottom]);
        let d = Dialog {
            tone: Tone::Warning,
            title,
            text,
            buttons: &[yes, no],
            default: 1,
            check: None,
        };
        match question::ask_alone(look, &d, &Aside { avoid, within }) {
            question::Answer::Pressed(0) => Answer::Yes,
            // Closed some other way is a no, as is the refusing button.
            question::Answer::Pressed(_) | question::Answer::Dismissed => Answer::No,
            question::Answer::Unanswered => Answer::Unanswered,
            question::Answer::TooTall => Answer::TooTall,
        }
    }
}

/// The look named in the app's state file, the app's first for none or
/// one this build does not know.
fn theme_of(state: &[u8]) -> horadric_ui::theme::Theme {
    let key = serde_json::from_slice::<Value>(state)
        .ok()
        .and_then(|v| v.get("theme").and_then(Value::as_str).map(str::to_string));
    horadric_ui::theme::Theme::from_key(key.as_deref())
}

/// `horadric takeover-helper-preview [theme] [first|risky|bypass]`, in a
/// debug build only: shows one of the helper's questions as the helper
/// draws it, without elevation, for a look at it on screen. The answer is
/// printed and nothing else is done with it.
#[cfg(debug_assertions)]
pub fn preview(args: &[String]) -> Result<(), String> {
    let look = horadric_ui::theme::Theme::from_key(args.first().map(String::as_str));
    let program = r"C:\Program Files\Acme\Studio\Studio.exe";
    let what = "type \"C:\\Projects\\line4.smc2\" in Edit \"Filnavn:\"";
    let (title, text, yes, no) = match args.get(1).map(String::as_str) {
        Some("first") => (
            "Horadric's helper (administrator): operate this program?",
            first_question(program, what, 60),
            "Let it operate".to_string(),
            "Refuse",
        ),
        Some("risky") => (
            "Horadric's helper (administrator): do this?",
            risky_question(
                program,
                "Open",
                what,
                "it is in an Open or Save dialog, or on its button, and such a dialog can \
                 start other programs with this program's administrator rights",
                60,
            ),
            "Do it".to_string(),
            "Refuse",
        ),
        _ => (
            "Horadric's helper (administrator): bypass here too?",
            bypass_question(program, what, "12:13", 60),
            "Allow until 12:13".to_string(),
            "Ask each time",
        ),
    };
    let answer = ask::asked_in(look, title, &text, &yes, no, Duration::from_secs(60), 0);
    println!("{answer:?}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_helper_asks_in_the_look_the_app_was_given() {
        use horadric_ui::theme::Theme;
        assert_eq!(theme_of(br#"{"theme":"matrix"}"#), Theme::Matrix);
        assert_eq!(
            theme_of(br#"{"theme":"winamp","recent":[]}"#),
            Theme::Winamp
        );
        // Missing, unknown, the wrong type or no JSON at all: the first.
        for state in [
            &b"{}"[..],
            br#"{"theme":"vaporwave"}"#,
            br#"{"theme":7}"#,
            b"",
            b"{",
        ] {
            assert_eq!(theme_of(state), Theme::Skeuomorph);
        }
    }

    #[test]
    fn a_bypass_claim_is_asked_about_once_and_held_to_its_end() {
        let most = listed::BYPASS_MOST.as_secs();
        let none = BypassHeld::default();
        // No claim, or one already over: asked as ever.
        assert_eq!(bypassing(None, none, 100), Bypassing::Ask);
        assert_eq!(bypassing(Some(100), none, 100), Bypassing::Ask);
        // A claim is no answer: the human is asked whether it holds here.
        assert_eq!(
            bypassing(Some(900), none, 100),
            Bypassing::AskBypass {
                claim: 900,
                until: 900
            }
        );
        // A claim of a bypass longer than any the app allows is held to
        // the longest, from when it is asked.
        assert_eq!(
            bypassing(Some(u64::MAX), none, 100),
            Bypassing::AskBypass {
                claim: u64::MAX,
                until: 100 + most
            }
        );
        let yes = BypassHeld {
            yes: Some((900, 900)),
            no: None,
        };
        assert_eq!(bypassing(Some(900), yes, 500), Bypassing::Allow);
        // The claim gone (Esc, or its work done) or the time passed: asked.
        assert_eq!(bypassing(None, yes, 500), Bypassing::Ask);
        assert_eq!(bypassing(Some(900), yes, 900), Bypassing::Ask);
        // A new bypass is a new question.
        assert_eq!(
            bypassing(Some(1_800), yes, 500),
            Bypassing::AskBypass {
                claim: 1_800,
                until: 1_800
            }
        );
        // A capped yes ends at its cap, though the claim goes on.
        let capped = BypassHeld {
            yes: Some((u64::MAX, 100 + most)),
            no: None,
        };
        assert_eq!(
            bypassing(Some(u64::MAX), capped, 99 + most),
            Bypassing::Allow
        );
        assert_eq!(
            bypassing(Some(u64::MAX), capped, 100 + most),
            Bypassing::Ask
        );
        // A no holds for that bypass, which then asks each action.
        let no = BypassHeld {
            yes: None,
            no: Some(900),
        };
        assert_eq!(bypassing(Some(900), no, 500), Bypassing::Ask);
        assert_eq!(
            bypassing(Some(1_800), no, 500),
            Bypassing::AskBypass {
                claim: 1_800,
                until: 1_800
            }
        );
    }

    #[test]
    fn the_bypass_end_reads_as_the_plate_shows_it() {
        // 11:43:50, and thirty minutes on: the plate says 12:13.
        let now = 11 * 3600 + 43 * 60 + 50;
        assert_eq!(clock_after(now, 30 * 60), "12:13");
        assert_eq!(clock_after(23 * 3600 + 50 * 60, 30 * 60), "00:20");
        assert_eq!(clock_after(0, 0), "00:00");
    }

    #[test]
    fn the_bypass_question_says_until_when_and_what_it_lets_in() {
        let q = bypass_question(r"C:\S\Studio.exe", "click Open", "12:13", 60);
        assert!(q.contains("until 12:13"));
        assert!(q.contains(r"C:\S\Studio.exe"));
        assert!(q.contains("click Open"));
        assert!(q.contains("Open and Save dialogs included"));
        assert!(q.contains("other programs can be started"));
        assert!(q.contains("mouse only"));
        assert!(q.contains("60 s"));
        // Its lines are joined by a single space where the source breaks.
        assert!(!q.contains("  "));
        assert!(!unanswered_text("Studio.exe", 60).contains("  "));
    }

    #[test]
    fn a_helper_line_is_one_line_and_a_long_reason_is_cut() {
        assert_eq!(
            helper_line(
                "2026-11-26 10:00:00",
                "helper for process 7",
                "failed: the window\nwould not come forward",
                r"C:\Acme\Studio.exe as administrator",
                "press Enter",
            ),
            "2026-11-26 10:00:00  helper for process 7  failed: the window would not come \
             forward  C:\\Acme\\Studio.exe as administrator  (press Enter)\n"
        );
        let long = helper_line("t", "s", &"\u{e6}".repeat(500), "p", " ");
        assert_eq!(long.lines().count(), 1);
        assert!(long.contains(&format!("{}...  p\n", "\u{e6}".repeat(LOG_REASON_MOST))));
    }

    #[test]
    fn helper_lines_are_read_by_their_key() {
        assert_eq!(
            from_helper(r#"{"app":{"exe":"a.exe"}}"#),
            Ok(FromHelper::App(json!({"exe": "a.exe"})))
        );
        assert_eq!(
            from_helper(r#"{"done":[{"type":"text","text":"hi"}]}"#),
            Ok(FromHelper::Done(vec![
                json!({"type": "text", "text": "hi"})
            ]))
        );
        assert_eq!(
            from_helper(r#"{"fail":"no"}"#),
            Ok(FromHelper::Fail("no".into()))
        );
        assert!(from_helper(r#"{"what":1}"#).is_err());
        assert!(from_helper("not json").is_err());
    }

    #[test]
    fn answers_survive_the_pipe_both_ways() {
        let yes = Ok(json!({"allowed": true}));
        assert_eq!(answer_of(&answer_line(&yes)), yes);
        let no: Result<Value, String> = Err("the human said no".into());
        assert_eq!(answer_of(&answer_line(&no)), no);
        assert!(answer_of("{}").is_err());
    }

    #[test]
    fn an_administrator_window_is_marked_by_a_field_not_by_its_name() {
        let body = json!({"window": {"exe": "AcmeStudio.exe", "pid": 4}, "action": "see"});
        let marked = as_administrator(body);
        assert_eq!(marked["window"]["exe"], "AcmeStudio.exe");
        assert_eq!(marked["window"]["pid"], 4);
        assert_eq!(marked["administrator"], true);
        assert_eq!(as_administrator(json!({}))["administrator"], true);
    }

    const STUDIO: &str = r"C:\Program Files\Acme\Acme Studio\AcmeStudio.exe";

    #[test]
    fn a_helper_is_bound_to_one_file() {
        assert!(same_program(STUDIO, &STUDIO.to_uppercase()));
        assert!(same_program(STUDIO, &STUDIO.replace('\\', "/")));
        assert!(!same_program(
            STUDIO,
            r"C:\Users\x\Downloads\AcmeStudio.exe"
        ));
        assert!(!same_program("", ""));
        assert_eq!(unbindable(STUDIO), None);
        // No path, no helper: there would be nothing to bind it to.
        assert!(unbindable("").is_some());
        assert!(unbindable("AcmeStudio.exe").is_some());
        assert!(unbindable("C:\\a\" b.exe").is_some());
        // Never Horadric itself, whose windows hold the questions.
        let own = unbindable(r"C:\Users\x\AppData\Local\Programs\Horadric\horadric.exe");
        assert!(own.unwrap().contains("Horadric's own"));
        let told = not_bound_text(STUDIO, r"C:\Windows\regedit.exe");
        assert!(
            told.contains("alone") && told.contains("regedit.exe"),
            "{told}"
        );
    }

    fn act(kind: listed::Kind, control: &str, name: &str, key: &str) -> Act {
        Act {
            kind: Some(kind),
            window: "Proj - Acme Studio".into(),
            control: control.into(),
            name: name.into(),
            key: key.into(),
            texts: Vec::new(),
            typed: String::new(),
        }
    }

    #[test]
    fn the_helper_judges_risk_itself_and_a_file_dialog_is_always_risky() {
        use listed::Kind::{Click, Press, Type};
        // Nothing risky about a plain click, whatever the session says of
        // it: its word can only add.
        assert_eq!(
            own_risk(false, &act(Click, "Button", "Watch", ""), false),
            None
        );
        assert!(own_risk(true, &act(Click, "Button", "Watch", ""), false).is_some());
        // The words are found by the helper, not sent to it.
        assert!(own_risk(false, &act(Click, "MenuItem", "Transfer...", ""), false).is_some());
        assert!(own_risk(false, &act(Press, "", "", "Delete"), false).is_some());
        let mut asks = act(Click, "Button", "Yes", "");
        asks.texts = vec!["Transfer to the Controller?".into()];
        assert!(own_risk(false, &asks, false).is_some());
        // Anything while a file dialog is open.
        let why = own_risk(false, &act(Click, "ListItem", "notes.txt", ""), true).unwrap();
        assert!(why.contains("Open or Save dialog"), "{why}");
        assert!(own_risk(false, &act(Press, "", "", "Enter"), true).is_some());
        // And by name, should a dialog be built some other way: the name
        // box, typed into or holding the keyboard, and the two buttons.
        for a in [
            act(Type, "Edit", "File name:", ""),
            act(Type, "ComboBox", "Filnavn:", ""),
            act(Press, "", "File name:", "Enter"),
            act(Click, "Button", "&Open", ""),
            act(Click, "Button", "_Save", ""),
            act(Click, "Button", "Gem", ""),
        ] {
            let why = own_risk(false, &a, false);
            assert!(why.is_some_and(|w| w.contains("Open or Save")), "{a:?}");
        }
        assert_eq!(
            own_risk(false, &act(Click, "MenuItem", "Open", ""), false),
            None
        );
        assert!(file_dialog_class("DUIViewWndClassName") && file_dialog_class("SHELLDLL_DefView"));
        assert!(!file_dialog_class("Button"));
    }

    #[test]
    fn the_helper_judges_and_shows_the_whole_text_typed() {
        use listed::Kind::Type;
        let mut typing = act(Type, "Edit", "Notes", "");
        typing.typed = "line one".into();
        assert_eq!(own_risk(false, &typing, false), None);
        // Past the engine's short line: what is typed late counts too.
        typing.typed = format!("{} then C:\\Tools\\run.exe", "x".repeat(80));
        let why = own_risk(false, &typing, false).unwrap();
        assert!(why.contains("administrator"), "{why}");
        typing.typed = format!("{} delete all", "x".repeat(80));
        assert!(own_risk(false, &typing, false).is_some());
        // The body the engine sends carries it, and the question shows it.
        let body = json!({"kind": "type", "typed": "a  b\nc"});
        let read = act_of(Some(&body));
        assert_eq!(read.typed, "a  b\nc");
        let shown = format!("type{}", listed::typed_block(&read.typed));
        let text = risky_question("C:\\a.exe", "W", &shown, "why", 60);
        assert!(text.contains("a\u{B7}\u{B7}b\u{21B5}\nc"), "{text}");
    }

    #[test]
    fn the_helper_s_first_question_says_what_can_not_be_promised() {
        let q = first_question(STUDIO, "click Button \"Online\"", 60);
        assert!(
            q.contains(STUDIO) && q.contains("click Button \"Online\""),
            "{q}"
        );
        assert!(q.contains("can not be limited to this program"), "{q}");
        assert!(q.contains("mouse only") && q.contains("60 s"), "{q}");
        let r = risky_question(
            STUDIO,
            "Synchronization",
            "click Button \"Transfer\"",
            "x",
            30,
        );
        assert!(r.contains("in the window \"Synchronization\"") && r.contains("because x"));
        assert!(!risky_question(STUDIO, " ", "press Delete", "x", 30).contains("in the window"));
    }

    #[test]
    fn the_parts_of_an_action_are_read_from_the_engine_s_body() {
        let body = json!({
            "kind": "type", "window": "Open", "control": "Edit", "name": "File name:",
            "key": "", "texts": ["a", 3],
        });
        let a = act_of(Some(&body));
        assert_eq!(a.kind, Some(listed::Kind::Type));
        assert_eq!((a.window.as_str(), a.name.as_str()), ("Open", "File name:"));
        assert_eq!(a.texts, vec!["a".to_string()]);
        assert_eq!(act_of(None), Act::default());
    }

    #[test]
    fn a_build_from_a_source_tree_is_named_before_windows_asks() {
        use std::path::Path;
        assert!(dev_build(
            Path::new(r"C:\src\horadric\target\debug\horadric.exe"),
            false
        ));
        assert!(dev_build(
            Path::new(r"C:\src\horadric\Target\release\horadric.exe"),
            false
        ));
        let installed = r"C:\Users\x\AppData\Local\Programs\Horadric\horadric.exe";
        assert!(!dev_build(Path::new(installed), false));
        assert!(dev_build(Path::new(installed), true));
    }

    #[test]
    fn a_no_holds_from_the_no_and_a_loss_from_the_asking() {
        let s = Duration::from_secs;
        // Just declined, though asked long ago: the full hold.
        assert_eq!(
            hold_left(Setback::Declined, s(0), s(600)),
            Some(ASK_AGAIN_AFTER)
        );
        assert_eq!(hold_left(Setback::Declined, s(100), s(130)), Some(s(20)));
        assert_eq!(hold_left(Setback::Declined, s(120), s(130)), None);
        // A helper that ran ten minutes is asked for again at once.
        assert_eq!(hold_left(Setback::Lost, s(0), s(600)), None);
        // One that died as it started is not asked for in a loop.
        assert_eq!(hold_left(Setback::Lost, s(0), s(5)), Some(s(115)));
        // A start that failed after a slow yes holds from the failure.
        assert_eq!(
            hold_left(Setback::Failed, s(0), s(300)),
            Some(ASK_AGAIN_AFTER)
        );
        assert_eq!(hold_left(Setback::Failed, s(60), s(300)), Some(s(60)));
        assert_eq!(hold_left(Setback::Failed, s(121), s(300)), None);
    }

    /// The pipes' only entries are this user and the administrators, so
    /// another user's program, or an anonymous one, can not open them.
    #[test]
    fn a_pipe_lets_in_only_this_user_and_administrators() {
        use windows::Win32::Security::{
            EqualSid, GetAce, GetKernelObjectSecurity, GetSecurityDescriptorDacl,
            DACL_SECURITY_INFORMATION,
        };
        let name = format!(
            r"\\.\pipe\horadric-desktop-test-dacl-{}",
            std::process::id()
        );
        let pipe = serve(&name, true).unwrap();
        let handle = HANDLE(pipe.as_raw_handle());
        unsafe {
            let mut len = 0;
            let _ = GetKernelObjectSecurity(handle, DACL_SECURITY_INFORMATION.0, None, 0, &mut len);
            let mut buf = vec![0u64; (len as usize).div_ceil(8)];
            let sd = PSECURITY_DESCRIPTOR(buf.as_mut_ptr().cast());
            GetKernelObjectSecurity(handle, DACL_SECURITY_INFORMATION.0, Some(sd), len, &mut len)
                .unwrap();
            let (mut present, mut defaulted) = (Default::default(), Default::default());
            let mut dacl: *mut ACL = std::ptr::null_mut();
            GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted).unwrap();
            assert!(present.as_bool() && !dacl.is_null());
            assert_eq!((*dacl).AceCount, 2);
            let user = OwnersOnly::user_sid().unwrap();
            let admins = OwnersOnly::admins_sid().unwrap();
            let mut seen = Vec::new();
            for i in 0..2 {
                let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
                GetAce(dacl, i, &mut ace).unwrap();
                let ace = &*(ace as *const ACCESS_ALLOWED_ACE);
                assert_eq!(ace.Mask, PIPE_ALL_ACCESS);
                let sid = PSID(&ace.SidStart as *const u32 as *mut _);
                seen.push((
                    EqualSid(sid, PSID(user.as_ptr() as *mut _)).is_ok(),
                    EqualSid(sid, PSID(admins.as_ptr() as *mut _)).is_ok(),
                ));
            }
            assert_eq!(seen, vec![(true, false), (false, true)]);
        }
    }

    #[test]
    fn pipe_names_go_one_each_way() {
        assert_eq!(
            pipe_names(r"\\.\pipe\horadric-desktop-1-2"),
            (
                r"\\.\pipe\horadric-desktop-1-2-down".to_string(),
                r"\\.\pipe\horadric-desktop-1-2-up".to_string()
            )
        );
    }

    /// A call while the reading thread waits on the pipe up, then a drop,
    /// over real pipes made as a start makes them, with a stand in for
    /// the helper on a thread. One duplex pipe here hung in the first
    /// write, behind the waiting read.
    #[test]
    fn a_call_and_a_drop_do_not_wait_behind_the_reading_thread() {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let base = format!(
                r"\\.\pipe\horadric-desktop-test-{}-{nanos}",
                std::process::id()
            );
            let (down_name, up_name) = pipe_names(&base);
            let down = serve(&down_name, false).unwrap();
            let up = serve(&up_name, true).unwrap();
            let stand_in = thread::spawn(move || {
                let mut from = BufReader::new(open_client(&down_name, false)?);
                let mut to = open_client(&up_name, true)?;
                let mut line = String::new();
                from.read_line(&mut line)?;
                let asked: Value = serde_json::from_str(line.trim_end()).unwrap();
                writeln!(to, "{}", json!({ "app": { "op": "desktop.allow" } }))?;
                line.clear();
                from.read_line(&mut line)?;
                let allowed = answer_of(line.trim_end());
                writeln!(
                    to,
                    "{}",
                    json!({ "done": [asked["name"], allowed.is_ok()] })
                )?;
                // The session dropping the helper ends this read.
                line.clear();
                let end = from.read_line(&mut line).map_or(true, |n| n == 0);
                Ok::<bool, std::io::Error>(end)
            });
            assert!(connect(down.as_raw_handle() as usize));
            assert!(connect(up.as_raw_handle() as usize));
            let mut helper = Helper::over(up, down, HANDLE::default(), STUDIO);
            // Give the reading thread time to be waiting in its read.
            thread::sleep(Duration::from_millis(200));
            let mut told = Vec::new();
            let mut app = |body: Value| {
                told.push(body);
                Ok(json!({ "allowed": true }))
            };
            let answer = helper.call("takeover_click", &json!({}), &mut app, Duration::ZERO);
            drop(helper);
            let ended = stand_in.join().unwrap().unwrap();
            let _ = tx.send((answer, told, ended));
        });
        let (answer, told, ended) = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the call or the drop hung");
        assert_eq!(answer, Ok(Ok(vec![json!("takeover_click"), json!(true)])));
        assert_eq!(told, vec![json!({ "op": "desktop.allow" })]);
        assert!(ended, "the helper's read ends when the session drops it");
    }

    #[test]
    fn a_hold_says_why_and_for_how_long() {
        let no = held_text(Setback::Declined, "", Duration::from_secs(90));
        assert!(no.contains("said no") && no.contains("90 s"), "{no}");
        assert!(no.contains("Ask the human"), "{no}");
        let lost = held_text(Setback::Lost, "the helper ended", Duration::from_millis(10));
        assert!(lost.contains("(the helper ended)"), "{lost}");
        assert!(lost.contains("in 1 s"), "{lost}");
    }

    #[test]
    fn an_open_question_says_nothing_was_done_and_to_call_again() {
        let t = still_asking_text("AcmeStudio.exe", Duration::from_millis(90_500));
        assert!(
            t.contains("AcmeStudio.exe") && t.contains("90 s ago"),
            "{t}"
        );
        assert!(
            t.contains("Nothing was done") && t.contains("Call again"),
            "{t}"
        );
    }

    #[test]
    fn the_app_is_told_before_windows_asks() {
        let b = elevating_body(json!({"exe": "AcmeStudio.exe", "pid": 4}));
        assert_eq!(b["op"], "desktop.elevating");
        assert_eq!(b["window"]["exe"], "AcmeStudio.exe");
    }

    #[test]
    fn a_request_carries_name_args_and_settle() {
        let line = request_line(
            "takeover_click",
            &json!({"ref": "e3"}),
            Duration::from_millis(250),
        );
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["name"], "takeover_click");
        assert_eq!(v["args"]["ref"], "e3");
        assert_eq!(v["settle_ms"], 250);
    }
}
