//! `horadric remote open` and `close`, and the `remote_` tools of `horadric mcp`: a
//! remote machine's desktop, served by its own Horadric, watched in a tab
//! of the browser pane through an SSH tunnel. The pure half is
//! `horadric_core::remote`; this half starts `ssh`, remembers the tunnels
//! and fetches through them.
//!
//! A tunnel is an `ssh -N -L` started apart from this process, so it
//! outlives the command that opened it, and remembered in the state
//! folder, so the next `remote open` and every session's MCP server use
//! the same one while it lives. Reading, starting and writing a tunnel is
//! done under a lock file, so two sessions opening the same machine at
//! once share one tunnel rather than leaving one running unremembered.
//!
//! The token is read over `ssh` before anything else, goes out only in a
//! header, and only after the far end of the tunnel has proven it holds
//! the same token, so a program that took the port learns nothing. It
//! never leaves this module in anything shown: what is said or printed
//! has it masked. A tool does nothing before the app has asked the human.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use horadric_core::remote::{self, Identity, Response, Tunnel, Unread};
use horadric_hooks::{OWNER_ENV, SESSION_ENV};
use serde_json::{json, Value};

/// How long a fresh tunnel may take to log in and start forwarding.
const TUNNEL_WAIT: Duration = Duration::from_secs(10);

/// How long the proof or an input request may take, all of it.
const SHORT_WAIT: Duration = Duration::from_secs(20);

/// How long a frame may take, all of it: a large screen over a slow link.
const FRAME_WAIT: Duration = Duration::from_secs(30);

/// How long letting go after a failure may take: it is a courtesy on top
/// of the remote's own watchdog, not worth a long wait.
const RELEASE_WAIT: Duration = Duration::from_secs(5);

/// How long the `ssh` that reads the token may take before it is killed.
const TOKEN_WAIT: Duration = Duration::from_secs(30);

/// The env that points everything here at a remote dev instance.
const DEV_ENV: &str = "HORADRIC_REMOTE_DEV";

/// A lock on the tunnels file older than this was left by a process that
/// died holding it: checking a kept tunnel and starting a new one take
/// [`SHORT_WAIT`] twice and [`TUNNEL_WAIT`] at most.
const LOCK_STALE: Duration = Duration::from_secs(60);

/// How long taking the lock may be denied before the folder is taken for
/// one that can not be written. A lock file being removed is denied for
/// a moment only.
const DENIED_WAIT: Duration = Duration::from_secs(2);

/// `horadric remote open|close [--dev] <destination>`.
pub fn run(args: &[String]) -> Result<(), String> {
    let usage = "usage: horadric remote open|close [--dev] <ssh destination>";
    let (dev_flag, rest): (Vec<&String>, Vec<&String>) =
        args.iter().skip(1).partition(|a| *a == "--dev");
    let dev = !dev_flag.is_empty() || dev_env();
    match (args.first().map(String::as_str), rest.as_slice()) {
        (Some("close"), [dest]) => {
            if close(dest, remote::remote_port(dev))? {
                println!("Closed the tunnel to {dest}.");
            } else {
                println!("No tunnel to {dest} was open.");
            }
            // Without this a session allowed before would open the tunnel
            // again at its next call, and closing would stop nothing.
            match tell_app(&json!({ "op": "remote_forget", "ssh": dest })) {
                Ok(_) => println!("Horadric asks again before a session watches {dest}."),
                Err(e) => println!(
                    "Horadric was not told to ask again ({e}): a session allowed to watch \
                     {dest} still is, until Horadric restarts."
                ),
            }
            Ok(())
        }
        (Some("open"), [dest]) => {
            let in_session = std::env::var(SESSION_ENV).is_ok_and(|s| !s.is_empty());
            let opened = open(dest, dev, |url| {
                if in_session {
                    tell_app(&json!({ "op": "open", "url": url }))
                        .map(|_| "in this project's browser pane".to_string())
                } else {
                    open_outside(url).map(|()| "in the default browser".to_string())
                }
            })?;
            println!(
                "Opened {} {} ({}).",
                opened.shown_url, opened.answer, opened.via
            );
            Ok(())
        }
        _ => Err(usage.into()),
    }
}

/// One call to the app that owns this terminal's session, as the command
/// typed in a Horadric terminal makes it.
fn tell_app(body: &Value) -> Result<Value, String> {
    let session = std::env::var(SESSION_ENV).ok().filter(|s| !s.is_empty());
    let owner = std::env::var(OWNER_ENV)
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(horadric_hooks::port);
    crate::mcp::ask_app(owner, session.as_deref(), body)
}

/// Whether the env asks for the remote's dev instance.
pub fn dev_env() -> bool {
    std::env::var(DEV_ENV).is_ok_and(|v| !v.is_empty() && v != "0")
}

/// A viewer opened: the URL as it may be shown, what the opener said of
/// it, and how far the tunnel came, the token masked in all of them.
pub struct Opened {
    pub shown_url: String,
    pub answer: String,
    pub via: String,
}

/// Opens `dest`'s screen with `show`, which is handed the URL with the
/// token in it and says how it went; the token is masked in what it says.
pub fn open(
    dest: &str,
    dev: bool,
    mut show: impl FnMut(&str) -> Result<String, String>,
) -> Result<Opened, String> {
    through(dest, dev, |link| {
        // A remote that serves nothing would show an empty tab: say why
        // here. The page itself needs no token, so it would not tell.
        let r = link
            .get(remote::DISPLAY_PATH, SHORT_WAIT, remote::SMALL_CAP)
            .map_err(Failed::Other)?;
        refused(&r)?;
        if r.status != 200 {
            return Err(Failed::Other(remote::status_refusal(r.status, &said(&r))));
        }
        let url = remote::view_url(link.port, &link.token);
        let answer = show(&url).map_err(Failed::Other)?;
        Ok(Opened {
            shown_url: remote::mask(&url, &link.token),
            answer: remote::mask(&answer, &link.token),
            via: format!("tunnel to {dest} on local port {}", link.port),
        })
    })
}

/// One frame of `dest`'s screen at most `width` wide, and the line that
/// says how large the screen is.
pub fn screenshot(dest: &str, dev: bool, width: u32) -> Result<(Vec<u8>, String), String> {
    through(dest, dev, |link| {
        let r = link
            .get(&remote::frame_path(width), FRAME_WAIT, remote::FRAME_CAP)
            .map_err(Failed::Other)?;
        refused(&r)?;
        if r.status != 200 {
            return Err(Failed::Other(remote::status_refusal(r.status, &said(&r))));
        }
        if r.body.is_empty() {
            return Err(Failed::Other("the remote sent an empty picture".into()));
        }
        let line = remote::screen_line(&r, remote::jpeg_size(&r.body));
        Ok((r.body, line))
    })
}

/// Posts `bodies` to `dest`'s screen, each a sequence the remote runs
/// whole, one after the other is answered. Stops at the first the remote
/// does not take, with what it said, and then asks it to let go of every
/// key and button: the remote does that itself for a sequence that fails
/// midway, but not for an answer that never arrived.
pub fn send_input(dest: &str, dev: bool, bodies: &[String]) -> Result<(), String> {
    let mut sent = 0;
    through(dest, dev, |link| {
        while sent < bodies.len() {
            let failed = match link.post(remote::INPUT_PATH, &bodies[sent], SHORT_WAIT) {
                Ok(r) if r.status == 204 => {
                    sent += 1;
                    continue;
                }
                // Nothing ran, and the token is not the remote's: there
                // is nothing to let go of and nothing to ask it with.
                Ok(r) if r.status == 403 => return Err(Failed::Refused),
                Ok(r) => remote::input_refusal(r.status, &said(&r)),
                Err(e) => e,
            };
            release(link);
            return Err(Failed::Other(if sent > 0 {
                format!("{failed} (after {sent} of {} pieces went)", bodies.len())
            } else {
                failed
            }));
        }
        Ok(())
    })
}

/// Asks the remote to let go of everything it holds down, best effort.
/// The far end proves itself again first: what failed may have been the
/// tunnel, and the token goes to nothing unproven.
fn release(link: &Link) {
    if matches!(hello(link.port, &link.token), Hello::Proven) {
        let _ = link.post(remote::INPUT_PATH, remote::RELEASE_BODY, RELEASE_WAIT);
    }
}

/// What the remote said in an answer's body, as text.
fn said(r: &Response) -> String {
    String::from_utf8_lossy(&r.body).into_owned()
}

/// A way through to a remote's screen: the local port of a live tunnel
/// and the token, which the far end of that tunnel has proven it holds.
struct Link {
    port: u16,
    token: String,
}

impl Link {
    fn get(&self, path: &str, wait: Duration, cap: usize) -> Result<Response, String> {
        let head = remote::request_head("GET", self.port, path, Some(&self.token), None);
        request(self.port, head.into_bytes(), wait, cap)
    }

    fn post(&self, path: &str, body: &str, wait: Duration) -> Result<Response, String> {
        let head =
            remote::request_head("POST", self.port, path, Some(&self.token), Some(body.len()));
        let mut bytes = head.into_bytes();
        bytes.extend_from_slice(body.as_bytes());
        request(self.port, bytes, wait, remote::SMALL_CAP)
    }
}

/// Why something asked through a [`Link`] did not come back.
enum Failed {
    /// A 403: the token is not the remote's any more, since it makes a
    /// fresh one at each of its starts.
    Refused,
    Other(String),
}

/// A 403 as the failure it is.
fn refused(r: &Response) -> Result<(), Failed> {
    if r.status == 403 {
        Err(Failed::Refused)
    } else {
        Ok(())
    }
}

/// Runs `ask` through a link to `dest`. When the remote refuses the token,
/// it is read again over `ssh` and `ask` runs once more, if it is another
/// now. The token is masked in what a failure says.
fn through<T>(
    dest: &str,
    dev: bool,
    mut ask: impl FnMut(&Link) -> Result<T, Failed>,
) -> Result<T, String> {
    let link = reach(dest, dev)?;
    let stale = match ask(&link) {
        Ok(t) => return Ok(t),
        Err(Failed::Other(e)) => return Err(remote::mask(&e, &link.token)),
        Err(Failed::Refused) => link.token,
    };
    keep_token(dest, dev, None);
    let link = reach(dest, dev)?;
    if link.token == stale {
        return Err(remote::status_refusal(403, ""));
    }
    match ask(&link) {
        Ok(t) => Ok(t),
        Err(Failed::Other(e)) => Err(remote::mask(&e, &link.token)),
        Err(Failed::Refused) => Err(remote::status_refusal(403, "")),
    }
}

/// What is said when the far end of a tunnel does not prove itself.
fn unproven(dest: &str) -> String {
    format!(
        "something else answers on the port of {dest}'s screen: it could not prove it holds \
         the token, so nothing was sent to it and the tunnel was closed. A Horadric there \
         from before this proof answers the same way: update it."
    )
}

/// A link to `dest`: the token first, so a host that has none is left
/// with no `ssh` running, then a tunnel whose far end proves it holds
/// that token. A token remembered from an earlier call that the far end
/// does not prove is read again once, since the remote makes a fresh one
/// at each of its starts.
fn reach(dest: &str, dev: bool) -> Result<Link, String> {
    if let Some(why) = remote::refusal(dest) {
        return Err(why);
    }
    let remote_port = remote::remote_port(dev);
    let (mut token, remembered) = match kept_token(dest, dev) {
        Some(t) => (t, true),
        None => (read_token(dest, dev)?, false),
    };
    let mut at = tunnel(dest, remote_port, &token)?;
    if !at.proven && remembered {
        keep_token(dest, dev, None);
        let fresh = read_token(dest, dev)?;
        if fresh != token {
            token = fresh;
            at = tunnel(dest, remote_port, &token)?;
        }
    }
    if !at.proven {
        keep_token(dest, dev, None);
        let _ = close(dest, remote_port);
        return Err(unproven(dest));
    }
    keep_token(dest, dev, Some(&token));
    Ok(Link {
        port: at.port,
        token,
    })
}

/// The tokens this process has read, by destination and dev, so each
/// click is not a whole `ssh` login. Only in memory: the MCP server's,
/// for as long as its session runs.
static TOKENS: Mutex<Vec<(String, bool, String)>> = Mutex::new(Vec::new());

fn kept_token(dest: &str, dev: bool) -> Option<String> {
    let kept = TOKENS.lock().unwrap_or_else(|e| e.into_inner());
    kept.iter()
        .find(|(d, v, _)| d == dest && *v == dev)
        .map(|(_, _, t)| t.clone())
}

/// Remembers `token` for `dest`, or forgets the one kept when None.
fn keep_token(dest: &str, dev: bool, token: Option<&str>) {
    let mut kept = TOKENS.lock().unwrap_or_else(|e| e.into_inner());
    kept.retain(|(d, v, _)| !(d == dest && *v == dev));
    if let Some(t) = token {
        kept.push((dest.to_string(), dev, t.to_string()));
    }
}

fn ssh_program() -> PathBuf {
    // The one Windows ships talks to the Windows ssh-agent, Git's does not.
    #[cfg(windows)]
    if let Ok(root) = std::env::var("SystemRoot") {
        let p = PathBuf::from(root).join(r"System32\OpenSSH\ssh.exe");
        if p.is_file() {
            return p;
        }
    }
    PathBuf::from("ssh")
}

fn tunnels_path() -> Result<PathBuf, String> {
    horadric_hooks::state_dir()
        .map(|d| d.join(remote::TUNNELS_FILE))
        .ok_or_else(|| "there is no state folder".into())
}

fn read_tunnels(path: &Path) -> Vec<Tunnel> {
    remote::parse_tunnels(&std::fs::read_to_string(path).unwrap_or_default())
}

fn write_tunnels(path: &Path, list: &[Tunnel]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(path, remote::tunnels_text(list)).map_err(|e| e.to_string())
}

/// The lock file beside the tunnels file, held while it is read, a tunnel
/// started and it written again. A file made with `create_new`, since
/// that is the one exclusive step every Windows file system has. It holds
/// a mark of who took it, and is removed when dropped only while it still
/// holds that mark: a lock taken over as stale belongs to the one who
/// took it, and removing it would let a third in beside them.
struct Lock {
    path: PathBuf,
    mark: String,
}

/// Tells this process's locks apart, with its pid.
static LOCKS: AtomicU64 = AtomicU64::new(0);

impl Lock {
    fn take(tunnels: &Path) -> Result<Lock, String> {
        let path = tunnels.with_extension("lock");
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let _ = std::fs::create_dir_all(&dir);
        let unwritable = || format!("cannot write the tunnel list in {}", dir.display());
        let mark = format!(
            "{} {}",
            std::process::id(),
            LOCKS.fetch_add(1, Ordering::Relaxed)
        );
        let deadline = Instant::now() + LOCK_STALE + Duration::from_secs(5);
        let mut denied_since: Option<Instant> = None;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    if file.write_all(mark.as_bytes()).is_err() {
                        drop(file);
                        let _ = std::fs::remove_file(&path);
                        return Err(unwritable());
                    }
                    return Ok(Lock { path, mark });
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    denied_since = None;
                    let age = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok());
                    if age.is_some_and(|a| a > LOCK_STALE) {
                        let _ = std::fs::remove_file(&path);
                    }
                }
                Err(e) if e.kind() == ErrorKind::PermissionDenied => {
                    if denied_since.get_or_insert_with(Instant::now).elapsed() > DENIED_WAIT {
                        return Err(unwritable());
                    }
                }
                Err(e) => return Err(format!("could not lock the tunnel list: {e}")),
            }
            if Instant::now() > deadline {
                return Err("another session held the tunnel list too long".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        if std::fs::read_to_string(&self.path).is_ok_and(|held| held == self.mark) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Whether the process under a remembered tunnel's pid is still the `ssh`
/// that was kept with it.
fn ours(t: &Tunnel) -> bool {
    remote::same_process(t.identity.as_ref(), ssh_identity(t.pid).as_ref())
}

/// A tunnel reached: its local port, and whether its far end proved it
/// holds the token.
struct Reached {
    port: u16,
    proven: bool,
}

/// A tunnel's local port for `dest`, the one remembered when its `ssh`
/// still runs and its far end answers, else a new one. Up means our
/// `ssh` and an answer to the proof through it, not only a port that
/// takes connections. A far end that answers the proof wrongly is given
/// back as not proven, the tunnel kept: the token may be an old one.
fn tunnel(dest: &str, remote_port: u16, token: &str) -> Result<Reached, String> {
    let path = tunnels_path()?;
    let _lock = Lock::take(&path)?;
    let known = read_tunnels(&path);
    if let Some(t) = remote::find_tunnel(&known, dest, remote_port) {
        if ours(t) && answers(t.port) {
            match hello(t.port, token) {
                Hello::Proven => {
                    return Ok(Reached {
                        port: t.port,
                        proven: true,
                    })
                }
                Hello::Wrong => {
                    return Ok(Reached {
                        port: t.port,
                        proven: false,
                    })
                }
                Hello::Silent(_) => {}
            }
        }
        // An ssh that runs but no longer forwards would linger forever
        // once its entry is replaced, since nothing else remembers it.
        end(t);
    }
    let local = free_port()?;
    let log = path.with_file_name(format!("remote-ssh-{local}.log"));
    let started = start_tunnel(dest, local, remote_port, token, &log);
    let _ = std::fs::remove_file(&log);
    let (mine, proven) = started?;
    // The lock keeps sessions apart, but one taken from a holder thought
    // dead, or a Horadric from before the lock, may have kept a tunnel
    // meanwhile: one is enough, and theirs is already remembered.
    let known = read_tunnels(&path);
    if let Some(t) = remote::find_tunnel(&known, dest, remote_port) {
        if t.pid != mine.pid
            && ours(t)
            && answers(t.port)
            && matches!(hello(t.port, token), Hello::Proven)
        {
            end(&mine);
            return Ok(Reached {
                port: t.port,
                proven: true,
            });
        }
    }
    write_tunnels(&path, &remote::with_tunnel(&known, mine))?;
    Ok(Reached {
        port: local,
        proven,
    })
}

/// Ends the tunnel kept for `dest` and forgets it; false when none was.
fn close(dest: &str, remote_port: u16) -> Result<bool, String> {
    if let Some(why) = remote::refusal(dest) {
        return Err(why);
    }
    let path = tunnels_path()?;
    let _lock = Lock::take(&path)?;
    let known = read_tunnels(&path);
    let Some(t) = remote::find_tunnel(&known, dest, remote_port) else {
        return Ok(false);
    };
    end(t);
    write_tunnels(&path, &remote::without_tunnel(&known, dest, remote_port))?;
    Ok(true)
}

/// What the `ssh` running under `pid` is, None when no `ssh` runs there.
/// With `end_when`, the process is ended if that is what it is: seen and
/// ended through one handle, so the pid can not go to another process
/// between the look and the kill.
#[cfg(windows)]
fn ssh_process(pid: u32, end_when: Option<&Identity>) -> Option<Identity> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, QueryFullProcessImageNameW,
        TerminateProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_TERMINATE,
    };
    const STILL_ACTIVE: u32 = 259;
    let access = if end_when.is_some() {
        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE
    } else {
        PROCESS_QUERY_LIMITED_INFORMATION
    };
    // SAFETY: the handle is opened, queried and closed here, and every
    // buffer outlives the call that fills it.
    unsafe {
        let h = OpenProcess(access, false, pid).ok()?;
        let mut code = 0u32;
        let running = GetExitCodeProcess(h, &mut code).is_ok() && code == STILL_ACTIVE;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let named =
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
                .is_ok()
                && String::from_utf16_lossy(&buf[..len as usize])
                    .to_ascii_lowercase()
                    .ends_with("ssh.exe");
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        let timed = GetProcessTimes(h, &mut created, &mut exited, &mut kernel, &mut user).is_ok();
        let is = (running && named && timed).then(|| {
            Identity::Created(
                (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime),
            )
        });
        if is.is_some() && is.as_ref() == end_when {
            let _ = TerminateProcess(h, 1);
        }
        let _ = CloseHandle(h);
        is
    }
}

#[cfg(windows)]
fn ssh_identity(pid: u32) -> Option<Identity> {
    ssh_process(pid, None)
}

/// Ends a remembered tunnel's `ssh`, if the process under its pid is
/// still the one kept with it. Anything else is left alone: the pid of a
/// tunnel long gone may be another program's by now.
#[cfg(windows)]
fn end(t: &Tunnel) {
    if let Some(kept) = &t.identity {
        ssh_process(t.pid, Some(kept));
    }
}

/// What the process running under `pid` is: when it started, as `ps`
/// prints it, in the C locale so two readings of one process agree.
#[cfg(not(windows))]
fn ssh_identity(pid: u32) -> Option<Identity> {
    let out = Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let started = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !started.is_empty()).then_some(Identity::Started(started))
}

/// Ends a remembered tunnel's `ssh`, if the process under its pid is
/// still the one kept with it. Anything else is left alone: the pid of a
/// tunnel long gone may be another program's by now.
#[cfg(not(windows))]
fn end(t: &Tunnel) {
    if ours(t) {
        let _ = Command::new("kill")
            .arg(t.pid.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// A port nobody listens on now. Released before `ssh` takes it, so
/// another program could take it between. `ExitOnForwardFailure` ends the
/// `ssh` then, but only once it has logged in, and until then the other
/// program is what answers on the port: the proof is what keeps the token
/// from it.
fn free_port() -> Result<u16, String> {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
    Ok(l.local_addr().map_err(|e| e.to_string())?.port())
}

/// Whether something accepts connections on `port` here. `ssh -L` listens
/// only once it has logged in, so this is a quick first look; that it is
/// the tunnel, with the real server at its far end, is what the proof
/// says.
fn answers(port: u16) -> bool {
    let at = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&at, Duration::from_millis(300)).is_ok()
}

/// What the far end of a port said when asked to prove it holds the token.
enum Hello {
    /// It holds the token: the remote's Horadric, through our tunnel.
    Proven,
    /// It answered HTTP, but not the proof: something else, a Horadric
    /// from before the proof, or one with another token.
    Wrong,
    /// It did not answer as a server does at all, and why.
    Silent(String),
}

/// Asks whatever answers on `port` to prove it holds `token`, with a
/// nonce nobody has seen before, so an answer overheard earlier is worth
/// nothing. The token itself is not sent.
fn hello(port: u16, token: &str) -> Hello {
    let nonce = match nonce() {
        Ok(n) => n,
        Err(e) => return Hello::Silent(e),
    };
    let head = remote::request_head("GET", port, &remote::hello_path(&nonce), None, None);
    match request(port, head.into_bytes(), SHORT_WAIT, remote::SMALL_CAP) {
        Ok(r) if r.status == 200 && remote::proven(token, &nonce, &r.body) => Hello::Proven,
        Ok(_) => Hello::Wrong,
        Err(e) => Hello::Silent(e),
    }
}

/// 32 hex characters from the system's random generator.
fn nonce() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    #[cfg(windows)]
    horadric_ui::portal_crypto::random(&mut bytes);
    #[cfg(not(windows))]
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| format!("could not read /dev/urandom: {e}"))?;
    Ok(remote::hex(&bytes))
}

/// Starts the tunnel apart from this process and waits until it forwards
/// and its far end answers the proof: the tunnel as it is remembered, and
/// whether the proof held. What `ssh` says on failure goes to `log` and
/// is given back, since the window it would print in does not exist. A
/// far end that says nothing ends the `ssh` again, so a host that serves
/// no screen is left with none running.
fn start_tunnel(
    dest: &str,
    local: u16,
    remote_port: u16,
    token: &str,
    log: &PathBuf,
) -> Result<(Tunnel, bool), String> {
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let err = std::fs::File::create(log).map_err(|e| e.to_string())?;
    let command = || -> Result<Command, String> {
        let mut c = Command::new(ssh_program());
        c.args(remote::tunnel_args(dest, local, remote_port))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(err.try_clone().map_err(|e| e.to_string())?);
        Ok(c)
    };
    let mut child = spawn_apart(command)?;
    let pid = child.id();
    let deadline = Instant::now() + TUNNEL_WAIT;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            let said = std::fs::read_to_string(log).unwrap_or_default();
            let said = remote::brief(&said);
            return Err(if said.is_empty() {
                format!("ssh to {dest} ended ({status}) before the tunnel was up")
            } else {
                format!("ssh to {dest} failed: {said}")
            });
        }
        if answers(local) {
            let proven = match hello(local, token) {
                Hello::Proven => true,
                Hello::Wrong => false,
                Hello::Silent(why) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(why);
                }
            };
            let kept = Tunnel {
                dest: dest.to_string(),
                remote: remote_port,
                port: local,
                pid,
                identity: ssh_identity(pid),
            };
            let_go(child);
            return Ok((kept, proven));
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "the tunnel to {dest} was not up after {} seconds",
                TUNNEL_WAIT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Lets a started tunnel live on without this process holding it. On
/// Windows that is dropping its handle.
#[cfg(windows)]
fn let_go(child: std::process::Child) {
    drop(child);
}

/// Lets a started tunnel live on without this process holding it. A
/// child nobody waits for stays a zombie once it ends, for as long as
/// this process runs, and an MCP server runs for hours: a thread waits.
#[cfg(not(windows))]
fn let_go(mut child: std::process::Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

/// Starts `ssh` so it outlives this process, whatever ends it: a console
/// of its own that is never shown, its own process group, and out of this
/// process's job where the job allows that (a job that does not refuses
/// the whole start, so it is tried again without). Not `DETACHED_PROCESS`:
/// `ssh` without any console is less sure to run than with a hidden one.
#[cfg(windows)]
fn spawn_apart(
    command: impl Fn() -> Result<Command, String>,
) -> Result<std::process::Child, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    keep_std_handles();
    let apart = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
    if let Ok(child) = command()?
        .creation_flags(apart | CREATE_BREAKAWAY_FROM_JOB)
        .spawn()
    {
        return Ok(child);
    }
    command()?
        .creation_flags(apart)
        .spawn()
        .map_err(|e| format!("could not start ssh: {e}"))
}

#[cfg(not(windows))]
fn spawn_apart(
    command: impl Fn() -> Result<Command, String>,
) -> Result<std::process::Child, String> {
    command()?
        .spawn()
        .map_err(|e| format!("could not start ssh: {e}"))
}

/// Makes this process's own stdin, stdout and stderr not inheritable. A
/// child is started with every inheritable handle, and a tunnel living on
/// with the MCP server's pipes would keep them open after it ends, so the
/// agent reading them would never see them close. A child given these
/// handles on purpose still gets them: the standard library duplicates
/// them for it.
#[cfg(windows)]
fn keep_std_handles() {
    use windows::Win32::Foundation::{SetHandleInformation, HANDLE_FLAGS, HANDLE_FLAG_INHERIT};
    use windows::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: only flags on handles this process already holds change.
        unsafe {
            if let Ok(h) = GetStdHandle(which) {
                if !h.is_invalid() {
                    let _ = SetHandleInformation(h, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0));
                }
            }
        }
    }
}

/// Reads a child's pipe to its end on a thread of its own, so a child
/// that is killed for taking too long, or whose own children keep the
/// pipe open, holds nothing up here. Held to 64 KiB: a token is 32
/// characters and a complaint a few lines.
fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(p) = pipe {
            let _ = p.take(64 * 1024).read_to_end(&mut bytes);
        }
        let _ = tx.send(String::from_utf8_lossy(&bytes).into_owned());
    });
    rx
}

/// The remote's token, read over `ssh`. The remote makes a fresh one at
/// each of its starts, so this is read again whenever the one kept stops
/// being taken. The `ssh` is killed when it takes too long: its own
/// timeout covers connecting, not a host that connects and then hangs.
fn read_token(dest: &str, dev: bool) -> Result<String, String> {
    let mut c = Command::new(ssh_program());
    c.args(remote::token_args(dest, dev))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    horadric_hooks::no_window(&mut c);
    let mut child = c.spawn().map_err(|e| format!("could not start ssh: {e}"))?;
    let printed = drain(child.stdout.take());
    let complaint = drain(child.stderr.take());
    let deadline = Instant::now() + TOKEN_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "ssh to {dest} did not answer within {} seconds",
                    TOKEN_WAIT.as_secs()
                ));
            }
        }
    }
    let grace = Duration::from_secs(2);
    let printed = printed.recv_timeout(grace).unwrap_or_default();
    if let Some(t) = remote::parse_token(&printed) {
        return Ok(t);
    }
    // What it printed is not a token, so it is safe to say; ssh's own
    // complaint is on stderr.
    let said = remote::brief(&complaint.recv_timeout(grace).unwrap_or_default());
    let folder = if dev { "Horadric-dev" } else { "Horadric" };
    let mut why = format!(
        "{dest} has no screen token in %APPDATA%\\{folder}\\screen-token: Horadric must run there \
         with the user environment variable HORADRIC_SCREEN_SHARE=1 set when it started"
    );
    if !said.is_empty() {
        why.push_str(&format!(". ssh said: {said}"));
    }
    Err(why)
}

/// What is left of `wait` at `deadline`, or that the remote took too long.
fn left(deadline: Instant, wait: Duration) -> Result<Duration, String> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| {
            format!(
                "the remote did not answer within {} seconds",
                wait.as_secs()
            )
        })
}

/// Sends `bytes`, a whole HTTP/1.0 request, through the tunnel on `port`
/// and reads the answer. `wait` is for all of it, not for each read, so a
/// peer that sends a byte now and then holds nothing up; reading stops at
/// the length the answer said, and at `cap` bytes of body, so one that
/// never closes or never stops sending does neither harm.
fn request(port: u16, bytes: Vec<u8>, wait: Duration, cap: usize) -> Result<Response, String> {
    let deadline = Instant::now() + wait;
    let at = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut s = TcpStream::connect_timeout(&at, wait.min(Duration::from_secs(3)))
        .map_err(|e| format!("the tunnel did not answer: {e}"))?;
    let timed_out = |e: &std::io::Error| {
        matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
    };
    let mut written = 0;
    while written < bytes.len() {
        let _ = s.set_write_timeout(Some(left(deadline, wait)?));
        match s.write(&bytes[written..]) {
            Ok(0) => return Err("could not ask the remote: the tunnel closed".into()),
            Ok(n) => written += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if timed_out(&e) => {}
            Err(e) => return Err(format!("could not ask the remote: {e}")),
        }
    }
    let unread = |e: Unread| match e {
        Unread::NotHttp => "nothing answered on the remote's screen port: Horadric must run \
                            there with HORADRIC_SCREEN_SHARE=1 set when it started"
            .to_string(),
        Unread::CutShort { said, got } => {
            format!("the remote's answer was cut off: {got} of {said} bytes came")
        }
        Unread::TooLarge { cap } => {
            format!("the answer was more than {cap} bytes, which the remote's Horadric never sends")
        }
    };
    let mut got = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    while !remote::whole_answer(&got, cap).map_err(unread)? {
        let _ = s.set_read_timeout(Some(left(deadline, wait)?));
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if timed_out(&e) => {}
            Err(e) => return Err(format!("the remote's answer was cut off: {e}")),
        }
    }
    remote::parse_response(&got).map_err(unread)
}

/// Outside a Horadric terminal there is no browser pane to open it in.
#[cfg(windows)]
fn open_outside(url: &str) -> Result<(), String> {
    use windows::core::{w, HSTRING, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    // SAFETY: every string outlives the call.
    let done = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(url),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // Above 32 is success, as ShellExecute has always said it.
    if done.0 as usize > 32 {
        Ok(())
    } else {
        Err("could not open the default browser".into())
    }
}

#[cfg(not(windows))]
fn open_outside(_url: &str) -> Result<(), String> {
    Err("run it in a Horadric terminal, which has a browser pane to open it in".into())
}

/// Whether `name` is one of the `remote_` tools.
pub(crate) fn is_tool(name: &str) -> bool {
    matches!(
        name,
        "remote_open"
            | "remote_screenshot"
            | "remote_click"
            | "remote_type"
            | "remote_press"
            | "remote_scroll"
    )
}

/// The `remote_` tools, offered with the browser's on Windows, where the
/// machines they watch are.
pub fn tools() -> Vec<Value> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let ssh = || {
        json!({
            "type": "string",
            "description": "The ssh destination, as you would give it to ssh: an alias from ~/.ssh/config or user@host"
        })
    };
    vec![
        crate::mcp::tool(
            "remote_open",
            &format!(
                "Show a remote Windows machine's desktop, live, in a tab of this project's \
                 browser pane, where the user sees it too and can take over its mouse and \
                 keyboard. Use it when you work on a Windows machine over SSH (a VM, say) that \
                 runs Horadric with HORADRIC_SCREEN_SHARE=1: open it as you start, so the user \
                 follows what happens there and sees a dialog that waits for someone, rather \
                 than you working blind. {ASKED} It opens an SSH tunnel of its own (key login, \
                 no prompts). The page's address carries a secret token, which is left out of \
                 this tool's reply, and the page drops it from its address once loaded."
            ),
            json!({ "ssh": ssh() }),
            &["ssh"],
        ),
        crate::mcp::tool(
            "remote_screenshot",
            &format!(
                "A picture of a remote Windows machine's whole desktop (every monitor), the \
                 same picture remote_open shows the user. Use it to look at a machine you work \
                 on over SSH that runs Horadric with HORADRIC_SCREEN_SHARE=1: after a step, to \
                 see what came up, before you say something worked there. {ASKED} Its text \
                 line gives the screen in physical pixels and the factor to multiply a spot in \
                 the picture by for the x and y remote_click and remote_scroll take, since the \
                 picture may be scaled down to width. While the desktop there is locked or \
                 shows a UAC prompt there is no picture, and the error says so."
            ),
            json!({
                "ssh": ssh(),
                "width": {
                    "type": "integer",
                    "description": "The widest the picture may be, in pixels (default 1600)"
                },
            }),
            &["ssh"],
        ),
        crate::mcp::tool(
            "remote_click",
            &format!(
                "Click on a remote Windows machine's desktop, as remote_screenshot shows it. \
                 {ACT} x and y are screen pixels from the top left of the whole desktop: a \
                 spot in the screenshot multiplied by the factor its text line gives (the \
                 product need not be whole, it is rounded)."
            ),
            json!({
                "ssh": ssh(),
                "x": { "type": "number", "description": "Screen pixels from the left" },
                "y": { "type": "number", "description": "Screen pixels from the top" },
                "button": {
                    "type": "string",
                    "enum": ["left", "right", "middle"],
                    "description": "Which button (default left)"
                },
                "double": { "type": "boolean", "description": "Click twice, to open (default false)" },
            }),
            &["ssh", "x", "y"],
        ),
        crate::mcp::tool(
            "remote_type",
            &format!(
                "Type text on a remote Windows machine, into whatever has the keyboard focus \
                 there: click the field first. Any characters, typed as Unicode, so the \
                 keyboard layout does not change them. A line break in the text presses Enter \
                 and a tab presses Tab; for any other key or a shortcut use remote_press. {ACT}"
            ),
            json!({
                "ssh": ssh(),
                "text": { "type": "string", "description": "What to type" },
            }),
            &["ssh", "text"],
        ),
        crate::mcp::tool(
            "remote_press",
            &format!(
                "Press a key or a shortcut on a remote Windows machine: a key such as Enter, \
                 Tab, Escape, Backspace, Delete, Up, PageDown, F5, PrintScreen, a letter, a \
                 digit or one of - = , . / (Plus and Minus by name too), or a chord such as \
                 Ctrl+S, Alt+F4, Ctrl+Shift+Esc or Win+R (the modifiers Ctrl, Shift, Alt and \
                 Win first, joined by +). {ACT}"
            ),
            json!({
                "ssh": ssh(),
                "key": { "type": "string", "description": "The key or chord, e.g. Enter or Ctrl+S" },
            }),
            &["ssh", "key"],
        ),
        crate::mcp::tool(
            "remote_scroll",
            &format!(
                "Turn the mouse wheel over a spot on a remote Windows machine's desktop. \
                 {ACT} x and y are screen pixels, as for remote_click."
            ),
            json!({
                "ssh": ssh(),
                "x": { "type": "number", "description": "Screen pixels from the left" },
                "y": { "type": "number", "description": "Screen pixels from the top" },
                "delta": {
                    "type": "integer",
                    "description": "Wheel notches, a few lines each: positive scrolls up, negative down, 100 at most either way"
                },
            }),
            &["ssh", "x", "y", "delta"],
        ),
    ]
}

/// What every tool's description says of the question the human gets.
const ASKED: &str = "The user is asked to allow each destination once per project before \
     anything is opened or sent; when they say no, or this session was not started by \
     Horadric, the tool answers an error and does nothing.";

/// What every input tool's description says: how to work with it, and
/// what it can not reach.
const ACT: &str = "For a machine you work on over SSH that runs Horadric with \
     HORADRIC_SCREEN_SHARE=1. The user is asked to allow each destination once per project \
     before anything is sent. Look with remote_screenshot, act, then look again to see what \
     it did: nothing comes back but a line. The user sees the same desktop live in \
     remote_open's tab while you work. When the input is refused (409) the error names the \
     real reason: the desktop there is locked or shows a UAC prompt, or the window runs as \
     administrator and the Horadric there does not.";

/// Answers a `remote_` tool, None for any other. `ask` is one call to the
/// app that owns this session, giving back what it answered: it asks the
/// human whether the destination may be watched and driven, and opens a
/// URL in the browser pane.
pub fn call(
    name: &str,
    args: &Value,
    mut ask: impl FnMut(Value) -> Result<Value, String>,
) -> Option<Result<Vec<Value>, String>> {
    is_tool(name).then(|| answer(name, args, &mut ask))
}

/// The app's leave to watch and drive `dest`, asked before a tool does
/// anything: before any `ssh` starts and before any file is touched. The
/// app asks the human once per project and destination and remembers the
/// answer, so asking it every time costs the human nothing. Whatever it
/// says against it is the tool's error, word for word.
fn allowed(
    dest: &str,
    ask: &mut dyn FnMut(Value) -> Result<Value, String>,
) -> Result<(), String> {
    let answer = ask(json!({ "op": "remote_allow", "ssh": dest }))?;
    if answer.get("allowed") == Some(&Value::Bool(true)) {
        Ok(())
    } else {
        Err(format!(
            "Horadric did not allow watching and driving {dest}"
        ))
    }
}

fn answer(
    name: &str,
    args: &Value,
    ask: &mut dyn FnMut(Value) -> Result<Value, String>,
) -> Result<Vec<Value>, String> {
    let dest = match args.get("ssh").and_then(Value::as_str).map(str::trim) {
        Some(d) if !d.is_empty() => d.to_string(),
        _ => return Err("say which machine: ssh, a destination ssh takes".into()),
    };
    // What is wrong with the call is said before the human is asked
    // about it: a question for a call that can not run helps nobody.
    if let Some(why) = remote::refusal(&dest) {
        return Err(why);
    }
    let input = input_events(name, args).transpose()?;
    allowed(&dest, ask)?;
    let dev = dev_env();
    if let Some((bodies, done)) = input {
        return send_input(&dest, dev, &bodies).map(|()| vec![crate::mcp::text(done)]);
    }
    if name == "remote_open" {
        let show = |url: &str| {
            ask(json!({ "op": "open", "url": url })).map(|a| crate::mcp::describe(&a))
        };
        return open(&dest, dev, show).map(|o| {
            vec![crate::mcp::text(format!(
                "Opened the desktop of {dest} ({}). {}",
                o.via, o.answer
            ))]
        });
    }
    let width = args
        .get("width")
        .and_then(remote::whole)
        .map_or(remote::FRAME_WIDTH, |w| w.clamp(64, 8192) as u32);
    screenshot(&dest, dev, width).map(|(jpeg, line)| {
        vec![
            json!({
                "type": "image",
                "data": horadric_core::release::base64_encode(&jpeg),
                "mimeType": "image/jpeg",
            }),
            crate::mcp::text(line),
        ]
    })
}

/// The bodies an input tool posts, each one sequence for the remote to
/// run whole, and the line it answers with once they are taken; None for
/// a tool that posts none. Everything is checked here, before the human
/// is asked and before a tunnel is started for it.
fn input_events(name: &str, args: &Value) -> Option<Result<(Vec<String>, String), String>> {
    let int = |k: &str| -> Result<i32, String> {
        args.get(k)
            .and_then(remote::whole)
            .ok_or_else(|| format!("say {k}, a number"))
    };
    let s = |k: &str| args.get(k).and_then(Value::as_str);
    Some(match name {
        "remote_click" => (|| {
            let (x, y) = (int("x")?, int("y")?);
            let double = args.get("double").and_then(Value::as_bool).unwrap_or(false);
            let events = remote::click_events(x, y, s("button"), double)?;
            let what = if double { "double clicked" } else { "clicked" };
            let button = match s("button") {
                Some(b @ ("right" | "middle")) => format!(" with the {b} button"),
                _ => String::new(),
            };
            Ok((
                vec![remote::batch_body(&events)?],
                format!("{what} at {x},{y}{button}"),
            ))
        })(),
        "remote_type" => s("text")
            .ok_or_else(|| "say what to type: text".to_string())
            .and_then(|text| {
                let bodies = remote::text_events(text)?
                    .into_iter()
                    .map(|piece| remote::batch_body(&[piece]))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((bodies, format!("typed {} characters", text.chars().count())))
            }),
        "remote_press" => s("key")
            .ok_or_else(|| "say which key: key, e.g. Enter or Ctrl+S".to_string())
            .and_then(|key| {
                let vks = remote::parse_chord(key)?;
                Ok((
                    vec![remote::batch_body(&remote::press_events(&vks))?],
                    format!("pressed {}", key.trim()),
                ))
            }),
        "remote_scroll" => (|| {
            let (x, y, delta) = (int("x")?, int("y")?, int("delta")?);
            let (event, sent) = remote::wheel_event(x, y, delta)?;
            let way = if sent > 0 { "up" } else { "down" };
            let held = if sent == delta {
                String::new()
            } else {
                format!(
                    " ({} were asked, {} is the most at once)",
                    delta.unsigned_abs(),
                    remote::WHEEL_MOST
                )
            };
            Ok((
                vec![remote::batch_body(&[event])?],
                format!(
                    "scrolled {way} {} notches at {x},{y}{held}",
                    sent.unsigned_abs()
                ),
            ))
        })(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An app that must not be asked: the call is to fail before it.
    fn unasked(body: Value) -> Result<Value, String> {
        panic!("nothing may be asked or opened: {body}")
    }

    #[test]
    fn only_remote_tools_are_answered() {
        assert!(call("browser_open", &json!({}), unasked).is_none());
        assert!(call("remote_nope", &json!({ "ssh": "vm" }), unasked).is_none());
        assert!(is_tool("remote_open") && !is_tool("browser_open"));
    }

    #[test]
    fn a_destination_is_needed_and_checked_before_anyone_is_asked() {
        let e = call("remote_open", &json!({}), unasked)
            .unwrap()
            .unwrap_err();
        assert!(e.contains("ssh"), "{e}");
        for bad in ["-oProxyCommand=calc", "vm;calc", "vm x"] {
            let e = call("remote_screenshot", &json!({ "ssh": bad }), unasked)
                .unwrap()
                .unwrap_err();
            assert!(e.contains("not an ssh destination"), "{e}");
        }
    }

    #[test]
    fn the_human_is_asked_before_anything_starts() {
        // Each of these would start ssh if the refusal did not stop it:
        // an answer here is the app's words, and nothing else happened.
        let calls = [
            ("remote_open", json!({ "ssh": "vm" })),
            ("remote_screenshot", json!({ "ssh": "vm" })),
            ("remote_click", json!({ "ssh": "vm", "x": 1, "y": 2 })),
            ("remote_type", json!({ "ssh": "vm", "text": "x" })),
            ("remote_press", json!({ "ssh": "vm", "key": "Enter" })),
            ("remote_scroll", json!({ "ssh": "vm", "x": 1, "y": 2, "delta": 1 })),
        ];
        for (name, args) in calls {
            let mut asked = Vec::new();
            let e = call(name, &args, |body| {
                asked.push(body);
                Err("the user said no to vm".to_string())
            })
            .unwrap()
            .unwrap_err();
            assert_eq!(e, "the user said no to vm", "{name}");
            assert_eq!(asked, [json!({ "op": "remote_allow", "ssh": "vm" })], "{name}");
        }
    }

    #[test]
    fn only_a_plain_yes_allows() {
        for answer in [
            json!({}),
            json!({ "allowed": false }),
            json!({ "allowed": "true" }),
            json!({ "allowed": 1 }),
            json!(null),
        ] {
            let e = call("remote_press", &json!({ "ssh": "vm", "key": "Enter" }), |_| {
                Ok(answer.clone())
            })
            .unwrap()
            .unwrap_err();
            assert!(e.contains("did not allow") && e.contains("vm"), "{e}");
        }
    }

    #[test]
    fn the_tools_say_when_to_use_them() {
        for t in tools() {
            let said = t["description"].as_str().unwrap();
            assert!(said.contains("HORADRIC_SCREEN_SHARE=1"), "{said}");
            assert!(
                said.contains("asked to allow each destination once per project"),
                "{said}"
            );
            assert_eq!(t["inputSchema"]["required"][0], "ssh");
        }
    }

    #[test]
    fn input_is_checked_before_anyone_is_asked() {
        let e = |name: &str, args: Value| {
            call(name, &args, unasked)
                .expect("a remote tool")
                .unwrap_err()
        };
        assert!(e("remote_click", json!({ "ssh": "vm", "x": 1 })).contains("say y"));
        assert!(e("remote_click", json!({ "ssh": "vm", "x": "1", "y": 2 })).contains("say x"));
        assert!(e(
            "remote_click",
            json!({ "ssh": "vm", "x": 1, "y": 2, "button": "back" })
        )
        .contains("button"));
        assert!(e("remote_type", json!({ "ssh": "vm", "text": "" })).contains("text"));
        assert!(e("remote_press", json!({ "ssh": "vm", "key": "Ctrl+Nope" })).contains("Nope"));
        assert!(e("remote_press", json!({ "ssh": "vm" })).contains("key"));
        assert!(e(
            "remote_scroll",
            json!({ "ssh": "vm", "x": 1, "y": 2, "delta": 0 })
        )
        .contains("notches"));
        assert!(e("remote_press", json!({ "key": "Enter" })).contains("ssh"));
    }

    #[test]
    fn input_tools_say_what_they_did() {
        let done = |name: &str, args: Value| input_events(name, &args).unwrap().unwrap().1;
        assert_eq!(
            done("remote_click", json!({ "x": 812, "y": 440 })),
            "clicked at 812,440"
        );
        assert_eq!(
            done("remote_click", json!({ "x": 1299.2, "y": 703.6 })),
            "clicked at 1299,704"
        );
        assert_eq!(
            done(
                "remote_click",
                json!({ "x": 1, "y": 2, "button": "right", "double": true })
            ),
            "double clicked at 1,2 with the right button"
        );
        assert_eq!(
            done("remote_type", json!({ "text": "æøå" })),
            "typed 3 characters"
        );
        assert_eq!(
            done("remote_press", json!({ "key": " Ctrl+S " })),
            "pressed Ctrl+S"
        );
        assert_eq!(
            done("remote_scroll", json!({ "x": 5, "y": 6, "delta": -3 })),
            "scrolled down 3 notches at 5,6"
        );
        assert_eq!(
            done("remote_scroll", json!({ "x": 5, "y": 6, "delta": 250 })),
            "scrolled up 100 notches at 5,6 (250 were asked, 100 is the most at once)"
        );
        assert!(input_events("remote_open", &json!({})).is_none());
    }

    #[test]
    fn each_act_is_one_request() {
        let bodies = |name: &str, args: Value| -> Vec<Value> {
            let (bodies, _) = input_events(name, &args).unwrap().unwrap();
            bodies
                .iter()
                .map(|b| serde_json::from_str(b).unwrap())
                .collect()
        };
        let click = bodies("remote_click", json!({ "x": 1, "y": 2, "double": true }));
        assert_eq!(click.len(), 1);
        assert_eq!(click[0].as_array().unwrap().len(), 5);
        let chord = bodies("remote_press", json!({ "key": "Ctrl+Shift+Esc" }));
        assert_eq!(chord.len(), 1);
        assert_eq!(chord[0].as_array().unwrap().len(), 6);
        assert_eq!(
            bodies("remote_type", json!({ "text": "a\tb\nc" })),
            [json!([{ "kind": "text", "text": "a\tb\nc" }])]
        );
        let long = bodies("remote_type", json!({ "text": "x".repeat(9000) }));
        assert_eq!(long.len(), 3);
        assert!(long.iter().all(|b| b.as_array().unwrap().len() == 1));
        assert_eq!(
            bodies("remote_scroll", json!({ "x": 5, "y": 6, "delta": 250 })),
            [json!([{ "kind": "wheel", "x": 5, "y": 6, "delta": 12000 }])]
        );
    }

    #[test]
    fn a_token_is_kept_per_destination_until_forgotten() {
        let token = "0123456789abcdef0123456789abcdef";
        assert_eq!(kept_token("kept-test-vm", false), None);
        keep_token("kept-test-vm", false, Some(token));
        assert_eq!(kept_token("kept-test-vm", false).as_deref(), Some(token));
        assert_eq!(kept_token("kept-test-vm", true), None);
        assert_eq!(kept_token("kept-test-other", false), None);
        keep_token("kept-test-vm", false, None);
        assert_eq!(kept_token("kept-test-vm", false), None);
    }

    #[test]
    fn a_lock_is_removed_only_by_the_one_who_holds_it() {
        let dir = std::env::temp_dir().join(format!("horadric-remote-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let tunnels = dir.join(remote::TUNNELS_FILE);
        let lock_file = tunnels.with_extension("lock");

        let held = Lock::take(&tunnels).unwrap();
        assert_eq!(std::fs::read_to_string(&lock_file).unwrap(), held.mark);
        drop(held);
        assert!(!lock_file.exists(), "our own lock goes with us");

        // Taken over by another while we still thought it ours.
        let held = Lock::take(&tunnels).unwrap();
        std::fs::write(&lock_file, "4242 0").unwrap();
        drop(held);
        assert_eq!(std::fs::read_to_string(&lock_file).unwrap(), "4242 0");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_peer_that_never_closes_or_never_stops_holds_nothing_up() {
        use std::net::TcpListener;
        let serve = |answer: Vec<u8>, linger: Duration| {
            let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = l.local_addr().unwrap().port();
            std::thread::spawn(move || {
                if let Ok((mut s, _)) = l.accept() {
                    let mut buf = [0u8; 1024];
                    let _ = s.read(&mut buf);
                    let _ = s.write_all(&answer);
                    std::thread::sleep(linger);
                }
            });
            port
        };
        let head = |port: u16| remote::request_head("GET", port, "/x", None, None).into_bytes();
        let long = Duration::from_secs(5);

        // The length said is where reading stops, though the peer stays.
        let port = serve(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(), long);
        let began = Instant::now();
        let r = request(port, head(port), Duration::from_secs(4), 64).unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, b"ok".as_slice()));
        assert!(began.elapsed() < Duration::from_secs(3));

        // An answer that never ends is given the whole wait and no more.
        let port = serve(b"HTTP/1.0 200 OK\r\n\r\nsome".to_vec(), long);
        let began = Instant::now();
        let e = request(port, head(port), Duration::from_secs(1), 64).unwrap_err();
        assert!(e.contains("did not answer within"), "{e}");
        assert!(began.elapsed() < Duration::from_secs(3));

        // More than the cap is refused, said or sent.
        let port = serve(
            b"HTTP/1.0 200 OK\r\nContent-Length: 65\r\n\r\n".to_vec(),
            long,
        );
        let e = request(port, head(port), Duration::from_secs(4), 64).unwrap_err();
        assert!(e.contains("more than 64 bytes"), "{e}");
        let mut flood = b"HTTP/1.0 200 OK\r\n\r\n".to_vec();
        flood.extend([b'x'; 4096]);
        let port = serve(flood, long);
        let e = request(port, head(port), Duration::from_secs(4), 64).unwrap_err();
        assert!(e.contains("more than 64 bytes"), "{e}");
    }
}
