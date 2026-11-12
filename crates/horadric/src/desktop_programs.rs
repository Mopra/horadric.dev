//! `horadric desktop allow|deny|list|log`: the list of desktop programs
//! every session may look at or operate without the approval question.
//! The running app reads the file again whenever it changes, so an edit
//! here counts from the next call on, with no restart. Risky actions are
//! asked every time whatever the list says; see
//! [`horadric_core::desktop_programs`].

use std::path::{Path, PathBuf};

use horadric_core::desktop_programs::{self as listed, Entry, Level};

const USAGE: &str = "\
usage: horadric desktop list                       The allowed programs, and where the file is
       horadric desktop allow PROGRAM look|operate  Let every session in without asking
       horadric desktop deny PROGRAM                Take it off the list
       horadric desktop log [N]                     The last N grants and answers (default 20)

PROGRAM is the exe's full path (only that install), or its exe name (any
program of that name). Risky actions are asked every time all the same.";

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
        ["log"] => log(20),
        ["log", n] => log(n
            .parse()
            .map_err(|_| format!("`{n}` is not a number\n\n{USAGE}"))?),
        _ => Err(USAGE.into()),
    }
}

fn read(file: &Path) -> Result<Vec<Entry>, String> {
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
fn change<F: FnOnce(&mut Vec<Entry>) -> Result<(), String>>(
    file: &Path,
    edit: F,
) -> Result<(), String> {
    let mut entries = read(file)?;
    edit(&mut entries)?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let part = PathBuf::from(format!("{}.part", file.display()));
    std::fs::write(&part, listed::render(&entries))
        .and_then(|()| std::fs::rename(&part, file))
        .map_err(|e| format!("{}: {e}", file.display()))
}

fn list(file: &Path) -> Result<(), String> {
    println!("{}", file.display());
    let entries = read(file)?;
    if entries.is_empty() {
        println!("  No programs are listed. Every session is asked about each program.");
    }
    for e in &entries {
        let how = if e.by_path() {
            "this path"
        } else {
            "any exe of this name"
        };
        println!("  {:<8} {}  ({how})", e.level.word(), e.program);
    }
    println!("Risky actions (transfer, mode, force, reset, delete, write) are asked every time.");
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
