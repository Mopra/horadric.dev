//! What the Folio runs beside its session, shared by both faces: `claude`
//! hidden for a login or a plugin command, and a `!` command in the
//! project's shell. Each runs on threads of its own and says it has
//! something through `wake`, which the face turns into a message to its
//! UI thread (a window message on Windows, the main queue on the Mac).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use horadric_core::folio::AuthStatus;

/// How often a login's waiter looks whether the CLI has ended.
const POLL: Duration = Duration::from_millis(250);

/// Tells the face's UI thread to look again. Called on any thread.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// `claude` with `args`, on pipes, with no window, and with the
/// environment a headless session gets, so it logs in where they look.
pub fn claude_command(args: &[&str]) -> Result<Command, String> {
    let exe = horadric_pty::headless::claude_exe().ok_or("claude was not found")?;
    let mut c = Command::new(exe);
    c.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    no_window(&mut c);
    let inherited = crate::console::inherited_env(crate::console::under_claude());
    let tags = [horadric_hooks::SESSION_ENV, horadric_hooks::OWNER_ENV];
    for name in (horadric_pty::headless::ENV_REMOVE.iter())
        .chain(&inherited)
        .chain(&tags)
    {
        c.env_remove(name);
    }
    Ok(c)
}

#[cfg(windows)]
fn no_window(c: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    c.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_window(_: &mut Command) {}

/// The output of `claude auth status --json`, when it says.
pub fn auth_status() -> Option<AuthStatus> {
    let out = claude_command(&["auth", "status", "--json"])
        .ok()?
        .stdin(Stdio::null())
        .output()
        .ok()?;
    AuthStatus::read(&String::from_utf8_lossy(&out.stdout))
}

/// What a login has said, filled by its threads.
#[derive(Debug, Clone, Default)]
pub struct Said {
    pub out: String,
    pub err: String,
    /// Its exit code once it ended, every word of it read by then.
    pub exit: Option<i32>,
    /// Who is logged in after a login that succeeded.
    pub status: Option<AuthStatus>,
}

/// `claude auth login` running hidden. Dropped, it ends.
pub struct Login {
    stdin: Option<ChildStdin>,
    child: Arc<Mutex<Child>>,
    said: Arc<Mutex<Said>>,
}

impl Drop for Login {
    fn drop(&mut self) {
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
        }
    }
}

impl Login {
    /// Starts it from `command`, waking the UI as it says anything and
    /// once it has ended.
    pub fn start(mut command: Command, wake: Wake) -> Result<Login, String> {
        let mut child = command
            .spawn()
            .map_err(|e| format!("claude auth login did not start: {e}"))?;
        let said: Arc<Mutex<Said>> = Arc::default();
        let readers = [
            (child.stdout.take()).map(|o| reader(o, Arc::clone(&said), false, wake.clone())),
            (child.stderr.take()).map(|e| reader(e, Arc::clone(&said), true, wake.clone())),
        ];
        let stdin = child.stdin.take();
        let child = Arc::new(Mutex::new(child));
        let (waited, told) = (Arc::clone(&child), Arc::clone(&said));
        std::thread::spawn(move || {
            let code = loop {
                match waited.lock().map(|mut c| c.try_wait()) {
                    Ok(Ok(Some(s))) => break s.code().unwrap_or(-1),
                    Ok(Ok(None)) => {}
                    _ => break -1,
                }
                std::thread::sleep(POLL);
            };
            for r in readers.into_iter().flatten() {
                let _ = r.join();
            }
            let who = (code == 0).then(auth_status).flatten();
            if let Ok(mut s) = told.lock() {
                s.status = who;
                s.exit = Some(code);
            }
            wake();
        });
        Ok(Login { stdin, child, said })
    }

    /// What it has said so far.
    pub fn said(&self) -> Said {
        self.said.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Types `code` into it, as the human pasted it. False when it went
    /// nowhere.
    pub fn send_code(&mut self, code: &str) -> bool {
        let Some(stdin) = self.stdin.as_mut() else {
            return false;
        };
        writeln!(stdin, "{code}")
            .and_then(|_| stdin.flush())
            .is_ok()
    }
}

/// Reads `from` into `said` as it comes, not by line: the CLI's prompt for
/// a code has no line end.
fn reader(
    mut from: impl Read + Send + 'static,
    said: Arc<Mutex<Said>>,
    err: bool,
    wake: Wake,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = from.read(&mut buf) {
            if n == 0 {
                break;
            }
            let text = String::from_utf8_lossy(&buf[..n]);
            if let Ok(mut s) = said.lock() {
                if err { &mut s.err } else { &mut s.out }.push_str(&text);
            }
            wake();
        }
    })
}

/// Runs `command` to its end on a thread, and gives `done` what it
/// printed, or why it failed: stdout when it succeeded, else whatever it
/// said on either stream.
pub fn run_hidden(
    mut command: Command,
    done: impl FnOnce(Result<String, String>) + Send + 'static,
) {
    std::thread::spawn(move || {
        let said = match command.stdin(Stdio::null()).output() {
            Ok(o) => {
                let out = String::from_utf8_lossy(&o.stdout).trim().to_string();
                let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
                match (o.status.success(), out.is_empty()) {
                    (true, _) => Ok(out),
                    (false, true) => Err(err),
                    (false, false) => Err(format!("{out}\n{err}").trim().to_string()),
                }
            }
            Err(e) => Err(e.to_string()),
        };
        done(said);
    });
}

/// What `claude auth logout` said, as the conversation tells it.
pub fn logout_said(result: std::io::Result<std::process::Output>) -> String {
    match result {
        Ok(o) if o.status.success() => {
            let out = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if out.is_empty() {
                "Logged out of Claude.".to_string()
            } else {
                out
            }
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
            format!("Not logged out: {err}")
        }
        Err(e) => format!("Not logged out: {e}"),
    }
}

/// Runs the `!` command `command` once in `dir` with `program`, the shell
/// a terminal gets, on a thread, and gives `done` what it printed on both
/// streams and its exit code.
pub fn run_shell(
    program: PathBuf,
    command: &str,
    dir: &Path,
    done: impl FnOnce(String, Option<i32>) + Send + 'static,
) {
    let mut c = Command::new(&program);
    c.args(crate::shell::run_args(&program, command))
        .current_dir(dir)
        .stdin(Stdio::null());
    no_window(&mut c);
    std::thread::spawn(move || {
        let (output, code) = match c.output() {
            Ok(o) => {
                let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
                let err = String::from_utf8_lossy(&o.stderr);
                if !err.trim().is_empty() {
                    if !text.is_empty() && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str(&err);
                }
                (text, o.status.code())
            }
            Err(e) => (format!("Could not start {}: {e}", program.display()), None),
        };
        done(output, code);
    });
}

/// A CLI's output as a block of code, so its lines and columns stay.
pub fn fenced(text: &str) -> String {
    format!("```text\n{}\n```", text.replace("```", "'''"))
}
