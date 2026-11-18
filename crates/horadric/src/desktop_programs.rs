//! `horadric desktop allow|deny|approve|unapprove|list|log`: the list of
//! desktop programs every session may look at or operate without the
//! approval question, and the risky actions in them the human said yes to
//! ahead of time. The running app reads the file again whenever it
//! changes, so an edit here counts from the next call on, with no restart.
//! Any other risky action is asked every time; see
//! [`horadric_core::desktop_programs`].

use std::path::{Path, PathBuf};

use horadric_core::desktop_programs::{self as listed, Approval, Kind, Level, List};

const USAGE: &str = "\
usage: horadric desktop list                       The allowed programs, and where the file is
       horadric desktop allow PROGRAM look|operate  Let every session in without asking
       horadric desktop deny PROGRAM                Take it off the list
       horadric desktop approve PROGRAM click|type NAME [--window W] [--control C]
                                [--text T] [--session S]
       horadric desktop approve PROGRAM press KEY [--window W] [--text T] [--session S]
                                                    Pre-approve one risky action, so it is
                                                    done without asking and logged
       horadric desktop unapprove PROGRAM N         Take the Nth pre-approved action off
       horadric desktop log [N]                     The last N grants and answers (default 20)

PROGRAM is the exe's full path (only that install), or its exe name (any
program of that name). Risky actions are asked every time unless
pre-approved. NAME is the element's name and W the window's title as the
log shows them; each is matched whole, ignoring case, with * for any run
of characters. T is a line of a small dialog's text, such as its
question, and S the session's name, for a pattern meant for one task.";

pub fn run(args: &[String]) -> Result<(), String> {
    let file = horadric_hooks::tasks::desktop_programs_file().ok_or("cannot find %APPDATA%")?;
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["list"] => list(&file),
        ["allow", program, level] => {
            let level = Level::parse(level)?;
            change(&file, |entries| listed::allow(entries, program, level))?;
            let may = match level {
                Level::Look => "look at",
                Level::Operate => "operate",
            };
            println!(
                "Every session may now {may} {} without asking, from its next call.",
                program.trim()
            );
            println!("Risky actions in it are still asked every time.");
            Ok(())
        }
        ["deny", program] => {
            let mut gone = 0;
            change(&file, |entries| {
                gone = listed::deny(entries, program);
                Ok(())
            })?;
            match gone {
                0 => Err(format!("{} is not on the list", program.trim())),
                n => {
                    println!(
                        "Took {n} entr{} off the list. Sessions are asked about it again.",
                        if n == 1 { "y" } else { "ies" }
                    );
                    Ok(())
                }
            }
        }
        ["approve", program, action, named, rest @ ..] => {
            let approval = approval(action, named, rest)?;
            let said = approval.describe();
            let mut on = String::new();
            change(&file, |entries| {
                on = listed::approve(entries, program, approval)?;
                Ok(())
            })?;
            println!("Pre-approved in {on}: {said}.");
            println!(
                "Done without asking from the next call, and logged as pre-approved. \
                 `horadric desktop list` numbers it, for unapprove."
            );
            Ok(())
        }
        ["unapprove", program, n] => {
            let n = n
                .parse()
                .map_err(|_| format!("`{n}` is not a number\n\n{USAGE}"))?;
            let mut gone = String::new();
            change(&file, |entries| {
                gone = listed::unapprove(entries, program, n)?;
                Ok(())
            })?;
            println!("No longer pre-approved: {gone}. It is asked again.");
            Ok(())
        }
        ["log"] => log(20),
        ["log", n] => log(n
            .parse()
            .map_err(|_| format!("`{n}` is not a number\n\n{USAGE}"))?),
        _ => Err(USAGE.into()),
    }
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
            "--session" => &mut a.session,
            other => return Err(format!("`{other}` is not a flag here\n\n{USAGE}")),
        };
        *slot = Some(value).filter(|v| !v.is_empty());
    }
    a.check()?;
    Ok(a)
}

fn read(file: &Path) -> Result<List, String> {
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

/// Reads the list, changes it, and writes it back whole, through a file
/// beside it so the app never reads half of it.
fn change<F: FnOnce(&mut Vec<listed::Entry>) -> Result<(), String>>(
    file: &Path,
    edit: F,
) -> Result<(), String> {
    let mut list = read(file)?;
    edit(&mut list.programs)?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let part = PathBuf::from(format!("{}.part", file.display()));
    std::fs::write(&part, listed::render(&list))
        .and_then(|()| std::fs::rename(&part, file))
        .map_err(|e| format!("{}: {e}", file.display()))
}

fn list(file: &Path) -> Result<(), String> {
    println!("{}", file.display());
    let list = read(file)?;
    if list.programs.is_empty() {
        println!("  No programs are listed. Every session is asked about each program.");
    }
    for e in &list.programs {
        let how = if e.by_path() {
            "this path"
        } else {
            "any exe of this name"
        };
        println!("  {:<8} {}  ({how})", e.level.word(), e.program);
        for (i, a) in e.pre_approved.iter().enumerate() {
            println!("           {}. pre-approved: {}", i + 1, a.describe());
        }
    }
    println!(
        "Other risky actions (transfer, mode, force, reset, delete, write) are asked every \
         time. A question nobody answers within {} s is answered \"no answer\" \
         (\"answer_within\" in the file).",
        list.answer_within()
    );
    Ok(())
}

fn log(n: usize) -> Result<(), String> {
    let file = horadric_hooks::tasks::desktop_log_file().ok_or("cannot find %APPDATA%")?;
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    println!("{}", file.display());
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        println!("  Nothing yet.");
    }
    for line in &lines[lines.len().saturating_sub(n)..] {
        println!("  {line}");
    }
    Ok(())
}
