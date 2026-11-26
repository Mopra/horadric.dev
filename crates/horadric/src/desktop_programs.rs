//! `horadric takeover allow|deny|approve|unapprove|list|log`: the two lists
//! of desktop programs a session may look at or operate without the
//! approval question, and the risky actions in them said yes to ahead of
//! time. The running app reads a file again whenever it changes, so an
//! edit here counts from the next call on, with no restart. Any other
//! risky action is asked every time; see
//! [`horadric_core::desktop_programs`].
//!
//! The user's list is in `%APPDATA%\Horadric` and covers only programs
//! that do not run as administrator. The administrators' list is in
//! `%ProgramData%\Horadric`, written only from an elevated shell, and
//! holds the programs that run as administrator and every pre-approved
//! risky action. This command never elevates itself: started without the
//! rights it says so and ends.
//!
//! No list is changed from inside a session Horadric started: the lists
//! are the human's yes, and an agent must not write its own. For the
//! user's list that is a speed bump and no wall, since the file stays
//! writable by every program of the user, which is why that list can
//! only answer what the user could be asked anyway.

use std::path::{Path, PathBuf};

use horadric_core::desktop_programs::{self as listed, Approval, Entry, Kind, Level, List};

/// The administrators' list on disk, the one file the app reads it with
/// too: see its notes on why it is shared by path.
#[path = "../../horadric-ui/src/desktop_admin.rs"]
pub(crate) mod desktop_admin;

use desktop_admin::Read;

const USAGE: &str = "\
usage: horadric takeover list                        Both lists, and where the files are
       horadric takeover allow PROGRAM look|operate  Let every session in without asking
                                                    (your list: programs that do not run
                                                    as administrator)
       horadric takeover allow --elevated PROGRAM look|operate
                                                    The same for PROGRAM run as
                                                    administrator (administrators' list)
       horadric takeover allow --admin PROGRAM operate
                                                    PROGRAM, not run as administrator, on
                                                    the administrators' list, so risky
                                                    actions in it can be pre-approved
       horadric takeover deny [--elevated|--admin] PROGRAM
                                                    Take it off that list
       horadric takeover approve [--elevated] PROGRAM click|type NAME [--window W]
                                 [--control C] [--text T]
       horadric takeover approve [--elevated] PROGRAM press KEY [--window W] [--text T]
                                                    Pre-approve one risky action, so it is
                                                    done without asking and logged
       horadric takeover unapprove [--elevated] PROGRAM N
                                                    Take the Nth pre-approved action off
       horadric takeover log [--elevated] [N]       The last N grants and answers (default
                                                    20); --elevated for the helper's own log

PROGRAM is the exe's full path (only that install), or its exe name (any
program of that name). Risky actions are asked every time unless
pre-approved. NAME is the element's name and W the window's title as the
log shows them; each is matched whole, ignoring case, with * for any run
of characters. T is a line of a small dialog's text, such as its question.

--elevated, --admin, approve and unapprove change the administrators' list
in %ProgramData%\\Horadric and need an elevated shell (a terminal started
with \"Run as administrator\"). No list can be changed from inside a
session Horadric started: the human runs these.";

/// Said when a session's agent tries to change a list.
const NOT_FROM_A_SESSION: &str = "the lists of desktop programs are the human's to change, and \
this shell runs inside a session Horadric started. Nothing was changed. Ask the human to run the \
command in a terminal of their own";

/// Which list a command means, and which of a program's two selves.
#[derive(Clone, Copy, PartialEq)]
enum Which {
    /// The user's list, the program not elevated.
    User,
    /// The administrators' list, the program not elevated.
    Admin,
    /// The administrators' list, the program run as administrator.
    Elevated,
}

impl Which {
    /// Takes `--elevated` or `--admin` off the front of `args`.
    fn take<'a>(args: &'a [&'a str], or: Which) -> (Which, &'a [&'a str]) {
        match args {
            ["--elevated", rest @ ..] => (Which::Elevated, rest),
            ["--admin", rest @ ..] => (Which::Admin, rest),
            rest => (or, rest),
        }
    }

    fn elevated(self) -> bool {
        self == Which::Elevated
    }
}

pub fn run(args: &[String]) -> Result<(), String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let (verb, rest) = match args.as_slice() {
        [verb, rest @ ..] => (*verb, rest),
        [] => return Err(USAGE.into()),
    };
    let changes = matches!(verb, "allow" | "deny" | "approve" | "unapprove");
    if changes && std::env::var(horadric_hooks::SESSION_ENV).is_ok_and(|s| !s.is_empty()) {
        return Err(NOT_FROM_A_SESSION.into());
    }
    match (verb, rest) {
        ("list", []) => list(),
        ("allow", rest) => {
            let (which, rest) = Which::take(rest, Which::User);
            let [program, level] = rest else {
                return Err(USAGE.into());
            };
            let level = Level::parse(level)?;
            change(which, |entries| {
                listed::allow(entries, program, level, which.elevated())
            })?;
            let may = match level {
                Level::Look => "look at",
                Level::Operate => "operate",
            };
            let how = if which.elevated() {
                " run as administrator"
            } else {
                ""
            };
            println!(
                "Every session may now {may} {}{how} without Horadric's approval question, from \
                 its next call.",
                program.trim()
            );
            match which {
                Which::Elevated => println!(
                    "Windows still asks before the helper starts, unless this machine elevates \
                     without asking. Risky actions in it, and everything in its Open and Save \
                     dialogs, are asked by the helper every time unless pre-approved."
                ),
                Which::Admin => println!("Risky actions in it are asked unless pre-approved."),
                Which::User => println!(
                    "Risky actions in it are still asked every time. Should it run as \
                     administrator, this entry does not cover it: that takes --elevated, from an \
                     elevated shell."
                ),
            }
            Ok(())
        }
        ("deny", rest) => {
            let (which, rest) = Which::take(rest, Which::User);
            let [program] = rest else {
                return Err(USAGE.into());
            };
            let mut gone = 0;
            change(which, |entries| {
                gone = listed::deny(entries, program, which.elevated());
                Ok(())
            })?;
            match gone {
                0 => Err(format!(
                    "{} is not on that list; `horadric takeover list` shows both",
                    program.trim()
                )),
                n => {
                    println!(
                        "Took {n} entr{} off the list. Sessions are asked about it again.",
                        if n == 1 { "y" } else { "ies" }
                    );
                    Ok(())
                }
            }
        }
        ("approve", rest) => {
            let (which, rest) = Which::take(rest, Which::Admin);
            let [program, action, named, flags @ ..] = rest else {
                return Err(USAGE.into());
            };
            let approval = approval(action, named, flags)?;
            let said = approval.describe();
            let mut on = String::new();
            change(admin(which), |entries| {
                on = listed::approve(entries, program, which.elevated(), approval)?;
                Ok(())
            })?;
            println!("Pre-approved in {on}: {said}.");
            println!(
                "Done without asking from the next call, and logged as pre-approved. \
                 `horadric takeover list` numbers it, for unapprove."
            );
            Ok(())
        }
        ("unapprove", rest) => {
            let (which, rest) = Which::take(rest, Which::Admin);
            let [program, n] = rest else {
                return Err(USAGE.into());
            };
            let n = n
                .parse()
                .map_err(|_| format!("`{n}` is not a number\n\n{USAGE}"))?;
            let mut gone = String::new();
            change(admin(which), |entries| {
                gone = listed::unapprove(entries, program, which.elevated(), n)?;
                Ok(())
            })?;
            println!("No longer pre-approved: {gone}. It is asked again.");
            Ok(())
        }
        ("log", ["--elevated"]) => log(true, 20),
        ("log", ["--elevated", n]) => log(true, count(n)?),
        ("log", []) => log(false, 20),
        ("log", [n]) => log(false, count(n)?),
        _ => Err(USAGE.into()),
    }
}

/// A pre-approved action is only ever on the administrators' list.
fn admin(which: Which) -> Which {
    match which {
        Which::User => Which::Admin,
        other => other,
    }
}

fn count(n: &str) -> Result<usize, String> {
    n.parse()
        .map_err(|_| format!("`{n}` is not a number\n\n{USAGE}"))
}

/// A pre-approved action from the command line: its kind, the element's
/// name or the key, and the flags that narrow it.
fn approval(action: &str, named: &str, rest: &[&str]) -> Result<Approval, String> {
    let mut a = Approval {
        action: Kind::parse(action)?,
        window: None,
        control: None,
        name: None,
        key: None,
        text: None,
        session: None,
    };
    match a.action {
        Kind::Press => a.key = Some(named.trim().to_string()),
        Kind::Click | Kind::Type => a.name = Some(named.trim().to_string()),
    }
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        let value = it
            .next()
            .map(|v| v.trim().to_string())
            .ok_or_else(|| format!("{flag} needs a value\n\n{USAGE}"))?;
        let slot = match *flag {
            "--window" => &mut a.window,
            "--control" if a.action != Kind::Press => &mut a.control,
            "--text" => &mut a.text,
            "--session" => {
                return Err(
                    "--session is gone: a session sets its own name, so a pattern for one \
                     session's name was a pattern for any session that took the name. Narrow it \
                     by --window and --text instead"
                        .into(),
                )
            }
            other => return Err(format!("`{other}` is not a flag here\n\n{USAGE}")),
        };
        *slot = Some(value).filter(|v| !v.is_empty());
    }
    a.check()?;
    Ok(a)
}

fn user_file() -> Result<PathBuf, String> {
    horadric_hooks::tasks::desktop_programs_file().ok_or_else(|| "cannot find %APPDATA%".into())
}

/// The user's file as written, with the parts that no longer count.
fn read_user(file: &Path) -> Result<List, String> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    listed::parse(&text).map_err(|why| {
        format!(
            "{} does not parse, so nothing on it is allowed: {why}. Fix it by hand, or delete it \
             and allow again",
            file.display()
        )
    })
}

/// Reads a list, changes it, and writes it back whole. The user's goes
/// through a file beside it so the app never reads half of it; the
/// administrators' is written by [`desktop_admin::write`], which wants an
/// elevated shell and says so.
fn change<F: FnOnce(&mut Vec<Entry>) -> Result<(), String>>(
    which: Which,
    edit: F,
) -> Result<(), String> {
    if which != Which::User {
        if !desktop_admin::elevated() {
            return Err(desktop_admin::NOT_ELEVATED.into());
        }
        let mut list = match desktop_admin::read() {
            Read::List(list) => list,
            Read::Missing => List::default(),
            Read::Refused(why) => return Err(why),
        };
        edit(&mut list.programs)?;
        return desktop_admin::write(&list).map(|_| ());
    }
    let file = user_file()?;
    let mut list = read_user(&file)?;
    edit(&mut list.programs)?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let part = PathBuf::from(format!("{}.part", file.display()));
    std::fs::write(&part, listed::render(&list))
        .and_then(|()| std::fs::rename(&part, &file))
        .map_err(|e| format!("{}: {e}", file.display()))
}

fn print_entries(programs: &[Entry], patterns: bool) {
    for e in programs {
        let how = if e.by_path() {
            "this path"
        } else {
            "any exe of this name"
        };
        let run = if e.elevated {
            ", run as administrator"
        } else {
            ""
        };
        println!("  {:<8} {}  ({how}{run})", e.level.word(), e.program);
        if !patterns {
            continue;
        }
        for (i, a) in e.pre_approved.iter().enumerate() {
            println!("           {}. pre-approved: {}", i + 1, a.describe());
        }
    }
}

fn list() -> Result<(), String> {
    let file = user_file()?;
    println!("Your list: {}", file.display());
    println!("  (any program of yours can write it, so it covers only programs that do not run");
    println!("  as administrator, and no risky action)");
    let (user, misplaced) = listed::user_list(read_user(&file)?);
    if user.programs.is_empty() {
        println!("  No programs are listed.");
    }
    print_entries(&user.programs, false);
    if !misplaced.is_empty() {
        println!("  No longer used from this file. To keep each, run from an elevated shell:");
        for m in &misplaced {
            println!("    {}", m.what);
            println!("      {}", m.command);
        }
        println!(
            "  (a pre-approved action needs its program on the administrators' list at operate \
             first: horadric takeover allow --admin PROGRAM operate, or --elevated)"
        );
    }
    println!();
    let admin_file = desktop_admin::file_read().ok_or("Windows would not say where %ProgramData% is")?;
    println!("The administrators' list: {}", admin_file.display());
    println!("  (written only from an elevated shell; programs that run as administrator, and");
    println!("  every pre-approved risky action)");
    let admin = match desktop_admin::read() {
        Read::Missing => {
            println!("  There is none yet.");
            List::default()
        }
        Read::Refused(why) => {
            println!("  NOT USED: {why}");
            List::default()
        }
        Read::List(list) => {
            if list.programs.is_empty() {
                println!("  No programs are listed.");
            }
            print_entries(&list.programs, true);
            list
        }
    };
    println!();
    let within = List {
        programs: Vec::new(),
        answer_within: admin.answer_within.or(user.answer_within),
    }
    .answer_within();
    println!(
        "A program on neither list is asked about, per session. Other risky actions (transfer, \
         mode, force, reset, delete, write) are asked every time. A question nobody answers \
         within {within} s is answered \"no answer\" (\"answer_within\" in either file), and \
         one refused or unanswered is not asked again for {} s.",
        listed::REFUSED_FOR.as_secs()
    );
    Ok(())
}

fn log(elevated: bool, n: usize) -> Result<(), String> {
    let file = if elevated {
        desktop_admin::log_file_read().ok_or("Windows would not say where %ProgramData% is")?
    } else {
        horadric_hooks::tasks::desktop_log_file().ok_or("cannot find %APPDATA%")?
    };
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    println!("{}", file.display());
    if elevated {
        println!("  (written by the elevated helper; only administrators can change it)");
    } else {
        println!("  (any program of yours can write this file: it is a record, not proof)");
    }
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        println!("  Nothing yet.");
    }
    for line in &lines[lines.len().saturating_sub(n)..] {
        println!("  {line}");
    }
    Ok(())
}
