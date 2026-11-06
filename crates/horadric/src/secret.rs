//! `horadric secret`: use a kept secret without reading it, keep one, and
//! list the names a session may use. The values never come here: `run`
//! sends the command line to the app, which starts the program with them
//! and streams its masked output back (see `listener::SecretStream`).

use std::io::{BufRead, Write};
use std::time::Duration;

use horadric_hooks::listener::Frame;
use horadric_hooks::{
    client, COMMAND_HEADER, OWNER_ENV, SECRET_KEY_ENV, SECRET_NAMES_PATH, SECRET_RUN_PATH,
    SECRET_SET_PATH, SESSION_ENV, SESSION_HEADER, STATE_HEADER,
};
use serde_json::{json, Value};

const USAGE: &str = "\
Usage:
  horadric secret run [--stdin NAME] NAME[,NAME...] -- PROGRAM ARGS...
                               Run PROGRAM with those secrets in its environment, its
                               output masked; --stdin also writes NAME's value to its input
  horadric secret set NAME [--everywhere | --identity SLUG]
                               Keep a secret for this folder's project, every project, or
                               an identity; the value is read without echo
  horadric secret list           The names this session may use";

/// The human confirms a `set` on the tile; the listener waits ten minutes.
const SET_WAIT: Duration = Duration::from_secs(610);

const NAMES_WAIT: Duration = Duration::from_secs(12);

pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("run") => secret_run(&args[1..]),
        Some("set") => set(&args[1..]),
        Some("list") => list(),
        _ => Err(USAGE.into()),
    }
}

/// What `secret run` was asked, before it goes to the app.
#[derive(Debug, PartialEq, Eq)]
struct RunArgs {
    names: Vec<String>,
    stdin: Vec<String>,
    program: String,
    args: Vec<String>,
}

fn parse_run(args: &[String]) -> Result<RunArgs, String> {
    let split = args
        .iter()
        .position(|a| a == "--")
        .ok_or("put `--` before the program: horadric secret run NAME -- PROGRAM ARGS...")?;
    let (ours, theirs) = (&args[..split], &args[split + 1..]);
    let program = theirs.first().ok_or("no program after `--`")?.clone();
    let mut names = Vec::new();
    let mut stdin = Vec::new();
    let mut it = ours.iter();
    while let Some(a) = it.next() {
        if a == "--stdin" {
            stdin.push(it.next().ok_or("--stdin needs a NAME")?.clone());
        } else if a.starts_with("--") {
            return Err(format!("unknown flag `{a}`\n\n{USAGE}"));
        } else {
            names.extend(a.split(',').map(str::trim).filter(|n| !n.is_empty()).map(String::from));
        }
    }
    for n in &stdin {
        if !names.contains(n) {
            names.push(n.clone());
        }
    }
    if names.is_empty() {
        return Err("name at least one secret".into());
    }
    Ok(RunArgs {
        names,
        stdin,
        program,
        args: theirs[1..].to_vec(),
    })
}

/// The app's port and the headers a command to it carries, as `quest` does:
/// a session's owner is the one Horadric it may mean.
fn app(command: &'static str) -> (u16, Vec<(&'static str, String)>) {
    let owner = std::env::var(OWNER_ENV).ok().and_then(|p| p.parse().ok());
    let state = match owner {
        Some(_) => String::new(),
        None => horadric_hooks::state_header(),
    };
    let mut headers = vec![(COMMAND_HEADER, command.to_string()), (STATE_HEADER, state)];
    if let Some(s) = session() {
        headers.push((SESSION_HEADER, s));
    }
    (owner.unwrap_or_else(horadric_hooks::port), headers)
}

fn session() -> Option<String> {
    std::env::var(SESSION_ENV).ok().filter(|s| !s.is_empty())
}

fn key() -> Result<String, String> {
    match (session(), std::env::var(SECRET_KEY_ENV).ok().filter(|k| !k.is_empty())) {
        (None, _) => Err("this terminal was not started by Horadric, so it has no secrets".into()),
        (Some(_), None) => Err(format!(
            "this session has no {SECRET_KEY_ENV}: it was started before Horadric kept secrets. \
             Start a new session."
        )),
        (Some(_), Some(k)) => Ok(k),
    }
}

fn borrowed<'a>(headers: &'a [(&'static str, String)]) -> Vec<(&'a str, &'a str)> {
    headers.iter().map(|(n, v)| (*n, v.as_str())).collect()
}

/// Why a reply that is not 200 was refused, from its status and body.
fn refused(status: u16, body: &str) -> String {
    if status == client::REFUSED {
        return client::reason(body);
    }
    let said = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("error")?.as_str().map(str::to_string));
    match (status, said) {
        (_, Some(e)) => e,
        (403, None) => "Horadric refused the call".into(),
        (503, None) => "Horadric is not taking commands".into(),
        (504, None) => "Horadric did not answer in time".into(),
        (s, None) => format!("Horadric refused the call ({s})"),
    }
}

fn not_listening(e: std::io::Error) -> String {
    if client::sent(&e) {
        format!("the answer was lost ({e})")
    } else {
        format!("Horadric is not running, or not on this port: {e}")
    }
}

fn secret_run(args: &[String]) -> Result<(), String> {
    let asked = parse_run(args)?;
    let key = key()?;
    let cwd = std::env::current_dir()
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .into_owned();
    let body = json!({
        "key": key,
        "names": asked.names,
        "stdin": asked.stdin,
        "program": asked.program,
        "args": asked.args,
        "cwd": cwd,
    });
    let (port, headers) = app("secret-run");
    let (status, mut reader) =
        client::open(port, SECRET_RUN_PATH, &borrowed(&headers), &body.to_string())
            .map_err(not_listening)?;
    if status != 200 {
        let mut rest = String::new();
        let _ = std::io::Read::read_to_string(&mut reader, &mut rest);
        return Err(refused(status, &rest));
    }
    let code = relay(&mut reader, &mut std::io::stdout(), &mut std::io::stderr())?;
    // The program's code is the answer, so it skips `main`'s mapping.
    std::process::exit(code)
}

/// Copies a run's frames to `out` and `err` until its last line, and gives
/// the program's exit code. A stream that ends without one is an error.
fn relay(
    reader: &mut impl BufRead,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<i32, String> {
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).map_err(|e| {
            format!("lost Horadric while the program ran ({e}); it may still be running")
        })?;
        if read == 0 {
            return Err(
                "Horadric closed the run without an exit code; the program may not have finished"
                    .into(),
            );
        }
        match Frame::parse(line.trim_end_matches(['\r', '\n'])) {
            Some(Frame::Out(b)) => {
                let _ = out.write_all(&b).and_then(|_| out.flush());
            }
            Some(Frame::Err(b)) => {
                let _ = err.write_all(&b).and_then(|_| err.flush());
            }
            Some(Frame::Exit(code)) => return Ok(code),
            Some(Frame::Fail(why)) => return Err(why),
            None => {}
        }
    }
}

fn set(args: &[String]) -> Result<(), String> {
    let mut name = None;
    let mut owner = "project".to_string();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--everywhere" => owner = "everywhere".into(),
            "--identity" => {
                let slug = it.next().ok_or("--identity needs a SLUG")?;
                owner = format!("identity:{slug}");
            }
            f if f.starts_with("--") => return Err(format!("unknown flag `{f}`\n\n{USAGE}")),
            n if name.is_none() => name = Some(n.to_string()),
            _ => return Err(USAGE.into()),
        }
    }
    let name = name.ok_or(USAGE)?;
    let value = read_value(&name)?;
    if value.is_empty() {
        return Err("no value given, nothing kept".into());
    }
    let cwd = std::env::current_dir()
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .into_owned();
    // A session's terminal proves it is that session's, so the human is
    // told which session asks; any other terminal asks as nobody's.
    let key = std::env::var(SECRET_KEY_ENV).unwrap_or_default();
    let body = json!({ "cwd": cwd, "name": name, "value": value, "owner": owner, "key": key });
    let (port, headers) = app("secret-set");
    eprintln!("Confirm it in Horadric.");
    let (status, reply) = client::ask(
        port,
        SECRET_SET_PATH,
        &borrowed(&headers),
        &body.to_string(),
        SET_WAIT,
    )
    .map_err(not_listening)?;
    if status != 200 {
        return Err(refused(status, &reply));
    }
    let answer: Value = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
    if let Some(e) = answer.get("error").and_then(Value::as_str) {
        return Err(e.to_string());
    }
    match answer.get("text").and_then(Value::as_str) {
        Some(t) => println!("{t}"),
        None => println!("{name} kept."),
    }
    Ok(())
}

fn list() -> Result<(), String> {
    let key = key()?;
    let (port, headers) = app("secret-names");
    let (status, reply) = client::ask(
        port,
        SECRET_NAMES_PATH,
        &borrowed(&headers),
        &json!({ "key": key }).to_string(),
        NAMES_WAIT,
    )
    .map_err(not_listening)?;
    if status != 200 {
        return Err(refused(status, &reply));
    }
    let answer: Value = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
    if let Some(e) = answer.get("error").and_then(Value::as_str) {
        return Err(e.to_string());
    }
    let names = answer.get("names").and_then(Value::as_array).cloned().unwrap_or_default();
    if names.is_empty() {
        eprintln!("No secrets kept for this project.");
    }
    for n in names.iter().filter_map(Value::as_str) {
        println!("{n}");
    }
    Ok(())
}

/// The value, from the console with echo off, or one line of piped input.
fn read_value(name: &str) -> Result<String, String> {
    let stdin = std::io::stdin();
    let console = std::io::IsTerminal::is_terminal(&stdin);
    if console {
        eprint!("Value for {name} (not shown): ");
        let _ = std::io::stderr().flush();
    }
    let echo = console.then(Echo::off);
    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line);
    drop(echo);
    if console {
        eprintln!();
    }
    read.map_err(|e| e.to_string())?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// Echo turned off on the console until dropped, so a value typed or
/// pasted never shows on screen or in the terminal's scrollback.
struct Echo {
    #[cfg(windows)]
    was: Option<windows::Win32::System::Console::CONSOLE_MODE>,
    #[cfg(unix)]
    was: Option<libc::termios>,
}

impl Echo {
    #[cfg(windows)]
    fn off() -> Echo {
        use windows::Win32::System::Console::{
            GetConsoleMode, GetStdHandle, SetConsoleMode, CONSOLE_MODE, ENABLE_ECHO_INPUT,
            STD_INPUT_HANDLE,
        };
        // SAFETY: the standard input handle is the process's own, and the
        // mode is read into a local before it is changed.
        let was = unsafe {
            let Ok(h) = GetStdHandle(STD_INPUT_HANDLE) else {
                return Echo { was: None };
            };
            let mut mode = CONSOLE_MODE(0);
            if GetConsoleMode(h, &mut mode).is_err() {
                return Echo { was: None };
            }
            SetConsoleMode(h, mode & !ENABLE_ECHO_INPUT).ok().map(|_| mode)
        };
        Echo { was }
    }

    #[cfg(unix)]
    fn off() -> Echo {
        // SAFETY: termios is plain data, filled by tcgetattr before use.
        let was = unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return Echo { was: None };
            }
            let mut quiet = t;
            quiet.c_lflag &= !libc::ECHO;
            (libc::tcsetattr(0, libc::TCSANOW, &quiet) == 0).then_some(t)
        };
        Echo { was }
    }
}

impl Drop for Echo {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(mode) = self.was {
            use windows::Win32::System::Console::{GetStdHandle, SetConsoleMode, STD_INPUT_HANDLE};
            // SAFETY: puts back the mode read in `off`.
            unsafe {
                if let Ok(h) = GetStdHandle(STD_INPUT_HANDLE) {
                    let _ = SetConsoleMode(h, mode);
                }
            }
        }
        #[cfg(unix)]
        if let Some(t) = self.was {
            // SAFETY: puts back the settings read in `off`.
            unsafe {
                libc::tcsetattr(0, libc::TCSANOW, &t);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn reads_names_stdin_and_the_program() {
        let got = parse_run(&words("--stdin PASS A,B -- gh pr create --fill")).unwrap();
        assert_eq!(got.names, ["A", "B", "PASS"]);
        assert_eq!(got.stdin, ["PASS"]);
        assert_eq!(got.program, "gh");
        assert_eq!(got.args, words("pr create --fill"));
        assert!(parse_run(&words("A gh")).is_err());
        assert!(parse_run(&words("-- gh")).is_err());
        assert!(parse_run(&words("A --")).is_err());
    }

    #[test]
    fn relays_frames_and_the_exit_code() {
        let e = horadric_core::release::base64_encode;
        let wire = format!("o {}\ne {}\nq ignored\nx 3\n", e(b"hi\n"), e(b"oops"));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = relay(&mut wire.as_bytes(), &mut out, &mut err).unwrap();
        assert_eq!((code, &out[..], &err[..]), (3, &b"hi\n"[..], &b"oops"[..]));
        let failed = relay(&mut "! no key\n".as_bytes(), &mut out, &mut err);
        assert_eq!(failed, Err("no key".into()));
        assert!(relay(&mut "o aGk=\n".as_bytes(), &mut out, &mut err).is_err());
    }
}
