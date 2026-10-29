//! `horadric heavy -- <command>`: runs a test suite or a build once no
//! other session on this machine runs one. See `horadric_core::heavy`.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use horadric_core::heavy::{self, Holder};
use horadric_hooks::SESSION_ENV;

const USAGE: &str = "\
usage: horadric heavy -- <command> [args...]
                                        Run a test suite or a build once no other
                                        session on this machine runs one, and show
                                        a digest of its output, the whole of it
                                        kept in a file
       horadric heavy --full -- <command> [args...]
                                        The same, showing the whole output
       A --timeout before the command is ignored: set the timeout on the
       tool that runs heavy instead.
       horadric heavy                   Say who runs one now";

/// Kept outputs older than this are dropped when the next command runs.
const KEEP_OUTPUT: Duration = Duration::from_secs(24 * 3600);

/// Free physical memory a command waits for before it starts. A `cargo
/// test` links and runs many test binaries at once, and with other
/// sessions holding RAM it was killed for want of memory three times on
/// 2026-10-29. Two GiB is what such a run needs on top of what it
/// already finds in use.
const ENOUGH_MEMORY: u64 = 2 << 30;

/// How long a command waits for memory before it starts anyway: a machine
/// that never frees two GiB must still get its tests run.
const MEMORY_WAIT: Duration = Duration::from_secs(10 * 60);

const MEMORY_POLL: Duration = Duration::from_secs(5);

/// What the words after `heavy` ask for.
#[derive(Debug, PartialEq, Eq)]
struct Request<'a> {
    full: bool,
    /// A `--timeout` was given, which heavy has no use for.
    timeout: bool,
    help: bool,
    words: &'a [String],
}

/// Reads the options before the command. Agents pass `--timeout <ms>`
/// out of habit from their Bash tool, and a heavy that refused it lost
/// them a whole queued run, so it is taken and ignored.
fn parse(mut args: &[String]) -> Request<'_> {
    let mut request = Request {
        full: false,
        timeout: false,
        help: false,
        words: &[],
    };
    while let Some(first) = args.first().map(String::as_str) {
        match first {
            "--full" => {
                request.full = true;
                args = &args[1..];
            }
            "--timeout" => {
                request.timeout = true;
                args = args.get(2..).unwrap_or(&[]);
            }
            t if t.starts_with("--timeout=") => {
                request.timeout = true;
                args = &args[1..];
            }
            "--" => {
                args = &args[1..];
                break;
            }
            "-h" | "--help" | "help" => {
                request.help = true;
                return request;
            }
            _ => break,
        }
    }
    request.words = args;
    request
}

/// What to do about memory before a command starts.
#[derive(Debug, PartialEq, Eq)]
enum MemoryVerdict {
    Go,
    Wait,
    /// Waited [`MEMORY_WAIT`] already: start anyway.
    GiveUp,
}

/// `available` is `None` where this platform cannot tell, which never
/// holds a command back.
fn memory_verdict(available: Option<u64>, waited: Duration) -> MemoryVerdict {
    match available {
        Some(free) if free < ENOUGH_MEMORY => {
            if waited >= MEMORY_WAIT {
                MemoryVerdict::GiveUp
            } else {
                MemoryVerdict::Wait
            }
        }
        _ => MemoryVerdict::Go,
    }
}

/// Exit codes a process gets when the system ran out of memory under it:
/// STATUS_NO_MEMORY and STATUS_COMMITMENT_LIMIT on Windows, and 137 for a
/// SIGKILL, which is how the Linux OOM killer ends one.
const MEMORY_CODES: [i32; 3] = [0xC000_0017_u32 as i32, 0xC000_012D_u32 as i32, 137];

/// What a compiler, linker or test binary prints when an allocation
/// failed. Lowercase, matched against the lowercased output.
const MEMORY_MARKERS: [&str; 8] = [
    "memory allocation of",
    "out of memory",
    "status_no_memory",
    "0xc0000017",
    "lnk1102",
    "paging file is too small",
    "os error 1455",
    "signal: 9, sigkill",
];

/// Whether a failed run looks killed for memory rather than failed on its
/// own, and so is worth one more run.
fn starved(code: i32, output: &str) -> bool {
    if code == 0 {
        return false;
    }
    if MEMORY_CODES.contains(&code) {
        return true;
    }
    let output = output.to_lowercase();
    MEMORY_MARKERS.iter().any(|m| output.contains(m))
}

/// Runs the command in the slot and exits with its exit code, or says who
/// holds the slot when there is no command.
pub fn run(args: &[String]) -> Result<(), String> {
    let request = parse(args);
    if request.help {
        return Err(USAGE.into());
    }
    if request.timeout {
        eprintln!(
            "horadric: heavy ignores --timeout; set the timeout on the Bash tool that runs it."
        );
    }
    let (full, words) = (request.full, request.words);
    let dir = std::env::temp_dir().join("horadric-heavy");
    fs::create_dir_all(&dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    let (lock_path, holder_path) = (dir.join("slot.lock"), dir.join("slot.json"));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("cannot open {}: {e}", lock_path.display()))?;
    if words.is_empty() {
        say_who(&lock, &holder_path);
        return Ok(());
    }
    take(&lock, &holder_path)?;
    let me = Holder {
        command: heavy::command_line(words),
        cwd: std::env::current_dir()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default(),
        session: std::env::var(SESSION_ENV).ok().filter(|s| !s.is_empty()),
        pid: std::process::id(),
        since: unix_now(),
    };
    if let Ok(json) = serde_json::to_string(&me) {
        let _ = fs::write(&holder_path, json);
    }
    let kept = (!full).then(|| dir.join(format!("{}-{}.log", me.since, me.pid)));
    if kept.is_some() {
        drop_old(&dir, me.since);
    }
    // The slot stays held throughout, so the one rerun runs by itself.
    let mut code = 0;
    for attempt in 0..2 {
        wait_for_memory();
        code = start(words, kept.as_deref())?;
        let output = kept
            .as_deref()
            .and_then(|k| fs::read_to_string(k).ok())
            .unwrap_or_default();
        if attempt == 0 && starved(code, &output) {
            eprintln!("horadric: it looks killed for want of memory, running it once more.");
            continue;
        }
        if let Some(kept) = &kept {
            let shown = heavy::digest(&output, code, &kept.to_string_lossy());
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(shown.as_bytes());
            let _ = stdout.flush();
        }
        break;
    }
    let _ = fs::remove_file(&holder_path);
    drop(lock);
    std::process::exit(code)
}

/// Holds the command back until [`ENOUGH_MEMORY`] is free, or for
/// [`MEMORY_WAIT`] at most.
fn wait_for_memory() {
    let begun = Instant::now();
    let mut said = false;
    loop {
        let free = available_memory();
        match memory_verdict(free, begun.elapsed()) {
            MemoryVerdict::Go => return,
            MemoryVerdict::GiveUp => {
                eprintln!("horadric: still short of memory, starting anyway.");
                return;
            }
            MemoryVerdict::Wait => {
                if !said {
                    eprintln!(
                        "horadric: waiting for memory, {} MiB free of the {} MiB a heavy command wants.",
                        free.unwrap_or(0) >> 20,
                        ENOUGH_MEMORY >> 20
                    );
                    said = true;
                }
                std::thread::sleep(MEMORY_POLL);
            }
        }
    }
}

#[cfg(windows)]
fn available_memory() -> Option<u64> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: `status` is a live MEMORYSTATUSEX with its length set, as
    // the call requires.
    unsafe { GlobalMemoryStatusEx(&mut status) }.ok()?;
    Some(status.ullAvailPhys)
}

#[cfg(target_os = "linux")]
fn available_memory() -> Option<u64> {
    meminfo_available(&fs::read_to_string("/proc/meminfo").ok()?)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn available_memory() -> Option<u64> {
    None
}

/// The `MemAvailable` line of `/proc/meminfo`, in bytes.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn meminfo_available(meminfo: &str) -> Option<u64> {
    let line = meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemAvailable:"))?;
    let kib: u64 = line.trim().trim_end_matches("kB").trim().parse().ok()?;
    Some(kib * 1024)
}

/// Takes the slot, first saying whose command it waits for when it is held.
fn take(lock: &File, holder_path: &PathBuf) -> Result<(), String> {
    match lock.try_lock() {
        Ok(()) => return Ok(()),
        Err(TryLockError::WouldBlock) => {}
        Err(TryLockError::Error(e)) => return Err(format!("cannot take the slot: {e}")),
    }
    let line = match read_holder(holder_path) {
        Some(h) => heavy::waiting_line(&h, unix_now()),
        None => "waiting for another session's heavy command to end.".into(),
    };
    eprintln!("horadric: {line}");
    lock.lock()
        .map_err(|e| format!("cannot take the slot: {e}"))
}

fn say_who(lock: &File, holder_path: &PathBuf) {
    match lock.try_lock() {
        Ok(()) => println!("Nobody runs a heavy command now."),
        Err(_) => match read_holder(holder_path) {
            Some(h) => println!(
                "`{}` in {}{}, pid {}, running for {}.",
                h.command,
                h.cwd,
                h.session
                    .map(|s| format!(", session {s}"))
                    .unwrap_or_default(),
                h.pid,
                heavy::duration(unix_now().saturating_sub(h.since))
            ),
            None => println!("Another session runs a heavy command now."),
        },
    }
}

fn read_holder(path: &PathBuf) -> Option<Holder> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/// Runs the command and gives its exit code. Its output goes to `kept`
/// when there is one, standard output and error alike into the one file
/// so they keep their order, else to this terminal. `npm`, `pnpm` and
/// their like are batch files on Windows, which only `cmd.exe` starts.
fn start(words: &[String], kept: Option<&Path>) -> Result<i32, String> {
    let run = |mut c: Command| -> std::io::Result<std::process::ExitStatus> {
        if let Some(kept) = kept {
            let out = File::create(kept)?;
            let err = out.try_clone()?;
            c.stdout(Stdio::from(out)).stderr(Stdio::from(err));
        }
        c.status()
    };
    let mut plain = Command::new(&words[0]);
    plain.args(&words[1..]);
    let status = match run(plain) {
        #[cfg(windows)]
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            use std::os::windows::process::CommandExt;
            let mut shell = Command::new("cmd");
            shell.raw_arg(format!("/d /s /c \"{}\"", heavy::command_line(words)));
            run(shell)
        }
        other => other,
    }
    .map_err(|e| format!("cannot start `{}`: {e}", words[0]))?;
    Ok(exit_code(status))
}

/// A process ended by a signal has no exit code. The shell's 128 plus the
/// signal keeps an OOM kill (SIGKILL, 137) recognisable.
fn exit_code(status: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    status.code().unwrap_or(1)
}

/// Drops kept outputs older than [`KEEP_OUTPUT`], so the folder holds a
/// day of runs and not every run ever.
fn drop_old(dir: &Path, now: u64) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(stamp) = name
            .to_str()
            .and_then(|n| n.strip_suffix(".log"))
            .and_then(|n| n.split('-').next())
            .and_then(|n| n.parse::<u64>().ok())
        else {
            continue;
        };
        if now.saturating_sub(stamp) > KEEP_OUTPUT.as_secs() {
            let _ = fs::remove_file(e.path());
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn plain_command_after_the_dashes() {
        let args = words("-- cargo test");
        let r = parse(&args);
        assert_eq!(r.words, &args[1..]);
        assert!(!r.full && !r.timeout && !r.help);
    }

    #[test]
    fn command_without_dashes_is_the_command() {
        let args = words("cargo test");
        assert_eq!(parse(&args).words, &args[..]);
    }

    #[test]
    fn timeout_with_a_value_is_ignored() {
        let args = words("--timeout 600000 -- cargo test");
        let r = parse(&args);
        assert!(r.timeout);
        assert_eq!(r.words, &words("cargo test")[..]);
    }

    #[test]
    fn timeout_with_equals_is_ignored() {
        let args = words("--timeout=600000 --full -- cargo test");
        let r = parse(&args);
        assert!(r.timeout && r.full);
        assert_eq!(r.words, &words("cargo test")[..]);
    }

    #[test]
    fn full_then_timeout() {
        let args = words("--full --timeout 5 -- npm test");
        let r = parse(&args);
        assert!(r.timeout && r.full);
        assert_eq!(r.words, &words("npm test")[..]);
    }

    #[test]
    fn timeout_after_the_dashes_belongs_to_the_command() {
        let args = words("-- cargo test --timeout 5");
        let r = parse(&args);
        assert!(!r.timeout);
        assert_eq!(r.words, &args[1..]);
    }

    #[test]
    fn timeout_without_a_value_leaves_no_command() {
        let args = words("--timeout");
        let r = parse(&args);
        assert!(r.timeout && r.words.is_empty());
    }

    #[test]
    fn help_is_help() {
        assert!(parse(&words("--help")).help);
        assert!(parse(&words("--timeout 5 -h")).help);
    }

    #[test]
    fn nothing_asks_who() {
        let r = parse(&[]);
        assert!(r.words.is_empty() && !r.help);
    }

    #[test]
    fn enough_memory_goes() {
        let v = memory_verdict(Some(ENOUGH_MEMORY), Duration::ZERO);
        assert_eq!(v, MemoryVerdict::Go);
    }

    #[test]
    fn unknown_memory_goes() {
        assert_eq!(memory_verdict(None, Duration::ZERO), MemoryVerdict::Go);
    }

    #[test]
    fn short_of_memory_waits_then_gives_up() {
        let short = Some(ENOUGH_MEMORY - 1);
        let early = memory_verdict(short, Duration::from_secs(60));
        assert_eq!(early, MemoryVerdict::Wait);
        assert_eq!(memory_verdict(short, MEMORY_WAIT), MemoryVerdict::GiveUp);
    }

    #[test]
    fn passing_run_is_never_starved() {
        assert!(!starved(0, "error: out of memory"));
    }

    #[test]
    fn memory_exit_codes_are_starved() {
        assert!(starved(0xC000_0017_u32 as i32, ""));
        assert!(starved(137, ""));
    }

    #[test]
    fn memory_markers_are_starved() {
        assert!(starved(101, "memory allocation of 1048576 bytes failed"));
        assert!(starved(1, "LINK : fatal error LNK1102: out of memory"));
        assert!(starved(1, "The paging file is too small (os error 1455)"));
        assert!(starved(1, "exit code: 0xc0000017, STATUS_NO_MEMORY"));
        assert!(starved(101, "(signal: 9, SIGKILL: kill)"));
    }

    #[test]
    fn ordinary_failure_is_not_starved() {
        assert!(!starved(101, "test result: FAILED. 3 passed; 1 failed"));
    }

    #[test]
    fn meminfo_reads_available() {
        let info = "MemTotal: 16000000 kB\nMemFree: 100 kB\nMemAvailable:    2048 kB\n";
        assert_eq!(meminfo_available(info), Some(2048 * 1024));
        assert_eq!(meminfo_available("MemTotal: 1 kB"), None);
    }
}
