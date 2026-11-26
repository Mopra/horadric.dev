//! The desktop programs the human allows ahead of time, so a session can
//! look at or operate them without the approval question each new session
//! would otherwise ask. Kept in `desktop-programs.json` beside the app's
//! state, edited by hand or with `horadric desktop allow|deny|approve|list`:
//!
//! ```json
//! {
//!   "answer_within": 60,
//!   "programs": [
//!     {
//!       "program": "C:\Program Files\OMRON\Sysmac Studio\SysmacStudio.exe",
//!       "level": "operate",
//!       "pre_approved": [
//!         { "action": "click", "control": "Button", "name": "_Delete", "window": "Open Project" },
//!         { "action": "press", "key": "Enter", "window": "Import solution" }
//!       ]
//!     },
//!     { "program": "notepad.exe", "level": "look" }
//!   ]
//! }
//! ```
//!
//! A program is named by its full path, which only that install matches,
//! or by its exe name, which any program of that name matches wherever it
//! lies. The level only ever stands in for the approval question. A risky
//! action (a transfer to a controller, a mode change, forcing, resetting,
//! deleting, writing) is still asked of the human every time, unless it
//! matches one of the program's `pre_approved` patterns: the technician's
//! yes given ahead of time for one named action (its kind, the element's
//! name or the key, and optionally the window, the control, a dialog's
//! text and the sessions it is for). Every such action is logged with the
//! pattern that let it through. There is no level that covers risky
//! actions wholesale.
//!
//! `answer_within` is how many seconds a question waits for the human
//! before the session is told nobody answered (60 when not set).

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
    /// Risky actions the human said yes to ahead of time.
    pub pre_approved: Vec<Approval>,
}

/// The whole file: the programs, and how long a question waits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct List {
    pub programs: Vec<Entry>,
    /// Seconds a question waits for the human, when set.
    pub answer_within: Option<u64>,
}

/// How long a question waits for the human when the file does not say.
pub const ANSWER_WITHIN: u64 = 60;
/// The shortest wait the file may set: a human must be able to read it.
const ANSWER_SHORTEST: u64 = 10;
/// The longest: the agent's call gives up at 90 seconds, and must hear
/// "no answer" before that rather than a timeout.
const ANSWER_LONGEST: u64 = 80;

impl List {
    /// Seconds a question waits, within what the agent's call allows.
    pub fn answer_within(&self) -> u64 {
        self.answer_within
            .unwrap_or(ANSWER_WITHIN)
            .clamp(ANSWER_SHORTEST, ANSWER_LONGEST)
    }
}

/// What kind of operate action a call is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Click,
    Type,
    Press,
}

impl Kind {
    pub fn parse(word: &str) -> Result<Kind, String> {
        match word.trim().to_lowercase().as_str() {
            "click" => Ok(Kind::Click),
            "type" => Ok(Kind::Type),
            "press" => Ok(Kind::Press),
            other => Err(format!("action `{other}` is none of click, type, press")),
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Kind::Click => "click",
            Kind::Type => "type",
            Kind::Press => "press",
        }
    }
}

/// An operate call as a pre-approved pattern is matched against it, from
/// what the MCP process sends beside its sentence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Act {
    pub kind: Option<Kind>,
    /// The title of the window acted in: the dialog's own, for a dialog.
    pub window: String,
    /// The element's UI Automation control type, like `Button`.
    pub control: String,
    /// The element's name, or for a key, the focused element's.
    pub name: String,
    /// The key pressed, as the agent named it, like `Enter`.
    pub key: String,
    /// What a small dialog says, its lines of text.
    pub texts: Vec<String>,
}

/// One risky action the human said yes to ahead of time. Every field set
/// must match; one left out matches anything. Text is matched whole,
/// ignoring case and runs of spaces, with `*` for any run of characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approval {
    pub action: Kind,
    /// The title of the window acted in.
    pub window: Option<String>,
    pub control: Option<String>,
    /// The element's name. Needed for a click or typing.
    pub name: Option<String>,
    /// The key. Needed for a press.
    pub key: Option<String>,
    /// A line of a small dialog's text, such as its question.
    pub text: Option<String>,
    /// The session's name: a pattern for one task only.
    pub session: Option<String>,
}

const APPROVAL_FIELDS: [&str; 7] = [
    "action", "window", "control", "name", "key", "text", "session",
];

impl Approval {
    /// Whether this pattern names its action narrowly enough to stand for
    /// the human's yes: a click or typing by the element's name, a press by
    /// its key, and neither by wildcards alone.
    pub fn check(&self) -> Result<(), String> {
        let named = |v: &Option<String>| v.as_deref().is_some_and(|s| !only_wild(s));
        match self.action {
            Kind::Press if !named(&self.key) => {
                Err("a press needs its \"key\", like \"Enter\"".into())
            }
            Kind::Click | Kind::Type if !named(&self.name) => Err(format!(
                "a {} needs the element's \"name\", like \"_Delete\" or \"File name:\"",
                self.action.word()
            )),
            _ => Ok(()),
        }
    }

    /// Whether `act`, made by the session named `session`, is this one.
    pub fn matches(&self, act: &Act, session: &str) -> bool {
        let fits =
            |pattern: &Option<String>, text: &str| pattern.as_deref().is_none_or(|p| glob(p, text));
        act.kind == Some(self.action)
            && fits(&self.window, &act.window)
            && fits(&self.control, &act.control)
            && fits(&self.name, &act.name)
            && fits(&self.key, &act.key)
            && fits(&self.session, session)
            && self
                .text
                .as_deref()
                .is_none_or(|p| act.texts.iter().any(|t| glob(p, t)))
    }

    /// The pattern in words, for the log and the list.
    pub fn describe(&self) -> String {
        let mut said = self.action.word().to_string();
        if let Some(c) = &self.control {
            said.push_str(&format!(" {c}"));
        }
        if let Some(n) = &self.name {
            said.push_str(&format!(" {n:?}"));
        }
        if let Some(k) = &self.key {
            said.push_str(&format!(" {k}"));
        }
        if let Some(w) = &self.window {
            said.push_str(&format!(" in window {w:?}"));
        }
        if let Some(t) = &self.text {
            said.push_str(&format!(" saying {t:?}"));
        }
        if let Some(s) = &self.session {
            said.push_str(&format!(" for sessions {s:?}"));
        }
        said
    }

    fn read(item: &Value) -> Result<Approval, String> {
        let object = item
            .as_object()
            .ok_or("a pre-approved action is an object")?;
        if let Some(stray) = object
            .keys()
            .find(|k| !APPROVAL_FIELDS.contains(&k.as_str()))
        {
            // A misspelt field would match anything, more than was meant.
            return Err(format!(
                "\"{stray}\" is not a field of a pre-approved action (they are {})",
                APPROVAL_FIELDS.join(", ")
            ));
        }
        let text = |k: &str| -> Result<Option<String>, String> {
            match object.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
                Some(Value::String(s)) => Ok(Some(s.trim().to_string())),
                Some(_) => Err(format!("\"{k}\" is text")),
            }
        };
        let action = text("action")?.ok_or("needs an \"action\": click, type or press")?;
        let approval = Approval {
            action: Kind::parse(&action)?,
            window: text("window")?,
            control: text("control")?,
            name: text("name")?,
            key: text("key")?,
            text: text("text")?,
            session: text("session")?,
        };
        approval.check()?;
        Ok(approval)
    }

    fn json(&self) -> Value {
        let mut o = serde_json::Map::new();
        o.insert("action".into(), json!(self.action.word()));
        for (k, v) in [
            ("window", &self.window),
            ("control", &self.control),
            ("name", &self.name),
            ("key", &self.key),
            ("text", &self.text),
            ("session", &self.session),
        ] {
            if let Some(v) = v {
                o.insert(k.into(), json!(v));
            }
        }
        Value::Object(o)
    }
}

/// The pattern on `entry` that lets `act` go ahead unasked, if any. Only
/// an entry at operate carries any.
pub fn pre_approved<'a>(entry: &'a Entry, act: &Act, session: &str) -> Option<&'a Approval> {
    if entry.level != Level::Operate {
        return None;
    }
    entry.pre_approved.iter().find(|a| a.matches(act, session))
}

fn only_wild(s: &str) -> bool {
    s.chars().all(|c| c == '*' || c.is_whitespace())
}

/// Text made comparable for a pattern: lower case, runs of spaces as one.
fn flat(s: &str) -> Vec<char> {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .chars()
        .collect()
}

/// Whether `text` is `pattern` whole, `*` standing for any run.
pub fn glob(pattern: &str, text: &str) -> bool {
    let (p, t) = (flat(pattern), flat(text));
    // The classic two pointer match, going back to the last star.
    let (mut i, mut j) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while j < t.len() {
        if i < p.len() && p[i] != '*' && p[i] == t[j] {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some((i, j));
            i += 1;
        } else if let Some((si, sj)) = star {
            i = si + 1;
            j = sj + 1;
            star = Some((si, sj + 1));
        } else {
            return false;
        }
    }
    p[i..].iter().all(|&c| c == '*')
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
pub fn parse(text: &str) -> Result<List, String> {
    if text.trim().is_empty() {
        return Ok(List::default());
    }
    let value: Value =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
    let list = value
        .get("programs")
        .and_then(Value::as_array)
        .ok_or("the file needs a \"programs\" list")?;
    let answer_within = match value.get("answer_within") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .ok_or("\"answer_within\" is a whole number of seconds")?,
        ),
    };
    let programs = list
        .iter()
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
            let level = Level::parse(level).map_err(at)?;
            let pre_approved = match item.get("pre_approved") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(list)) => list
                    .iter()
                    .enumerate()
                    .map(|(j, a)| {
                        Approval::read(a)
                            .map_err(|why| at(format!("pre-approved {}: {why}", j + 1)))
                    })
                    .collect::<Result<_, _>>()?,
                Some(_) => return Err(at("\"pre_approved\" is a list".into())),
            };
            if level != Level::Operate && !pre_approved.is_empty() {
                return Err(at(
                    "pre-approved actions need the level operate, since they operate".into(),
                ));
            }
            Ok(Entry {
                program: program.trim().to_string(),
                level,
                pre_approved,
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(List {
        programs,
        answer_within,
    })
}

/// Writes the list back as the file holds it.
pub fn render(list: &List) -> String {
    let programs: Vec<Value> = list
        .programs
        .iter()
        .map(|e| {
            let mut o = json!({ "program": e.program, "level": e.level.word() });
            if !e.pre_approved.is_empty() {
                o["pre_approved"] = e.pre_approved.iter().map(Approval::json).collect();
            }
            o
        })
        .collect();
    let mut file = json!({ "programs": programs });
    if let Some(s) = list.answer_within {
        file["answer_within"] = json!(s);
    }
    let mut text =
        serde_json::to_string_pretty(&file).expect("a list of strings always serialises");
    text.push('\n');
    text
}

/// Lists `program` at `level`, in place of any entry naming it the same
/// way, keeping what that entry pre-approved.
pub fn allow(entries: &mut Vec<Entry>, program: &str, level: Level) -> Result<(), String> {
    check(program)?;
    let program = program.trim().to_string();
    match entries.iter_mut().find(|e| same(&e.program, &program)) {
        Some(e) => {
            if level != Level::Operate && !e.pre_approved.is_empty() {
                return Err(format!(
                    "{} has pre-approved actions, which need operate; take them off first \
                     (horadric desktop unapprove)",
                    e.program
                ));
            }
            e.program = program;
            e.level = level;
        }
        None => entries.push(Entry {
            program,
            level,
            pre_approved: Vec::new(),
        }),
    }
    Ok(())
}

/// Adds a pre-approved action to the listed `program`, which must be at
/// operate. Says the entry it went on.
pub fn approve(entries: &mut [Entry], program: &str, approval: Approval) -> Result<String, String> {
    approval.check()?;
    let entry = entries
        .iter_mut()
        .find(|e| same(&e.program, program))
        .ok_or_else(|| {
            format!(
                "{} is not on the list; allow it first: horadric desktop allow \"{}\" operate",
                program.trim(),
                program.trim()
            )
        })?;
    if entry.level != Level::Operate {
        return Err(format!(
            "{} is listed to look only; pre-approved actions need operate",
            entry.program
        ));
    }
    if !entry.pre_approved.contains(&approval) {
        entry.pre_approved.push(approval);
    }
    Ok(entry.program.clone())
}

/// Takes the `n`th (from 1, as `list` numbers them) pre-approved action off
/// the listed `program`, and says what it was.
pub fn unapprove(entries: &mut [Entry], program: &str, n: usize) -> Result<String, String> {
    let entry = entries
        .iter_mut()
        .find(|e| same(&e.program, program))
        .ok_or_else(|| format!("{} is not on the list", program.trim()))?;
    if n == 0 || n > entry.pre_approved.len() {
        return Err(format!(
            "{} has {} pre-approved action(s); `horadric desktop list` numbers them",
            entry.program,
            entry.pre_approved.len()
        ));
    }
    Ok(entry.pre_approved.remove(n - 1).describe())
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The list, with no question asked.
    List(Level),
    /// The human, answering the approval question.
    Asked(Option<Level>),
    /// The human, answering a risky action's question.
    Risky(bool),
    /// A risky action let through by a pattern on the list, in words.
    PreApproved(String),
    /// Nobody answered within so many seconds, risky or not. On a machine
    /// with nobody at it this is not a refusal, and the log keeps the two
    /// apart.
    NoAnswer { risky: bool, secs: u64 },
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
        Source::PreApproved(pattern) => {
            format!(
                "operate from the list, pre-approved: {}",
                flat_line(&pattern)
            )
        }
        Source::NoAnswer { risky, secs } => format!(
            "{}no answer, nobody answered within {secs} s",
            if risky { "risky action, " } else { "" }
        ),
        Source::Unreadable => "the list could not be read, asking instead".into(),
        Source::Bypass => "not asked, the human had bypass on".into(),
        Source::BypassOn => "bypass turned on by the human".into(),
        Source::BypassOff(how) => format!("bypass off: {how}"),
    };
    let mut line = format!(
        "{utc}  {}  {how}  {}",
        flat_line(session),
        flat_line(program)
    );
    if !what.trim().is_empty() {
        line.push_str(&format!("  ({})", flat_line(what)));
    }
    line.push('\n');
    line
}

fn flat_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
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
            pre_approved: Vec::new(),
        }
    }

    fn programs(text: &str) -> Result<Vec<Entry>, String> {
        parse(text).map(|l| l.programs)
    }

    fn file(programs: &[Entry]) -> String {
        render(&List {
            programs: programs.to_vec(),
            answer_within: None,
        })
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
            programs(text),
            Ok(vec![
                entry(SYSMAC, Level::Operate),
                entry("notepad.exe", Level::Look)
            ])
        );
    }

    #[test]
    fn an_empty_file_is_an_empty_list() {
        assert_eq!(programs(""), Ok(vec![]));
        assert_eq!(programs("  \n"), Ok(vec![]));
        assert_eq!(programs(r#"{"programs": []}"#), Ok(vec![]));
        assert_eq!(programs("\u{feff}{\"programs\": []}"), Ok(vec![]));
    }

    #[test]
    fn anything_wrong_fails_the_whole_file() {
        assert!(programs("{").is_err());
        assert!(programs("{}").is_err());
        assert!(programs(r#"{"programs": [{"program": "a.exe"}]}"#).is_err());
        assert!(programs(r#"{"programs": [{"level": "look"}]}"#).is_err());
        assert!(programs(r#"{"programs": [{"program": " ", "level": "look"}]}"#).is_err());
        let two = r#"{"programs": [
            {"program": "a.exe", "level": "operate"},
            {"program": "b.exe", "level": "always"}
        ]}"#;
        let err = programs(two).unwrap_err();
        assert!(err.starts_with("program 2:"), "{err}");
    }

    #[test]
    fn there_is_no_level_for_risky_actions() {
        for word in ["risky", "all", "force", "transfer", "write", "none", ""] {
            assert!(Level::parse(word).is_err(), "{word}");
        }
        let risky = r#"{"programs": [{"program": "a.exe", "level": "operate", "risky": true}]}"#;
        // A stray field changes nothing: operate is the most there is.
        assert_eq!(programs(risky), Ok(vec![entry("a.exe", Level::Operate)]));
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
        assert!(programs(r#"{"programs": [{"program": "horadric", "level": "look"}]}"#).is_err());
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
        assert_eq!(programs(&file(&list)), Ok(list));
        assert_eq!(programs(&file(&[])), Ok(vec![]));
    }

    fn approval(action: Kind) -> Approval {
        Approval {
            action,
            window: None,
            control: None,
            name: None,
            key: None,
            text: None,
            session: None,
        }
    }

    fn act(kind: Kind, window: &str, control: &str, name: &str, key: &str) -> Act {
        Act {
            kind: Some(kind),
            window: window.into(),
            control: control.into(),
            name: name.into(),
            key: key.into(),
            texts: Vec::new(),
        }
    }

    #[test]
    fn a_glob_matches_whole_text_ignoring_case_and_spaces() {
        assert!(glob("_Delete", "_delete"));
        assert!(glob("*Delete", "_Delete"));
        assert!(glob("Transfer from*", "Transfer  From Controller"));
        assert!(glob("*", ""));
        assert!(glob("a*b*c", "aXXbYYc"));
        assert!(glob("a*b*c", "abbc"));
        assert!(!glob("Delete", "_Delete"));
        assert!(!glob("Transfer", "Transfer..."));
        assert!(!glob("Transfer from*", "Transfer to Controller"));
        assert!(!glob("a*b", "ac"));
    }

    #[test]
    fn pre_approved_actions_are_read_with_the_program() {
        let text = r#"{
            "answer_within": 20,
            "programs": [{
                "program": "SysmacStudio.exe",
                "level": "operate",
                "pre_approved": [
                    { "action": "click", "control": "Button", "name": "_Delete", "window": "Open Project" },
                    { "action": "press", "key": "Enter", "window": "Import solution", "session": "OM01347*" }
                ]
            }]
        }"#;
        let list = parse(text).unwrap();
        assert_eq!(list.answer_within, Some(20));
        let pre = &list.programs[0].pre_approved;
        assert_eq!(pre.len(), 2);
        assert_eq!(
            pre[0].describe(),
            r#"click Button "_Delete" in window "Open Project""#
        );
        assert_eq!(
            pre[1].describe(),
            r#"press Enter in window "Import solution" for sessions "OM01347*""#
        );
        assert_eq!(parse(&render(&list)), Ok(list));
    }

    #[test]
    fn a_pre_approval_must_name_its_action() {
        let one = |a: &str| {
            parse(&format!(
                r#"{{"programs": [{{"program": "a.exe", "level": "operate", "pre_approved": [{a}]}}]}}"#
            ))
        };
        assert!(one(r#"{"action": "click", "name": "OK"}"#).is_ok());
        // Nothing to match on, or only wildcards: that would be every click.
        assert!(one(r#"{"action": "click"}"#).is_err());
        assert!(one(r#"{"action": "click", "name": "*"}"#).is_err());
        assert!(one(r#"{"action": "click", "window": "x", "name": " * "}"#).is_err());
        assert!(one(r#"{"action": "press", "name": "OK"}"#).is_err());
        assert!(one(r#"{"action": "press", "key": "Enter"}"#).is_ok());
        assert!(one(r#"{"action": "drag", "name": "OK"}"#).is_err());
        assert!(one(r#"{"name": "OK"}"#).is_err());
        // A misspelt field would match more than was meant.
        let err = one(r#"{"action": "click", "name": "OK", "windw": "x"}"#).unwrap_err();
        assert!(err.contains("windw"), "{err}");
        assert!(one(r#"{"action": "click", "name": 3}"#).is_err());
        // Pre-approving needs operate.
        assert!(parse(
            r#"{"programs": [{"program": "a.exe", "level": "look", "pre_approved": [{"action": "click", "name": "OK"}]}]}"#
        )
        .is_err());
        assert!(parse(r#"{"programs": [], "answer_within": "soon"}"#).is_err());
    }

    #[test]
    fn a_pre_approval_matches_only_its_action() {
        let mut delete = approval(Kind::Click);
        delete.name = Some("_Delete".into());
        delete.control = Some("Button".into());
        delete.window = Some("Open Project".into());
        let hit = act(Kind::Click, "Open Project", "Button", "_Delete", "");
        assert!(delete.matches(&hit, "any session"));
        // Another window, control, name or kind of action is not it.
        for other in [
            act(Kind::Click, "Sysmac Studio", "Button", "_Delete", ""),
            act(Kind::Click, "Open Project", "MenuItem", "_Delete", ""),
            act(Kind::Click, "Open Project", "Button", "Delete All", ""),
            act(Kind::Press, "Open Project", "Button", "_Delete", "Delete"),
            Act::default(),
        ] {
            assert!(!delete.matches(&other, "s"), "{other:?}");
        }

        let mut enter = approval(Kind::Press);
        enter.key = Some("Enter".into());
        enter.window = Some("Import solution".into());
        let import = act(Kind::Press, "Import solution", "", "File name:", "enter");
        assert!(enter.matches(&import, "s"));
        assert!(!enter.matches(&act(Kind::Press, "Transfer", "", "", "Enter"), "s"));
    }

    #[test]
    fn a_pre_approval_can_be_for_one_task_and_one_question() {
        let mut upload = approval(Kind::Click);
        upload.name = Some("Transfer from Controller".into());
        upload.session = Some("OM01347 backup*".into());
        let a = act(
            Kind::Click,
            "Synchronization",
            "Button",
            "Transfer From Controller",
            "",
        );
        assert!(upload.matches(&a, "OM01347 backup V2_7"));
        assert!(!upload.matches(&a, "OM01347 V2_7 download"));

        let mut yes = approval(Kind::Click);
        yes.name = Some("Yes".into());
        yes.text = Some("*delete the project*".into());
        let mut confirm = act(Kind::Click, "Sysmac Studio", "Button", "Yes", "");
        assert!(!yes.matches(&confirm, "s"));
        confirm.texts = vec!["Do you want to delete the project \"OM01347\"?".into()];
        assert!(yes.matches(&confirm, "s"));
        confirm.texts = vec!["Transfer to the Controller?".into()];
        assert!(!yes.matches(&confirm, "s"));
    }

    #[test]
    fn only_an_operate_entry_pre_approves() {
        let mut ok = approval(Kind::Click);
        ok.name = Some("OK".into());
        let mut e = entry("a.exe", Level::Operate);
        e.pre_approved.push(ok.clone());
        let a = act(Kind::Click, "w", "Button", "OK", "");
        assert_eq!(pre_approved(&e, &a, "s"), Some(&ok));
        e.level = Level::Look;
        assert_eq!(pre_approved(&e, &a, "s"), None);
    }

    #[test]
    fn approving_and_unapproving_change_one_entry() {
        let mut list = vec![entry(SYSMAC, Level::Operate), entry("notepad", Level::Look)];
        let mut del = approval(Kind::Click);
        del.name = Some("_Delete".into());
        assert_eq!(
            approve(&mut list, &SYSMAC.to_lowercase(), del.clone()),
            Ok(SYSMAC.into())
        );
        // The same twice is kept once.
        approve(&mut list, SYSMAC, del.clone()).unwrap();
        assert_eq!(list[0].pre_approved, vec![del.clone()]);
        assert!(approve(&mut list, "notepad", del.clone()).is_err());
        assert!(approve(&mut list, "calc.exe", del.clone()).is_err());
        assert!(approve(&mut list, SYSMAC, approval(Kind::Click)).is_err());
        // Allowing again keeps them, and will not lower it below operate.
        allow(&mut list, SYSMAC, Level::Operate).unwrap();
        assert_eq!(list[0].pre_approved.len(), 1);
        assert!(allow(&mut list, SYSMAC, Level::Look).is_err());
        assert!(unapprove(&mut list, SYSMAC, 2).is_err());
        assert!(unapprove(&mut list, SYSMAC, 0).is_err());
        assert_eq!(
            unapprove(&mut list, SYSMAC, 1),
            Ok(r#"click "_Delete""#.into())
        );
        assert!(list[0].pre_approved.is_empty());
    }

    #[test]
    fn a_question_waits_within_what_the_call_allows() {
        let mut list = List::default();
        assert_eq!(list.answer_within(), ANSWER_WITHIN);
        list.answer_within = Some(1);
        assert_eq!(list.answer_within(), 10);
        list.answer_within = Some(30);
        assert_eq!(list.answer_within(), 30);
        list.answer_within = Some(600);
        assert_eq!(list.answer_within(), 80);
    }

    #[test]
    fn the_log_tells_no_answer_from_a_refusal() {
        let at = "2026-11-18T10:35:03Z";
        let risky = Source::NoAnswer {
            risky: true,
            secs: 60,
        };
        let none = log_line(at, "s", risky, "a.exe", "");
        assert_eq!(
            none,
            format!("{at}  s  risky action, no answer, nobody answered within 60 s  a.exe\n")
        );
        assert!(!none.contains("refused"));
        let plain = Source::NoAnswer {
            risky: false,
            secs: 20,
        };
        assert!(log_line(at, "s", plain, "a.exe", "")
            .contains("  no answer, nobody answered within 20 s  "));
        assert_eq!(
            log_line(
                at,
                "s",
                Source::PreApproved(r#"click "_Delete""#.into()),
                "a.exe",
                "click Button \"_Delete\""
            ),
            format!(
                "{at}  s  operate from the list, pre-approved: click \"_Delete\"  a.exe  \
                 (click Button \"_Delete\")\n"
            )
        );
    }
}
