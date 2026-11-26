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

use crate::takeover::{file_name_field, list_windows, looks_risky, pick_window, Takeover};
use crate::takeover_programs::desktop_admin;

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
        let params = HSTRING::from(format!("takeover-helper {name} {me} \"{}\"", program.trim()));
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
        _ => return Err("usage: horadric takeover-helper <pipe> <pid> <program's full path>".into()),
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
            let out = json!({ "fail": not_bound_text(&gate.program, &w.path) });
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
    let mut texts = vec![act.name.as_str(), act.window.as_str(), act.key.as_str()];
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
}

impl Gate {
    fn new(program: String, parent: u32) -> Gate {
        Gate {
            program,
            parent,
            operate: false,
            refused: Refusals::default(),
            list_refused: None,
        }
    }

    fn log(&self, source: Source, what: &str) {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        desktop_admin::log(&listed::log_line(
            &horadric_core::tasks::utc(secs),
            &format!("helper for process {}", self.parent),
            source,
            &format!("{} as administrator", self.program),
            what,
        ));
    }

    /// The administrators' list as it stands now, read on every decision
    /// so an entry taken off counts at once.
    fn admin_list(&mut self) -> listed::List {
        match desktop_admin::read() {
            desktop_admin::Read::List(list) => {
                self.list_refused = None;
                list
            }
            desktop_admin::Read::Missing => listed::List::default(),
            desktop_admin::Read::Refused(why) => {
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
        let time = Duration::from_secs(within).min(ASKING_MOST.saturating_sub(since.elapsed()));
        let (title, yes) = match question {
            Question::Approve => ("Horadric's helper (administrator): operate this program?", "Let it operate"),
            Question::Risky => ("Horadric's helper (administrator): do this?", "Do it"),
        };
        let answer = match time.as_secs() {
            0..=4 => ask::Answer::Unanswered,
            _ => ask::ask(title, text, yes, "Refuse", time, near),
        };
        let (source, said) = match (answer, risky) {
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
                Some(format!(
                    "nobody answered the helper's question about {name} within {within} s, so \
                     nothing was done, and it is not asked again for {} s. The helper's questions \
                     are answered with the mouse at this machine. If this is safe to do without \
                     asking, an administrator can list it from an elevated shell (horadric \
                     takeover allow --elevated, horadric takeover approve --elevated)",
                    listed::REFUSED_FOR.as_secs()
                )),
            ),
        };
        self.log(source, what);
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

    /// Whether the helper lets the action of an `allow` body go ahead.
    /// The body was built by this process's own engine, so the window, the
    /// action's parts and its risk are what the helper read; only the
    /// agent's sentence of why is the session's, and it is not shown.
    fn decide(&mut self, body: &Value) -> Result<(), String> {
        let since = Instant::now();
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
        let said = body.get("risky").and_then(Value::as_bool) == Some(true);
        let risk = own_risk(said, &act, file_dialog_open(pid));
        let list = self.admin_list();
        let within = list.answer_within();
        let name = listed_name(&path).to_string();
        if !self.operate {
            let level = listed::find(&list.programs, &path, &name, true).map(|e| e.level);
            if level == Some(Level::Operate) {
                self.log(Source::List(Level::Operate), &what);
            } else {
                let text = first_question(&self.program, &what, within);
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
        let window = match act.window.trim() {
            "" => title_of(hwnd),
            w => w.to_string(),
        };
        let text = risky_question(&self.program, &window, &what, why, within);
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
        if body.get("op").and_then(Value::as_str) != Some("desktop.allow") {
            return peer(body);
        }
        if let Err(why) = self.decide(&body) {
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

/// The file name of a path.
fn listed_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// The helper's own question: a plain window of this elevated process.
///
/// It never takes the keyboard (it can not be activated), so no key meant
/// for another window answers it, the agent's or the human's: only a
/// mouse click on one of its two buttons does, and not in its first
/// moments, when a click already on its way could land on it. It draws
/// its buttons itself, so UI Automation finds nothing in it to invoke.
/// Windows keeps the input of programs that are not elevated out of it,
/// and the desktop tools leave Horadric's own windows alone.
mod ask {
    use std::cell::RefCell;
    use std::time::{Duration, Instant};

    use windows::core::{w, HSTRING};
    use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CreateFontW, DeleteObject, DrawTextW, EndPaint, FillRect, FrameRect, GetDC,
        GetMonitorInfoW, GetSysColor, GetSysColorBrush, InvalidateRect, MonitorFromWindow,
        ReleaseDC, SelectObject, SetBkMode, SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
        COLOR_BTNFACE, COLOR_WINDOW, COLOR_WINDOWTEXT, DEFAULT_CHARSET, DT_CALCRECT, DT_CENTER,
        DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, HFONT, HGDIOBJ, MONITORINFO,
        MONITOR_DEFAULTTOPRIMARY, OUT_DEFAULT_PRECIS, PAINTSTRUCT, TRANSPARENT,
    };
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    use windows::Win32::UI::WindowsAndMessaging::{
        AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
        GetMessageW, KillTimer, LoadCursorW, RegisterClassW, SetTimer, SetWindowPos, ShowWindow,
        TranslateMessage, HWND_TOPMOST, IDC_ARROW, MSG, SWP_NOACTIVATE, SW_SHOWNOACTIVATE,
        WM_CLOSE, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_PAINT, WM_TIMER, WNDCLASSW,
        WS_CAPTION, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_SYSMENU,
    };

    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut core::ffi::c_void;
    }

    /// What Windows is told when the mouse comes down on the window: take
    /// the click, do not come forward.
    const MA_NOACTIVATE: isize = 3;
    /// A click this soon after the window shows was aimed at what was
    /// there before it.
    const ARMED_AFTER: Duration = Duration::from_millis(900);
    const TICK: usize = 1;

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum Answer {
        Yes,
        No,
        /// Nobody answered in time.
        Unanswered,
    }

    struct State {
        text: Vec<u16>,
        yes: Vec<u16>,
        no: Vec<u16>,
        text_at: RECT,
        yes_at: RECT,
        no_at: RECT,
        note_at: RECT,
        font: HFONT,
        /// The button the mouse went down on: a click is down and up on
        /// the same one.
        down: Option<Answer>,
        answer: Option<Answer>,
        armed: Instant,
        ends: Instant,
    }

    thread_local! {
        /// The question on screen. The helper serves one call at a time on
        /// one thread, so there is never more than one.
        static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    }

    fn inside(r: &RECT, x: i32, y: i32) -> bool {
        x >= r.left && x < r.right && y >= r.top && y < r.bottom
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    unsafe fn paint(hwnd: HWND, s: &State) {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut ps);
        let old = SelectObject(dc, HGDIOBJ(s.font.0));
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, COLORREF(GetSysColor(COLOR_WINDOWTEXT)));
        let mut at = s.text_at;
        DrawTextW(dc, &mut s.text.clone(), &mut at, DT_WORDBREAK | DT_NOPREFIX);
        let frame = GetSysColorBrush(COLOR_WINDOWTEXT);
        for (label, place, heavy) in [(&s.yes, s.yes_at, false), (&s.no, s.no_at, true)] {
            FillRect(dc, &place, GetSysColorBrush(COLOR_BTNFACE));
            FrameRect(dc, &place, frame);
            if heavy {
                // The refusing button is the marked one.
                let inner = RECT {
                    left: place.left + 1,
                    top: place.top + 1,
                    right: place.right - 1,
                    bottom: place.bottom - 1,
                };
                FrameRect(dc, &inner, frame);
            }
            let mut at = place;
            DrawTextW(
                dc,
                &mut label.clone(),
                &mut at,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
        }
        let left = s.ends.saturating_duration_since(Instant::now()).as_secs();
        let mut note = wide(&format!("Refused in {left} s"));
        let mut at = s.note_at;
        DrawTextW(dc, &mut note, &mut at, DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
        SelectObject(dc, old);
        let _ = EndPaint(hwnd, &ps);
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        let at = |lp: LPARAM| {
            (
                i32::from((lp.0 & 0xFFFF) as u16 as i16),
                i32::from(((lp.0 >> 16) & 0xFFFF) as u16 as i16),
            )
        };
        let hit = |s: &State, lp: LPARAM| {
            let (x, y) = at(lp);
            if inside(&s.yes_at, x, y) {
                Some(Answer::Yes)
            } else if inside(&s.no_at, x, y) {
                Some(Answer::No)
            } else {
                None
            }
        };
        match msg {
            WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE),
            WM_PAINT => {
                let painted = STATE.with(|s| {
                    let s = s.borrow();
                    s.as_ref().map(|s| paint(hwnd, s)).is_some()
                });
                if painted {
                    return LRESULT(0);
                }
            }
            WM_LBUTTONDOWN => {
                STATE.with(|s| {
                    if let Some(s) = s.borrow_mut().as_mut() {
                        s.down = hit(s, lp).filter(|_| Instant::now() >= s.armed);
                    }
                });
                return LRESULT(0);
            }
            WM_LBUTTONUP => {
                STATE.with(|s| {
                    if let Some(s) = s.borrow_mut().as_mut() {
                        let up = hit(s, lp);
                        if up.is_some() && up == s.down {
                            s.answer = up;
                        }
                        s.down = None;
                    }
                });
                return LRESULT(0);
            }
            WM_TIMER => {
                STATE.with(|s| {
                    if let Some(s) = s.borrow_mut().as_mut() {
                        if Instant::now() >= s.ends {
                            s.answer = Some(Answer::Unanswered);
                        }
                        let _ = InvalidateRect(Some(hwnd), Some(&s.note_at), true);
                    }
                });
                return LRESULT(0);
            }
            WM_CLOSE => {
                STATE.with(|s| {
                    if let Some(s) = s.borrow_mut().as_mut() {
                        s.answer = Some(Answer::No);
                    }
                });
                return LRESULT(0);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }

    /// Asks, on the screen of the window `near`, and waits `within` for a
    /// click. A question that can not be shown is not answered yes.
    pub fn ask(title: &str, text: &str, yes: &str, no: &str, within: Duration, near: isize) -> Answer {
        unsafe {
            let module = HINSTANCE(GetModuleHandleW(std::ptr::null()));
            let class = w!("HoradricHelperQuestion");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: module,
                lpszClassName: class,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hbrBackground: GetSysColorBrush(COLOR_WINDOW),
                ..Default::default()
            };
            // A second question finds the class there already.
            RegisterClassW(&wc);
            let ex = WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW;
            let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
            let Ok(hwnd) = CreateWindowExW(
                ex,
                class,
                &HSTRING::from(title),
                style,
                0,
                0,
                100,
                100,
                None,
                None,
                Some(module),
                None,
            ) else {
                return Answer::Unanswered;
            };
            let dpi = GetDpiForWindow(hwnd).max(96) as i32;
            let px = |dip: i32| dip * dpi / 96;
            let font = CreateFontW(
                -px(15),
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                w!("Segoe UI"),
            );
            let (width, pad, gap) = (px(560), px(18), px(12));
            let (button_w, button_h) = (px(150), px(34));
            let text = wide(text);
            let mut measured = RECT {
                right: width - 2 * pad,
                ..Default::default()
            };
            let dc = GetDC(Some(hwnd));
            let old = SelectObject(dc, HGDIOBJ(font.0));
            DrawTextW(
                dc,
                &mut text.clone(),
                &mut measured,
                DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
            );
            SelectObject(dc, old);
            ReleaseDC(Some(hwnd), dc);
            let text_at = RECT {
                left: pad,
                top: pad,
                right: width - pad,
                bottom: pad + measured.bottom,
            };
            let row = text_at.bottom + pad;
            let no_at = RECT {
                left: width - pad - button_w,
                top: row,
                right: width - pad,
                bottom: row + button_h,
            };
            let yes_at = RECT {
                left: no_at.left - gap - button_w,
                top: row,
                right: no_at.left - gap,
                bottom: row + button_h,
            };
            let note_at = RECT {
                left: pad,
                top: row,
                right: yes_at.left - gap,
                bottom: row + button_h,
            };
            let mut outer = RECT {
                left: 0,
                top: 0,
                right: width,
                bottom: row + button_h + pad,
            };
            let _ = AdjustWindowRectEx(&mut outer, style, false, ex);
            let (w, h) = (outer.right - outer.left, outer.bottom - outer.top);
            let mut screen = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let monitor = MonitorFromWindow(HWND(near as *mut _), MONITOR_DEFAULTTOPRIMARY);
            let _ = GetMonitorInfoW(monitor, &mut screen);
            let work = screen.rcWork;
            // Near the top, clear of the middle, where the program's own
            // dialog most likely is.
            let x = work.left + ((work.right - work.left) - w).max(0) / 2;
            let y = work.top + px(40);
            let now = Instant::now();
            STATE.set(Some(State {
                text,
                yes: wide(yes),
                no: wide(no),
                text_at,
                yes_at,
                no_at,
                note_at,
                font,
                down: None,
                answer: None,
                armed: now + ARMED_AFTER,
                ends: now + within,
            }));
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            SetTimer(Some(hwnd), TICK, 500, None);
            let mut msg = MSG::default();
            let answer = loop {
                let answered = STATE.with(|s| s.borrow().as_ref().and_then(|s| s.answer));
                if let Some(answer) = answered {
                    break answer;
                }
                if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    break Answer::Unanswered;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            };
            let _ = KillTimer(Some(hwnd), TICK);
            STATE.set(None);
            let _ = DestroyWindow(hwnd);
            let _ = DeleteObject(HGDIOBJ(font.0));
            answer
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(!same_program(STUDIO, r"C:\Users\x\Downloads\AcmeStudio.exe"));
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
        assert!(told.contains("alone") && told.contains("regedit.exe"), "{told}");
    }

    fn act(kind: listed::Kind, control: &str, name: &str, key: &str) -> Act {
        Act {
            kind: Some(kind),
            window: "Proj - Acme Studio".into(),
            control: control.into(),
            name: name.into(),
            key: key.into(),
            texts: Vec::new(),
        }
    }

    #[test]
    fn the_helper_judges_risk_itself_and_a_file_dialog_is_always_risky() {
        use listed::Kind::{Click, Press, Type};
        // Nothing risky about a plain click, whatever the session says of
        // it: its word can only add.
        assert_eq!(own_risk(false, &act(Click, "Button", "Watch", ""), false), None);
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
        assert_eq!(own_risk(false, &act(Click, "MenuItem", "Open", ""), false), None);
        assert!(file_dialog_class("DUIViewWndClassName") && file_dialog_class("SHELLDLL_DefView"));
        assert!(!file_dialog_class("Button"));
    }

    #[test]
    fn the_helper_s_first_question_says_what_can_not_be_promised() {
        let q = first_question(STUDIO, "click Button \"Online\"", 60);
        assert!(q.contains(STUDIO) && q.contains("click Button \"Online\""), "{q}");
        assert!(q.contains("can not be limited to this program"), "{q}");
        assert!(q.contains("mouse only") && q.contains("60 s"), "{q}");
        let r = risky_question(STUDIO, "Synchronization", "click Button \"Transfer\"", "x", 30);
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
        assert!(dev_build(Path::new(r"C:\src\horadric\target\debug\horadric.exe"), false));
        assert!(dev_build(Path::new(r"C:\src\horadric\Target\release\horadric.exe"), false));
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
