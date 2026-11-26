//! The desktop tools' reach into programs that run as administrator.
//!
//! Windows shows a session that is not elevated only the title bar of an
//! elevated program and drops the input it sends there. Some programs,
//! Sysmac Studio among them, restart themselves as administrator however
//! they are started, so the user can not simply run them without. For a
//! window like that `horadric mcp` starts `horadric desktop-helper` as
//! administrator (Windows asks the user, unless its settings elevate
//! without asking) and hands it the whole desktop call over a named pipe.
//! The helper asks for the human's leave, and tells the app how each
//! action went, through the MCP process, so the grants and what the human
//! watches work the same as for any other program. It works out where an
//! action lands itself, so that point reaches the app in the same `allow`
//! body, before the action, as it does for a program run normally.
//!
//! The pipe takes one client, never a remote one, and each end checks the
//! other is the process it expects, so nothing else on the machine can
//! drive the helper.
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
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, ERROR_PIPE_CONNECTED, HANDLE};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Com::{
    CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    GetNamedPipeServerProcessId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    GetProcessId, OpenProcess, TerminateProcess, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE,
};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

use crate::desktop::Desktop;

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

/// The helper's own call for an unnamed `desktop_wait`: what changed in its
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

/// An `allow` or `done` body for a window reached through the helper. The
/// program is named as running as administrator, which also keeps its
/// grant apart from the grant for the same program run normally.
pub fn as_administrator(mut body: Value) -> Value {
    if let Some(exe) = body
        .pointer("/window/exe")
        .and_then(Value::as_str)
        .map(str::to_string)
    {
        body["window"]["exe"] = json!(format!("{exe} as administrator"));
    }
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
/// none when it may now. A no holds for [`ASK_AGAIN_AFTER`] from the no;
/// a failure or a loss for as long from when the human was last asked, so
/// a helper that ran a good while is started again at once and one that
/// dies as it starts is not asked for over and over.
pub fn hold_left(
    setback: Setback,
    since_setback: Duration,
    since_asked: Duration,
) -> Option<Duration> {
    let since = match setback {
        Setback::Declined => since_setback,
        Setback::Failed | Setback::Lost => since_asked,
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
/// helper run as administrator, so the human knows why it asks.
pub fn elevating_body(window: Value) -> Value {
    json!({ "op": "desktop.elevating", "window": window })
}

/// A start of the helper on its own thread, Windows' question and all.
pub struct Starting {
    result: Receiver<Result<Helper, HelperStart>>,
    /// When Windows was asked.
    pub asked: Instant,
}

impl Starting {
    /// Asks Windows to start the helper, and returns while it asks.
    pub fn begin() -> Starting {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            // ShellExecuteEx wants COM, apartment threaded, on its thread.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
            }
            let _ = tx.send(Helper::start());
        });
        Starting {
            result: rx,
            asked: Instant::now(),
        }
    }

    /// The helper, or why there is none, once the start is over; none
    /// while Windows still asks after waiting `most`.
    pub fn wait(&self, most: Duration) -> Option<Result<Helper, HelperStart>> {
        match self.result.recv_timeout(most) {
            Ok(r) => Some(r),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(Err(HelperStart::Failed(
                "the helper's start ended without a word".into(),
            ))),
        }
    }
}

/// A desktop call as the MCP process sends it.
pub fn request_line(name: &str, args: &Value, settle: Duration) -> String {
    json!({ "name": name, "args": args, "settle_ms": settle.as_millis() as u64 }).to_string()
}

/// The helper, running as administrator, with the pipe to it.
pub struct Helper {
    /// Its lines, read on a thread of their own so a call can stop
    /// waiting for a helper that hangs.
    lines: Receiver<std::io::Result<String>>,
    writer: File,
    process: HANDLE,
}

// The process handle is only waited on, stopped and closed, which any
// thread may do, and the helper is made on the thread that starts it.
unsafe impl Send for Helper {}

impl Drop for Helper {
    fn drop(&mut self) {
        // Cutting the pipe is what ends the helper, so it never outlives
        // the session's use of it. The reading thread still holds its end,
        // so the pipe is disconnected rather than left to close.
        unsafe {
            let _ = DisconnectNamedPipe(HANDLE(self.writer.as_raw_handle()));
            let _ = CloseHandle(self.process);
        }
    }
}

impl Helper {
    /// Starts the helper as administrator and waits for it to connect.
    pub fn start() -> Result<Helper, HelperStart> {
        Self::start_inner().map_err(|e| match e {
            Ok(declined) => HelperStart::Declined(declined),
            Err(failed) => HelperStart::Failed(failed),
        })
    }

    /// The outer error is `Ok` when the user declined.
    fn start_inner() -> Result<Helper, Result<String, String>> {
        let me = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let name = format!(r"\\.\pipe\horadric-desktop-{me}-{nanos}");
        let pipe = unsafe {
            CreateNamedPipeW(
                &HSTRING::from(name.as_str()),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                1 << 16,
                1 << 16,
                0,
                None,
            )
        };
        if pipe.is_invalid() {
            return Err(Err(format!(
                "could not open a pipe for the helper: {}",
                windows::core::Error::from_thread()
            )));
        }
        // Owned from here, so every way out closes it.
        let pipe_file = unsafe { File::from_raw_handle(pipe.0 as _) };
        let writer = pipe_file.try_clone().map_err(|e| Err(e.to_string()))?;

        let exe = std::env::current_exe().map_err(|e| Err(e.to_string()))?;
        // A test binary sits in target\<profile>\deps, beside no
        // `desktop-helper` command, so the by hand tests use the built one.
        #[cfg(test)]
        let exe = exe
            .parent()
            .and_then(|deps| deps.parent())
            .map(|dir| dir.join("horadric.exe"))
            .unwrap_or(exe);
        let exe = HSTRING::from(exe.as_os_str());
        let params = HSTRING::from(format!("desktop-helper {name} {me}"));
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

        let raw = pipe.0 as usize;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let r = unsafe { ConnectNamedPipe(HANDLE(raw as _), None) };
            let ok = match r {
                Ok(()) => true,
                Err(e) => e.code() == ERROR_PIPE_CONNECTED.to_hresult(),
            };
            let _ = tx.send(ok);
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
                // Connecting ourselves frees the thread waiting on the pipe.
                let _ = OpenOptions::new().read(true).write(true).open(&name);
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
        let mut client = 0u32;
        let checked =
            unsafe { GetNamedPipeClientProcessId(HANDLE(pipe_file.as_raw_handle()), &mut client) };
        if !connected || checked.is_err() || client != helper_pid {
            unsafe {
                let _ = CloseHandle(process);
            }
            return Err(Err(
                "something other than the helper answered on its pipe".into()
            ));
        }
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(pipe_file);
            loop {
                let mut line = String::new();
                let read = reader.read_line(&mut line);
                let end = !matches!(read, Ok(n) if n > 0);
                if tx.send(read.map(|_| line)).is_err() || end {
                    break;
                }
            }
        });
        Ok(Helper {
            lines,
            writer,
            process,
        })
    }

    /// Stops a helper that went silent. Should Windows not let this
    /// session stop it, cutting the pipe on drop ends it as soon as it is
    /// out of what it was doing.
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
        let lost = |e: std::io::Error| format!("lost the helper: {e}");
        writeln!(self.writer, "{}", request_line(name, args, settle)).map_err(lost)?;
        loop {
            // The clock runs only while the helper has the call: the app's
            // questions to the human are waited for here, not there.
            let line = match self.lines.recv_timeout(SILENCE) {
                Ok(Ok(line)) if !line.is_empty() => line,
                Ok(Ok(_)) | Err(RecvTimeoutError::Disconnected) => {
                    return Err("the helper ended".into())
                }
                Ok(Err(e)) => return Err(lost(e)),
                Err(RecvTimeoutError::Timeout) => {
                    self.stop();
                    return Err(format!(
                        "the helper did not answer in {} s, so it was stopped",
                        SILENCE.as_secs()
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

/// `horadric desktop-helper <pipe> <pid>`: serves desktop calls from the
/// MCP process `pid` until it closes the pipe.
pub fn run(args: &[String]) -> Result<(), String> {
    let (name, parent) = match args {
        [name, pid] => (name, pid.parse::<u32>().map_err(|e| e.to_string())?),
        _ => return Err("usage: horadric desktop-helper <pipe> <pid>".into()),
    };
    if !name.starts_with(r"\\.\pipe\horadric-desktop-") {
        return Err("not a desktop helper pipe".into());
    }
    // Held from before the pipe's check, so the process watched is the
    // one checked, not another given its id later.
    let session = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, parent) }
        .map_err(|e| format!("could not watch the session that started this helper: {e}"))?;
    let pipe = OpenOptions::new()
        .read(true)
        .write(true)
        .open(name)
        .map_err(|e| format!("could not open {name}: {e}"))?;
    let mut server = 0u32;
    unsafe { GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &mut server) }
        .map_err(|e| e.to_string())?;
    if server != parent {
        return Err("the pipe is not held by the session that started this helper".into());
    }
    // The pipe closing ends the helper only once it is back reading it,
    // and an elevated process must not linger past its session in the
    // middle of a long call either.
    let raw = session.0 as usize;
    thread::spawn(move || {
        unsafe { WaitForSingleObject(HANDLE(raw as _), INFINITE) };
        std::process::exit(0);
    });
    let mut writer = pipe.try_clone().map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(pipe);
    let mut desktop = Desktop::for_helper();
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
        let mut broken = None;
        let mut app = |body: Value| -> Result<Value, String> {
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
        let result = desktop.tool(name, &call_args, &mut app, settle);
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
    fn an_administrator_window_is_its_own_program() {
        let body = json!({"window": {"exe": "SysmacStudio.exe", "pid": 4}, "action": "see"});
        let marked = as_administrator(body);
        assert_eq!(marked["window"]["exe"], "SysmacStudio.exe as administrator");
        assert_eq!(marked["window"]["pid"], 4);
        assert_eq!(marked["administrator"], true);
        assert_eq!(as_administrator(json!({}))["administrator"], true);
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
        assert_eq!(hold_left(Setback::Failed, s(0), s(60)), Some(s(60)));
        assert_eq!(hold_left(Setback::Failed, s(0), s(121)), None);
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
        let t = still_asking_text("SysmacStudio.exe", Duration::from_millis(90_500));
        assert!(
            t.contains("SysmacStudio.exe") && t.contains("90 s ago"),
            "{t}"
        );
        assert!(
            t.contains("Nothing was done") && t.contains("Call again"),
            "{t}"
        );
    }

    #[test]
    fn the_app_is_told_before_windows_asks() {
        let b = elevating_body(json!({"exe": "SysmacStudio.exe", "pid": 4}));
        assert_eq!(b["op"], "desktop.elevating");
        assert_eq!(b["window"]["exe"], "SysmacStudio.exe");
    }

    #[test]
    fn a_request_carries_name_args_and_settle() {
        let line = request_line(
            "desktop_click",
            &json!({"ref": "e3"}),
            Duration::from_millis(250),
        );
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["name"], "desktop_click");
        assert_eq!(v["args"]["ref"], "e3");
        assert_eq!(v["settle_ms"], 250);
    }
}
