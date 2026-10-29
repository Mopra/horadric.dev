//! `horadric headless-host-debug`, hidden: the end to end check of headless
//! hosts without the UI. Starts a headless claude.exe in a host, sends a
//! prompt, detaches mid turn, attaches again, prints the replayed and the
//! live lines until the turn's result, then closes the session for good.
//! See docs/plan/headless.md, step 3.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use horadric_pty::headless::Options;
use horadric_pty::headless_host::{Event, Hosted};

const USAGE: &str = "usage: horadric headless-host-debug --cwd DIR [--arg FLAG]... \
[--detach-after MS] [--away MS] [--timeout SECS] PROMPT";

/// How much of a line is printed.
const SHOWN: usize = 160;

#[derive(Debug, PartialEq)]
struct Args {
    cwd: PathBuf,
    extra: Vec<String>,
    detach_after: u64,
    away: u64,
    timeout: u64,
    prompt: String,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut cwd = None;
    let mut extra = Vec::new();
    let (mut detach_after, mut away, mut timeout) = (4000, 3000, 300);
    let mut prompt = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        let number = |v: String| v.parse::<u64>().map_err(|_| format!("{a}: not a number"));
        match a.as_str() {
            "--cwd" => cwd = Some(PathBuf::from(value()?)),
            "--arg" => extra.push(value()?),
            "--detach-after" => detach_after = number(value()?)?,
            "--away" => away = number(value()?)?,
            "--timeout" => timeout = number(value()?)?,
            _ if a.starts_with("--") => return Err(format!("unknown flag {a}\n{USAGE}")),
            _ => prompt.push(a.clone()),
        }
    }
    let cwd = cwd.ok_or_else(|| {
        format!("--cwd is needed, so claude never works in the wrong folder\n{USAGE}")
    })?;
    if prompt.is_empty() {
        return Err(USAGE.to_string());
    }
    Ok(Args {
        cwd,
        extra,
        detach_after,
        away,
        timeout,
        prompt: prompt.join(" "),
    })
}

pub fn run(args: &[String]) -> Result<(), String> {
    let args = parse(args)?;
    let instance = horadric_hooks::instance();
    let id = format!("debug-{}", std::process::id());
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let options = Options {
        args: args.extra.clone(),
        ..Options::default()
    };

    let (tx, rx) = mpsc::channel::<Event>();
    let first = Hosted::start(
        &exe,
        &instance,
        &id,
        &args.cwd,
        None,
        &options,
        "headless-host-debug",
        move |e| {
            let _ = tx.send(e);
        },
    )
    .map_err(|e| format!("could not start the host: {e}"))?;
    println!(
        "started session {id} of instance {instance}, claude.exe pid {}",
        first.pid()
    );
    first.send_prompt(&args.prompt).map_err(|e| e.to_string())?;

    let deadline = Instant::now() + Duration::from_millis(args.detach_after);
    let mut before = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(Event::Out(l)) => {
                print_line("first", &l);
                before.push(l);
            }
            Ok(Event::End(code)) => {
                return Err(format!("claude ended before the detach: {code:?}"))
            }
            Ok(other) => println!("[first] {other:?}"),
            Err(_) => break,
        }
    }
    drop(first);
    println!(
        "detached after {} lines; away for {} ms while the turn runs on",
        before.len(),
        args.away
    );
    std::thread::sleep(Duration::from_millis(args.away));

    let listed = Hosted::list(&instance);
    println!("headless hosts of instance {instance}: {listed:?}");
    let (tx, rx) = mpsc::channel::<Event>();
    let second = Hosted::attach(&instance, &id, move |e| {
        let _ = tx.send(e);
    })
    .map_err(|e| format!("could not attach again: {e}"))?;
    println!("attached again, tag {:?}", second.info().tag);

    let deadline = Instant::now() + Duration::from_secs(args.timeout);
    let (mut replayed, mut live, mut sent) = (Vec::new(), 0usize, 0usize);
    let mut is_live = false;
    let mut result = false;
    while !result {
        let left = deadline
            .checked_duration_since(Instant::now())
            .ok_or("timed out waiting for the turn's result")?;
        match rx.recv_timeout(left) {
            Ok(Event::Out(l)) => {
                result = l.contains(r#""type":"result""#);
                if is_live {
                    live += 1;
                    print_line("live", &l);
                } else {
                    print_line("replay", &l);
                    replayed.push(l);
                }
            }
            Ok(Event::Sent(l)) => {
                sent += 1;
                print_line("replay sent", &l);
            }
            Ok(Event::Live) => {
                is_live = true;
                println!("[live] from here on");
            }
            Ok(Event::Dropped(n)) => println!("[replay] {n} earlier lines dropped"),
            Ok(Event::End(code)) => return Err(format!("claude ended mid turn: {code:?}")),
            Err(_) => return Err("timed out waiting for the turn's result".into()),
        }
    }
    let kept = replayed.len() >= before.len() && replayed[..before.len()] == before[..];
    println!(
        "replayed {} lines and {sent} sent, then {live} live; the replay starts with all {} lines the first client saw: {}",
        replayed.len(),
        before.len(),
        if kept { "yes" } else { "NO" }
    );

    second.close();
    loop {
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(Event::End(code)) => {
                println!("closed: claude ended with {code:?}");
                break;
            }
            Ok(Event::Out(l)) => print_line("live", &l),
            Ok(_) => {}
            Err(_) => return Err("claude did not end after the close".into()),
        }
    }
    for line in second.stderr_tail() {
        println!("[stderr] {line}");
    }
    if !kept {
        return Err("the replay lost lines".into());
    }
    Ok(())
}

fn print_line(tag: &str, line: &str) {
    let mut end = line.len().min(SHOWN);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    let more = if end < line.len() { " ..." } else { "" };
    println!("[{tag}] {}{more}", &line[..end]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flags_and_the_prompt_are_read() {
        let a = parse(&strings(&[
            "--cwd",
            "C:\\scratch",
            "--arg",
            "--model=haiku",
            "--away",
            "10",
            "say",
            "hi",
        ]))
        .unwrap();
        assert_eq!(a.cwd, PathBuf::from("C:\\scratch"));
        assert_eq!(a.extra, vec!["--model=haiku".to_string()]);
        assert_eq!((a.detach_after, a.away, a.timeout), (4000, 10, 300));
        assert_eq!(a.prompt, "say hi");
    }

    #[test]
    fn a_folder_and_a_prompt_are_needed() {
        assert!(parse(&strings(&["hi"])).is_err());
        assert!(parse(&strings(&["--cwd", "x"])).is_err());
        assert!(parse(&strings(&["--cwd", "x", "--away", "soon", "hi"])).is_err());
        assert!(parse(&strings(&["--cwd", "x", "--nope", "hi"])).is_err());
    }
}
