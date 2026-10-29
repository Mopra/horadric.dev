//! `horadric headless-debug`, hidden: starts a headless `claude.exe`, sends
//! one prompt, prints every stdout line, and ends after the turn's result.
//! For checking the stream-json protocol against the real CLI and for
//! recording the Folio's fixtures. See docs/plan/headless.md.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use horadric_core::folio;
use horadric_pty::headless::{Headless, Options};

const USAGE: &str = "usage: horadric headless-debug [--cwd DIR] [--resume ID] [--deny] \
[--init] [--leave-asks] [--arg FLAG]... [--permission-mode MODE] [--image FILE]... [--reply JSON] [--reply-raw JSON] [--pick N] [--send MS LINE]... [--at-result LINE]... [--midturn LINE]... [--linger MS] [--env K=V]... [--interrupt-after MS] [--then PROMPT] [--out FILE] [--timeout SECS] PROMPT";

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
    /// Image files sent with the first prompt.
    images: Vec<PathBuf>,
    /// Merged into the input echoed back as `updatedInput` when an ask is
    /// allowed, such as `{"answers":{...}}` for AskUserQuestion.
    reply: Option<serde_json::Value>,
    /// Answers every AskUserQuestion with each question's option N.
    pick: Option<usize>,
    /// Fields laid over every ask's `response`, such as
    /// `{"updatedPermissions":[...]}`; a `"behavior":"deny"` drops the input.
    reply_raw: Option<serde_json::Value>,
    /// Lines written to stdin this many milliseconds after the start.
    sends: Vec<(u64, String)>,
    /// Lines written to stdin at the first streamed text, mid turn.
    midturn: Vec<String>,
    /// Lines written to stdin after the first result.
    at_result: Vec<String>,
    /// Set for claude.exe, such as a dead `ANTHROPIC_BASE_URL` to see retries.
    env: Vec<(String, String)>,
    /// How long to keep reading after the last result.
    linger: Option<u64>,
    prompt: String,
}

fn json_value(text: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))
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
            "--permission-mode" => out.extra.push(format!("--permission-mode={}", value()?)),
            "--image" => out.images.push(value()?.into()),
            "--reply" => out.reply = Some(json_value(&value()?)?),
            "--pick" => out.pick = Some(value()?.parse().map_err(|_| "bad option number")?),
            "--reply-raw" => out.reply_raw = Some(json_value(&value()?)?),
            "--send" => {
                let ms = value()?.parse().map_err(|_| "bad milliseconds")?;
                out.sends.push((ms, value()?));
            }
            "--env" => {
                let kv = value()?;
                let (k, v) = kv.split_once('=').ok_or("--env needs K=V")?;
                out.env.push((k.into(), v.into()));
            }
            "--midturn" => out.midturn.push(value()?),
            "--at-result" => out.at_result.push(value()?),
            "--linger" => out.linger = Some(value()?.parse().map_err(|_| "bad milliseconds")?),
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
        env_set: args.env.clone(),
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
    if args.images.is_empty() {
        session
            .send_prompt(&args.prompt)
            .map_err(|e| e.to_string())?;
        eprintln!(">> prompt: {}", args.prompt);
    } else {
        let mut images = Vec::new();
        for p in &args.images {
            let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
            images.push(
                folio::ImageData::from_bytes(&bytes)
                    .ok_or_else(|| format!("{}: not an image the API takes", p.display()))?,
            );
        }
        eprintln!(">> prompt with {} image(s): {}", images.len(), args.prompt);
        session
            .send_line(&folio::prompt_line_with_images(&args.prompt, &images))
            .map_err(|e| e.to_string())?;
    }

    let started = Instant::now();
    let deadline = started + Duration::from_secs(args.timeout.unwrap_or(180));
    let mut interrupted = false;
    let mut then = args.then.clone();
    let mut sends = args.sends.clone();
    let mut at_result = args.at_result.clone();
    let mut midturn = args.midturn.clone();
    let mut ended: Option<Instant> = None;
    let stdin = std::io::stdin();
    loop {
        sends.retain(|(ms, line)| {
            if started.elapsed() < Duration::from_millis(*ms) {
                return true;
            }
            let _ = send(line);
            false
        });
        if ended.is_some_and(|t| t.elapsed() >= Duration::from_millis(args.linger.unwrap_or(0))) {
            break;
        }
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
            } else if args.reply.is_some() || args.reply_raw.is_some() || args.pick.is_some() {
                true
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
            let mut response = if allow {
                let mut input = input;
                if let Some(n) = args.pick {
                    let mut answers = serde_json::Map::new();
                    for q in input["questions"].as_array().into_iter().flatten() {
                        if let (Some(text), Some(label)) =
                            (q["question"].as_str(), q["options"][n]["label"].as_str())
                        {
                            answers.insert(text.into(), label.into());
                        }
                    }
                    input["answers"] = answers.into();
                }
                if let (Some(i), Some(r)) = (
                    input.as_object_mut(),
                    args.reply.as_ref().and_then(|r| r.as_object()),
                ) {
                    for (k, v) in r {
                        i.insert(k.clone(), v.clone());
                    }
                }
                serde_json::json!({"behavior": "allow", "updatedInput": input})
            } else {
                serde_json::json!({"behavior": "deny", "message": "The user said no."})
            };
            if let (Some(r), Some(raw)) = (
                response.as_object_mut(),
                args.reply_raw.as_ref().and_then(|r| r.as_object()),
            ) {
                for (k, v) in raw {
                    r.insert(k.clone(), v.clone());
                }
                if r["behavior"] == "deny" {
                    r.remove("updatedInput");
                }
            }
            let line = serde_json::json!({
                "type": "control_response",
                "response": {"subtype": "success", "request_id": id, "response": response},
            });
            send(&line.to_string())?;
        }
        if v["event"]["delta"]["type"] == "text_delta" {
            for line in midturn.drain(..) {
                send(&line)?;
            }
        }
        if v["type"] == "result" {
            for line in at_result.drain(..) {
                send(&line)?;
            }
            match then.take() {
                Some(next) => {
                    eprintln!(">> prompt: {next}");
                    session.send_prompt(&next).map_err(|e| e.to_string())?;
                }
                None if args.linger.is_some() => ended = Some(Instant::now()),
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
