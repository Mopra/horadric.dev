//! `horadric headless-debug`, hidden: starts a headless `claude.exe`, sends
//! one prompt, prints every stdout line, and ends after the turn's result.
//! For checking the stream-json protocol against the real CLI and for
//! recording the Folio's fixtures. See docs/plan/headless.md.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use horadric_pty::headless::{Headless, Options};

const USAGE: &str = "usage: horadric headless-debug [--cwd DIR] [--resume ID] [--deny] \
[--init] [--leave-asks] [--arg FLAG]... [--interrupt-after MS] [--then PROMPT] [--out FILE] [--timeout SECS] PROMPT";

#[derive(Debug, Default, PartialEq)]
struct Args {
    cwd: Option<PathBuf>,
    resume: Option<String>,
    deny: bool,
    /// Asks go unanswered, so an interrupt can come while one waits.
    leave_asks: bool,
    init: bool,
    interrupt_after: Option<u64>,
    then: Option<String>,
    out: Option<PathBuf>,
    timeout: Option<u64>,
    /// Passed on to claude.exe, such as `--permission-mode=default`.
    extra: Vec<String>,
    prompt: String,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut it = args.iter();
    let mut prompt = Vec::new();
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a.as_str() {
            "--cwd" => out.cwd = Some(value()?.into()),
            "--resume" => out.resume = Some(value()?),
            "--deny" => out.deny = true,
            "--init" => out.init = true,
            "--leave-asks" => out.leave_asks = true,
            "--interrupt-after" => {
                out.interrupt_after = Some(value()?.parse().map_err(|_| "bad milliseconds")?)
            }
            "--arg" => out.extra.push(value()?),
            "--then" => out.then = Some(value()?),
            "--out" => out.out = Some(value()?.into()),
            "--timeout" => out.timeout = Some(value()?.parse().map_err(|_| "bad seconds")?),
            _ => prompt.push(a.clone()),
        }
    }
    out.prompt = prompt.join(" ");
    if out.prompt.is_empty() {
        return Err(USAGE.into());
    }
    Ok(out)
}

pub fn run(args: &[String]) -> Result<(), String> {
    let args = parse(args)?;
    let cwd = match &args.cwd {
        Some(c) => c.clone(),
        None => std::env::current_dir().map_err(|e| e.to_string())?,
    };
    let mut out = match &args.out {
        Some(p) => Some(std::fs::File::create(p).map_err(|e| e.to_string())?),
        None => None,
    };

    // Not any tile's session: the hooks should not post under the tag of
    // the terminal this was typed in.
    let options = Options {
        env_remove: vec![
            horadric_hooks::SESSION_ENV.into(),
            horadric_hooks::OWNER_ENV.into(),
        ],
        args: args.extra.clone(),
        ..Options::default()
    };
    let (tx, rx) = mpsc::channel::<Option<String>>();
    let session = Headless::start_with(&cwd, args.resume.as_deref(), &options, move |line| {
        let _ = tx.send(line);
    })
    .map_err(|e| format!("could not start claude.exe: {e}"))?;
    eprintln!("headless-debug: claude.exe pid {}", session.pid());

    let send = |line: &str| {
        eprintln!(">> {line}");
        session.send_line(line).map_err(|e| e.to_string())
    };
    if args.init {
        send(
            r#"{"type":"control_request","request_id":"init_1","request":{"subtype":"initialize"}}"#,
        )?;
    }
    session
        .send_prompt(&args.prompt)
        .map_err(|e| e.to_string())?;
    eprintln!(">> prompt: {}", args.prompt);

    let started = Instant::now();
    let deadline = started + Duration::from_secs(args.timeout.unwrap_or(180));
    let mut interrupted = false;
    let mut then = args.then.clone();
    let stdin = std::io::stdin();
    loop {
        if let Some(ms) = args.interrupt_after {
            if !interrupted && started.elapsed() >= Duration::from_millis(ms) {
                eprintln!(">> interrupt");
                session.interrupt().map_err(|e| e.to_string())?;
                interrupted = true;
            }
        }
        if Instant::now() >= deadline {
            eprintln!("headless-debug: timed out");
            break;
        }
        let line = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(l)) => l,
            Ok(None) => {
                eprintln!("headless-debug: stdout ended");
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        println!("{line}");
        if let Some(f) = out.as_mut() {
            let _ = writeln!(f, "{line}");
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if v["type"] == "control_request"
            && v["request"]["subtype"] == "can_use_tool"
            && !args.leave_asks
        {
            let id = v["request_id"].as_str().unwrap_or_default();
            let input = v["request"]["input"].clone();
            let allow = if args.deny {
                false
            } else {
                eprint!(
                    "?? allow {} {}? [y/n] ",
                    v["request"]["tool_name"].as_str().unwrap_or("?"),
                    input
                );
                let mut answer = String::new();
                let _ = stdin.lock().read_line(&mut answer);
                answer.trim().eq_ignore_ascii_case("y")
            };
            let response = if allow {
                serde_json::json!({"behavior": "allow", "updatedInput": input})
            } else {
                serde_json::json!({"behavior": "deny", "message": "The user said no."})
            };
            let line = serde_json::json!({
                "type": "control_response",
                "response": {"subtype": "success", "request_id": id, "response": response},
            });
            send(&line.to_string())?;
        }
        if v["type"] == "result" {
            match then.take() {
                Some(next) => {
                    eprintln!(">> prompt: {next}");
                    session.send_prompt(&next).map_err(|e| e.to_string())?;
                }
                None => break,
            }
        }
    }
    for l in session.stderr_tail() {
        eprintln!("stderr: {l}");
    }
    drop(session);
    // The drop ends claude.exe on a thread; give it its grace before this
    // process exits and the job closes on it.
    std::thread::sleep(Duration::from_millis(3500));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_is_every_word_not_a_flag() {
        let a: Vec<String> = ["--cwd", "C:\\x", "--deny", "say", "hi"]
            .map(String::from)
            .to_vec();
        let p = parse(&a).unwrap();
        assert_eq!(p.cwd, Some(PathBuf::from("C:\\x")));
        assert!(p.deny);
        assert_eq!(p.prompt, "say hi");
        assert!(parse(&["--deny".to_string()]).is_err());
    }
}
