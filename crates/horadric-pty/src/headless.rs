//! A headless `claude.exe` on pipes, speaking stream-json. The session's
//! conversation is drawn by Horadric (the Folio), not a terminal. See
//! docs/plan/headless.md.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Threading::CREATE_NO_WINDOW;

/// The flags every headless start passes. Values always in the `--x=value`
/// form, so a value can never be read as a flag of its own.
pub const FLAGS: &[&str] = &[
    "--input-format=stream-json",
    "--output-format=stream-json",
    "--verbose",
    "--include-partial-messages",
    "--permission-prompt-tool=stdio",
];

/// Taken out of the environment: the API key variables would bill the API
/// instead of the human's subscription, and the rest name a parent Claude
/// session the new one is not part of.
pub const ENV_REMOVE: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
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

/// How long a dropped session gets to end on its own.
const GRACE: Duration = Duration::from_secs(3);

/// How many of the last stderr lines are kept, for saying why a start failed.
const STDERR_KEPT: usize = 40;

/// How long the stream's end waits for stderr to be read to its end, so
/// whoever is told the stream ended can say why from it.
const STDERR_WAIT: Duration = Duration::from_millis(500);

/// What [`Headless::start_with`] may add to a plain start.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The `claude.exe` to run. Found with [`claude_exe`] when None.
    pub program: Option<PathBuf>,
    /// After [`FLAGS`] and `--resume`, such as `--model=sonnet`.
    pub args: Vec<String>,
    /// Added to the environment, replacing any value it had.
    pub env_set: Vec<(String, String)>,
    /// Removed from the environment, beside [`ENV_REMOVE`].
    pub env_remove: Vec<String>,
}

pub struct Headless {
    /// Taken by Drop, which hands it to a thread that waits for the end.
    child: Option<Child>,
    /// Holds claude.exe and everything it starts. Closing it ends them all,
    /// so a Headless dropped, or a Horadric that dies, leaves nothing behind.
    job: Option<OwnedHandle>,
    /// To the writer thread, which owns stdin, so a CLI that stops reading
    /// can never block the caller.
    input: Mutex<Option<Sender<Vec<u8>>>>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    next_request: AtomicU64,
}

impl Headless {
    /// Starts `claude.exe` in `cwd`, resuming `resume` when given. Each stdout
    /// line goes to `on_line` on a reader thread of its own; the stream ending
    /// calls it with None.
    pub fn start(
        cwd: &Path,
        resume: Option<&str>,
        on_line: impl FnMut(Option<String>) + Send + 'static,
    ) -> io::Result<Headless> {
        Headless::start_with(cwd, resume, &Options::default(), on_line)
    }

    /// [`Headless::start`] with a chosen program, more flags or environment.
    pub fn start_with(
        cwd: &Path,
        resume: Option<&str>,
        options: &Options,
        mut on_line: impl FnMut(Option<String>) + Send + 'static,
    ) -> io::Result<Headless> {
        let program = match &options.program {
            Some(p) => p.clone(),
            None => claude_exe()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "claude.exe not found"))?,
        };
        let mut command = Command::new(program);
        command
            .args(arguments(resume, &options.args))
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW.0);
        for name in ENV_REMOVE {
            command.env_remove(name);
        }
        for name in &options.env_remove {
            command.env_remove(name);
        }
        for (k, v) in &options.env_set {
            command.env(k, v);
        }

        let job = kill_on_close_job();
        if let Err(e) = &job {
            eprintln!(
                "horadric: no kill-on-close job for claude.exe, so it may outlive Horadric: {e}"
            );
        }
        let job = job.ok();
        let mut child = command.spawn()?;
        // Assigned after the start, which only misses what claude.exe starts
        // in its first instants, before it has read its settings. The child
        // kill in Drop covers it if this fails.
        if let Some(job) = &job {
            unsafe {
                let _ = AssignProcessToJobObject(
                    HANDLE(job.as_raw_handle()),
                    HANDLE(child.as_raw_handle()),
                );
            }
        }

        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");

        let (input, rx) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
            let mut stdin = stdin;
            for bytes in rx {
                if stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_err() {
                    break;
                }
            }
        });

        let (stderr_done, stderr_read) = mpsc::channel::<()>();
        thread::spawn(move || {
            read_lines(stdout, |line| on_line(Some(line)));
            let _ = stderr_read.recv_timeout(STDERR_WAIT);
            on_line(None);
        });

        let kept = Arc::new(Mutex::new(VecDeque::new()));
        let tail = kept.clone();
        thread::spawn(move || {
            read_lines(stderr, |line| {
                if let Ok(mut t) = tail.lock() {
                    if t.len() == STDERR_KEPT {
                        t.pop_front();
                    }
                    t.push_back(line);
                }
            });
            let _ = stderr_done.send(());
        });

        Ok(Headless {
            child: Some(child),
            job,
            input: Mutex::new(Some(input)),
            stderr: kept,
            next_request: AtomicU64::new(1),
        })
    }

    pub fn send_prompt(&self, text: &str) -> io::Result<()> {
        self.send_line(&prompt_line(text))
    }

    /// Stops the turn in progress. The CLI ends it with a `result` and
    /// stays up for the next prompt.
    pub fn interrupt(&self) -> io::Result<()> {
        self.send_line(&control_request(self.request_id(), "interrupt"))
    }

    /// Writes one raw line, such as the `control_response` from
    /// `Folio::answer`.
    pub fn send_line(&self, line: &str) -> io::Result<()> {
        let mut bytes = line.trim_end_matches(['\r', '\n']).as_bytes().to_vec();
        bytes.push(b'\n');
        let input = self
            .input
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?;
        input
            .as_ref()
            .and_then(|tx| tx.send(bytes).ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "claude.exe has ended"))
    }

    /// Closes stdin, which the CLI takes as the end of the session: it
    /// finishes and exits on its own.
    pub fn close_input(&self) {
        if let Ok(mut input) = self.input.lock() {
            input.take();
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.as_ref().map_or(0, Child::id)
    }

    /// Whether claude.exe is still running.
    pub fn running(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|c| matches!(c.try_wait(), Ok(None)))
    }

    /// The last lines claude.exe wrote to stderr, oldest first. Its errors
    /// before the stream starts (a bad flag, a missing login) only go there.
    pub fn stderr_tail(&self) -> Vec<String> {
        self.stderr
            .lock()
            .map(|t| t.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn request_id(&self) -> String {
        format!(
            "horadric_{}",
            self.next_request.fetch_add(1, Ordering::Relaxed)
        )
    }
}

impl Drop for Headless {
    /// Closes stdin so the CLI can end the session itself (its SessionEnd
    /// hook fires), and ends the whole tree if it has not after a grace
    /// period. The wait is on a thread, so dropping never blocks the UI; if
    /// Horadric exits first, the job's last handle closes and ends it all.
    fn drop(&mut self) {
        self.close_input();
        let Some(mut child) = self.child.take() else {
            return;
        };
        let job = self.job.take();
        thread::spawn(move || {
            let deadline = Instant::now() + GRACE;
            while Instant::now() < deadline {
                if !matches!(child.try_wait(), Ok(None)) {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            if let Some(job) = &job {
                unsafe {
                    let _ = TerminateJobObject(HANDLE(job.as_raw_handle()), 1);
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        });
    }
}

/// The arguments after the program.
pub fn arguments(resume: Option<&str>, extra: &[String]) -> Vec<String> {
    let mut args: Vec<String> = FLAGS.iter().map(|f| f.to_string()).collect();
    if let Some(id) = resume {
        args.push(format!("--resume={id}"));
    }
    args.extend(extra.iter().cloned());
    args
}

/// One prompt as the stdin line the CLI reads.
pub fn prompt_line(text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": text},
        "parent_tool_use_id": null,
        "session_id": "",
    })
    .to_string()
}

/// A `control_request` with no fields beyond its subtype.
pub fn control_request(request_id: String, subtype: &str) -> String {
    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {"subtype": subtype},
    })
    .to_string()
}

/// The installed `claude.exe`: on `PATH`, then where the native installer
/// and winget put it. Never the npm `.cmd` shim, which needs a console and
/// a command interpreter between us and the pipes.
pub fn claude_exe() -> Option<PathBuf> {
    let var = |name| std::env::var_os(name).map(PathBuf::from);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    if let Some(p) = var("USERPROFILE") {
        dirs.push(p.join(".local").join("bin"));
    }
    if let Some(l) = var("LOCALAPPDATA") {
        dirs.push(l.join("Microsoft").join("WinGet").join("Links"));
    }
    let joined = std::env::join_paths(dirs).ok()?;
    crate::find_program("claude", &joined, &[".exe"], Path::is_file)
}

/// Calls `each` with every line of `from`, without its line end. No length
/// cap: a tool result or an image can be megabytes on one line.
fn read_lines(from: impl Read, mut each: impl FnMut(String)) {
    let mut reader = BufReader::with_capacity(64 * 1024, from);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                while buf.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
                    buf.pop();
                }
                if !buf.is_empty() {
                    each(String::from_utf8_lossy(&buf).into_owned());
                }
            }
        }
    }
}

/// A job that ends everything in it when its last handle closes.
fn kill_on_close_job() -> windows::core::Result<OwnedHandle> {
    unsafe {
        let job = CreateJobObjectW(None, None)?;
        let job = OwnedHandle::from_raw_handle(job.0);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            HANDLE(job.as_raw_handle()),
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const c_void,
            std::mem::size_of_val(&limits) as u32,
        )?;
        Ok(job)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_take_their_values_after_an_equals_sign() {
        let args = arguments(Some("abc"), &["--model=sonnet".into()]);
        assert_eq!(args[0], "--input-format=stream-json");
        assert!(args.contains(&"--resume=abc".to_string()));
        assert_eq!(args.last().unwrap(), "--model=sonnet");
        assert!(!args.iter().any(|a| a == "-p"));
    }

    #[test]
    fn a_prompt_is_one_user_message_on_one_line() {
        let line = prompt_line("two\nlines");
        assert!(!line.contains('\n'));
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["content"], "two\nlines");
    }

    #[test]
    fn an_interrupt_is_a_control_request() {
        let v: serde_json::Value =
            serde_json::from_str(&control_request("r1".into(), "interrupt")).unwrap();
        assert_eq!(v["type"], "control_request");
        assert_eq!(v["request"]["subtype"], "interrupt");
        assert_eq!(v["request_id"], "r1");
    }

    #[test]
    fn lines_come_whole_without_their_ends_and_with_no_cap() {
        let long = "x".repeat(3 * 1024 * 1024);
        let input = format!("a\r\n\n{long}\nlast");
        let mut got = Vec::new();
        read_lines(input.as_bytes(), |l| got.push(l));
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], "a");
        assert_eq!(got[1].len(), long.len());
        assert_eq!(got[2], "last");
    }
}
