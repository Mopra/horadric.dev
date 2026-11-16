//! The desktop programs the human allows ahead of time, so a session can
//! look at or operate them without the approval question each new session
//! would otherwise ask. Kept in `desktop-programs.json` beside the app's
//! state, edited by hand or with `horadric desktop allow|deny|list`:
//!
//! ```json
//! {
//!   "programs": [
//!     { "program": "C:\\Program Files\\OMRON\\Sysmac Studio\\SysmacStudio.exe", "level": "operate" },
//!     { "program": "notepad.exe", "level": "look" }
//!   ]
//! }
//! ```
//!
//! A program is named by its full path, which only that install matches,
//! or by its exe name, which any program of that name matches wherever it
//! lies. The list only ever stands in for the approval question. A risky
//! action (a transfer to a controller, a mode change, forcing, resetting,
//! deleting, writing) is still asked of the human every time, and nothing
//! here can say otherwise: there is no level for it.

use serde_json::{json, Value};

/// The file's name, in `%APPDATA%\Horadric` (`Horadric-dev` for a dev
/// instance).
pub const FILE: &str = "desktop-programs.json";

/// How far a listed program is let in. There is deliberately no level that
/// covers risky actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Look,
    Operate,
}

impl Level {
    pub fn parse(word: &str) -> Result<Level, String> {
        match word.trim().to_lowercase().as_str() {
            "look" => Ok(Level::Look),
            "operate" => Ok(Level::Operate),
            other => Err(format!("level `{other}` is neither look nor operate")),
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Level::Look => "look",
            Level::Operate => "operate",
        }
    }
}

/// One listed program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// A full path to the exe, or only its file name.
    pub program: String,
    pub level: Level,
}

impl Entry {
    /// Whether this names the program by its full path.
    pub fn by_path(&self) -> bool {
        is_path(&self.program)
    }
}

fn is_path(program: &str) -> bool {
    program.contains(['\\', '/'])
}

/// Horadric's own programs, which the list never covers: a session that
/// may operate Horadric could answer its own risky questions.
pub fn is_horadric(exe: &str) -> bool {
    let name = file_name(exe).to_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    name == "horadric" || name == "horadricw" || name.starts_with("horadric-")
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// A program as written, made comparable: trimmed, backslashes for
/// slashes, lower case.
fn normal(program: &str) -> String {
    program.trim().replace('/', "\\").to_lowercase()
}

/// An exe name without `.exe`, lower case.
fn bare(name: &str) -> String {
    let name = name.trim().to_lowercase();
    name.strip_suffix(".exe").unwrap_or(&name).to_string()
}

/// Checks a program the human means to list.
pub fn check(program: &str) -> Result<(), String> {
    let p = program.trim();
    if p.is_empty() {
        return Err("say which program: its full path or its exe name".into());
    }
    if file_name(p).trim().is_empty() || bare(file_name(p)).is_empty() || p == "?" {
        return Err(format!("`{p}` does not name a program"));
    }
    if is_horadric(p) {
        return Err("Horadric's own programs can not be listed".into());
    }
    Ok(())
}

/// Whether `program`, as listed, names the program running from `path`,
/// whose file name is `exe`. A path names exactly that path; a name, any
/// exe of that name, with or without `.exe`. Case never matters.
pub fn matches(program: &str, path: &str, exe: &str) -> bool {
    if is_path(program) {
        !path.trim().is_empty() && normal(program) == normal(path)
    } else {
        let exe = if exe.trim().is_empty() {
            file_name(path)
        } else {
            exe
        };
        !bare(exe).is_empty() && bare(program) == bare(exe)
    }
}

/// The entry that covers the program at `path` named `exe`, if any. One
/// listed by path is surer than one listed by name, so it wins whatever
/// the order; otherwise the first in the file. Horadric's own programs are
/// never covered.
pub fn find<'a>(entries: &'a [Entry], path: &str, exe: &str) -> Option<&'a Entry> {
    if is_horadric(exe) || is_horadric(path) {
        return None;
    }
    let hits = || entries.iter().filter(|e| matches(&e.program, path, exe));
    hits().find(|e| e.by_path()).or_else(|| hits().next())
}

/// Reads the file. Empty or blank is an empty list. Anything wrong fails
/// the whole file, so a typo never lets in more than was meant; the app
/// then falls back to asking.
pub fn parse(text: &str) -> Result<Vec<Entry>, String> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let value: Value =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
    let list = value
        .get("programs")
        .and_then(Value::as_array)
        .ok_or("the file needs a \"programs\" list")?;
    list.iter()
        .enumerate()
        .map(|(i, item)| {
            let at = |why: String| format!("program {}: {why}", i + 1);
            let program = item
                .get("program")
                .and_then(Value::as_str)
                .ok_or_else(|| at("needs a \"program\": its full path or exe name".into()))?;
            check(program).map_err(at)?;
            let level = item
                .get("level")
                .and_then(Value::as_str)
                .ok_or_else(|| at("needs a \"level\": look or operate".into()))?;
            Ok(Entry {
                program: program.trim().to_string(),
                level: Level::parse(level).map_err(at)?,
            })
        })
        .collect()
}

/// Writes the list back as the file holds it.
pub fn render(entries: &[Entry]) -> String {
    let programs: Vec<Value> = entries
        .iter()
        .map(|e| json!({ "program": e.program, "level": e.level.word() }))
        .collect();
    let mut text = serde_json::to_string_pretty(&json!({ "programs": programs }))
        .expect("a list of strings always serialises");
    text.push('\n');
    text
}

/// Lists `program` at `level`, in place of any entry naming it the same
/// way.
pub fn allow(entries: &mut Vec<Entry>, program: &str, level: Level) -> Result<(), String> {
    check(program)?;
    let entry = Entry {
        program: program.trim().to_string(),
        level,
    };
    match entries
        .iter_mut()
        .find(|e| same(&e.program, &entry.program))
    {
        Some(e) => *e = entry,
        None => entries.push(entry),
    }
    Ok(())
}

/// Takes `program` off the list: every entry naming it the same way, or,
/// given only a name, every entry whose path ends in that name too, since
/// "deny Sysmac" means none of it. Says how many went.
pub fn deny(entries: &mut Vec<Entry>, program: &str) -> usize {
    let before = entries.len();
    let by_name = !is_path(program);
    entries.retain(|e| {
        let gone = same(&e.program, program)
            || (by_name && e.by_path() && bare(file_name(&e.program)) == bare(program));
        !gone
    });
    before - entries.len()
}

/// The log of what was allowed and refused, beside the list, so the human
/// can see afterwards which grants came from the list and which from a
/// question.
pub const LOG_FILE: &str = "desktop.log";

/// Where a decision about a call came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The list, with no question asked.
    List(Level),
    /// The human, answering the approval question.
    Asked(Option<Level>),
    /// The human, answering a risky action's question.
    Risky(bool),
    /// The list could not be read; the app asks instead.
    Unreadable,
    /// Not asked because the human turned bypass on, on the Thinking
    /// plate, and was there to stop it with Esc.
    Bypass,
    /// The human turned bypass on, so the log shows where the unasked
    /// calls after it begin.
    BypassOn,
    /// Bypass ended, and how: by Esc, by its switch, or by itself once the
    /// work it was on for was done.
    BypassOff(&'static str),
}

/// One line of [`LOG_FILE`]: when, which session, how, which program, and
/// what the call wanted.
pub fn log_line(utc: &str, session: &str, source: Source, program: &str, what: &str) -> String {
    let how = match source {
        Source::List(level) => format!("{} from the list, not asked", level.word()),
        Source::Asked(Some(level)) => format!("{} as the human answered", level.word()),
        Source::Asked(None) => "refused by the human".into(),
        Source::Risky(true) => "risky action done as the human answered".into(),
        Source::Risky(false) => "risky action refused by the human".into(),
        Source::Unreadable => "the list could not be read, asking instead".into(),
        Source::Bypass => "not asked, the human had bypass on".into(),
        Source::BypassOn => "bypass turned on by the human".into(),
        Source::BypassOff(how) => format!("bypass off: {how}"),
    };
    let flat = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut line = format!("{utc}  {}  {how}  {}", flat(session), flat(program));
    if !what.trim().is_empty() {
        line.push_str(&format!("  ({})", flat(what)));
    }
    line.push('\n');
    line
}

fn same(a: &str, b: &str) -> bool {
    match (is_path(a), is_path(b)) {
        (true, true) => normal(a) == normal(b),
        (false, false) => bare(a) == bare(b),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYSMAC: &str = r"C:\Program Files\OMRON\Sysmac Studio\SysmacStudio.exe";

    fn entry(program: &str, level: Level) -> Entry {
        Entry {
            program: program.into(),
            level,
        }
    }

    #[test]
    fn the_file_is_read_with_its_levels() {
        let text = r#"{
            "programs": [
                { "program": "C:\\Program Files\\OMRON\\Sysmac Studio\\SysmacStudio.exe", "level": "operate" },
                { "program": " notepad.exe ", "level": "Look" }
            ]
        }"#;
        assert_eq!(
            parse(text),
            Ok(vec![
                entry(SYSMAC, Level::Operate),
                entry("notepad.exe", Level::Look)
            ])
        );
    }

    #[test]
    fn an_empty_file_is_an_empty_list() {
        assert_eq!(parse(""), Ok(vec![]));
        assert_eq!(parse("  \n"), Ok(vec![]));
        assert_eq!(parse(r#"{"programs": []}"#), Ok(vec![]));
        assert_eq!(parse("\u{feff}{\"programs\": []}"), Ok(vec![]));
    }

    #[test]
    fn anything_wrong_fails_the_whole_file() {
        assert!(parse("{").is_err());
        assert!(parse("{}").is_err());
        assert!(parse(r#"{"programs": [{"program": "a.exe"}]}"#).is_err());
        assert!(parse(r#"{"programs": [{"level": "look"}]}"#).is_err());
        assert!(parse(r#"{"programs": [{"program": " ", "level": "look"}]}"#).is_err());
        let two = r#"{"programs": [
            {"program": "a.exe", "level": "operate"},
            {"program": "b.exe", "level": "always"}
        ]}"#;
        let err = parse(two).unwrap_err();
        assert!(err.starts_with("program 2:"), "{err}");
    }

    #[test]
    fn there_is_no_level_for_risky_actions() {
        for word in ["risky", "all", "force", "transfer", "write", "none", ""] {
            assert!(Level::parse(word).is_err(), "{word}");
        }
        let risky = r#"{"programs": [{"program": "a.exe", "level": "operate", "risky": true}]}"#;
        // A stray field changes nothing: operate is the most there is.
        assert_eq!(parse(risky), Ok(vec![entry("a.exe", Level::Operate)]));
    }

    #[test]
    fn a_path_matches_only_that_path() {
        assert!(matches(SYSMAC, SYSMAC, "SysmacStudio.exe"));
        assert!(matches(
            "c:/program files/omron/sysmac studio/sysmacstudio.exe",
            SYSMAC,
            "SysmacStudio.exe"
        ));
        assert!(!matches(
            SYSMAC,
            r"C:\Users\x\Downloads\SysmacStudio.exe",
            "SysmacStudio.exe"
        ));
        assert!(!matches(SYSMAC, "", "SysmacStudio.exe"));
    }

    #[test]
    fn a_name_matches_that_exe_anywhere() {
        let elsewhere = r"D:\Tools\SysmacStudio.exe";
        assert!(matches("SysmacStudio.exe", elsewhere, "SysmacStudio.exe"));
        assert!(matches("sysmacstudio", elsewhere, "SysmacStudio.exe"));
        assert!(matches("SysmacStudio", elsewhere, ""));
        assert!(!matches("Sysmac", elsewhere, "SysmacStudio.exe"));
        assert!(!matches("SysmacStudio.exe", "", "?"));
        assert!(!matches("notepad.exe", r"C:\Windows\notepad.exe.bak", ""));
    }

    #[test]
    fn a_path_entry_wins_over_a_name_entry() {
        let list = vec![
            entry("SysmacStudio.exe", Level::Operate),
            entry(SYSMAC, Level::Look),
        ];
        assert_eq!(
            find(&list, SYSMAC, "SysmacStudio.exe").map(|e| e.level),
            Some(Level::Look)
        );
        assert_eq!(
            find(&list, r"D:\SysmacStudio.exe", "SysmacStudio.exe").map(|e| e.level),
            Some(Level::Operate)
        );
        assert_eq!(find(&list, r"C:\Windows\notepad.exe", "notepad.exe"), None);
    }

    #[test]
    fn horadric_is_never_covered() {
        let list = vec![
            entry("horadric.exe", Level::Operate),
            entry("horadricw", Level::Operate),
        ];
        assert_eq!(find(&list, r"C:\x\horadric.exe", "horadric.exe"), None);
        assert_eq!(find(&list, r"C:\x\horadricw.exe", "HoradricW.exe"), None);
        assert!(check("horadric.exe").is_err());
        assert!(check(r"C:\x\horadric-host-1.exe").is_err());
        assert!(parse(r#"{"programs": [{"program": "horadric", "level": "look"}]}"#).is_err());
        assert!(!is_horadric("horadrical.exe"));
    }

    #[test]
    fn allowing_replaces_the_same_program() {
        let mut list = vec![];
        allow(&mut list, SYSMAC, Level::Look).unwrap();
        allow(&mut list, "notepad", Level::Look).unwrap();
        allow(&mut list, &SYSMAC.to_uppercase(), Level::Operate).unwrap();
        allow(&mut list, "NOTEPAD.EXE", Level::Operate).unwrap();
        assert_eq!(
            list,
            vec![
                entry(&SYSMAC.to_uppercase(), Level::Operate),
                entry("NOTEPAD.EXE", Level::Operate)
            ]
        );
        assert!(allow(&mut list, " ", Level::Look).is_err());
        assert!(allow(&mut list, "?", Level::Look).is_err());
        assert!(allow(&mut list, r"C:\dir\", Level::Look).is_err());
    }

    #[test]
    fn denying_a_name_takes_its_paths_too() {
        let mut list = vec![
            entry(SYSMAC, Level::Operate),
            entry("SysmacStudio.exe", Level::Look),
            entry("notepad.exe", Level::Look),
        ];
        assert_eq!(deny(&mut list, "sysmacstudio"), 2);
        assert_eq!(list, vec![entry("notepad.exe", Level::Look)]);
        assert_eq!(deny(&mut list, "calc.exe"), 0);
        let mut list = vec![
            entry(SYSMAC, Level::Operate),
            entry("SysmacStudio", Level::Look),
        ];
        assert_eq!(deny(&mut list, SYSMAC), 1);
        assert_eq!(list, vec![entry("SysmacStudio", Level::Look)]);
    }

    #[test]
    fn the_log_says_where_a_grant_came_from() {
        let at = "2026-11-11T10:00:00Z";
        assert_eq!(
            log_line(
                at,
                "fix plc",
                Source::List(Level::Operate),
                SYSMAC,
                "click\nOK"
            ),
            format!("{at}  fix plc  operate from the list, not asked  {SYSMAC}  (click OK)\n")
        );
        assert_eq!(
            log_line(at, "s", Source::Asked(Some(Level::Look)), "a.exe", " "),
            format!("{at}  s  look as the human answered  a.exe\n")
        );
        assert!(log_line(at, "s", Source::Risky(false), "a.exe", "").contains("refused"));
        assert!(log_line(at, "s", Source::Asked(None), "a.exe", "").contains("refused"));
        assert!(log_line(at, "s", Source::Bypass, "a.exe", "").contains("bypass"));
        assert_eq!(
            log_line(at, "s", Source::BypassOn, "a.exe", ""),
            format!("{at}  s  bypass turned on by the human  a.exe\n")
        );
        assert_eq!(
            log_line(at, "s", Source::BypassOff("Esc"), "a.exe", ""),
            format!("{at}  s  bypass off: Esc  a.exe\n")
        );
    }

    #[test]
    fn the_file_reads_back_what_was_written() {
        let list = vec![
            entry(SYSMAC, Level::Operate),
            entry("notepad.exe", Level::Look),
        ];
        assert_eq!(parse(&render(&list)), Ok(list));
        assert_eq!(parse(&render(&[])), Ok(vec![]));
    }
}
