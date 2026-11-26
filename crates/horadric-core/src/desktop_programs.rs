//! The desktop programs the human allows ahead of time, so a session can
//! look at or operate them without the approval question each new session
//! would otherwise ask. There are two lists of the same form, both named
//! `takeover-programs.json` (`desktop-programs.json` before the tools were
//! renamed, see [`read_first`]), edited with `horadric takeover
//! allow|deny|approve|list`.
//!
//! The one beside the app's state (`%APPDATA%\Horadric`) is the user's. Any
//! program of the user can write it, a session's agent among them, so it
//! only ever stands in for the approval question of a program that does
//! not run as administrator: no entry in it covers an elevated program,
//! and its `pre_approved` patterns are not used ([`user_list`]).
//!
//! The one in `%ProgramData%\Horadric` is the administrators'. Only an
//! elevated shell can write it, and whoever reads it checks that first
//! ([`admins_only`]). It lists the programs that run as administrator
//! (`"elevated": true`), and holds every pre-approved risky action, of
//! any program.
//!
//! ```json
//! {
//!   "answer_within": 60,
//!   "programs": [
//!     {
//!       "program": "C:\Program Files\Acme\Acme Studio\AcmeStudio.exe",
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
//! name or the key, and optionally the window, the control and a dialog's
//! text). Every such action is logged with the pattern that let it
//! through. There is no level that covers risky
//! actions wholesale.
//!
//! `answer_within` is how many seconds a question waits for the human
//! before the session is told nobody answered (60 when not set).

use serde_json::{json, Value};

/// The file's name, in `%APPDATA%\Horadric` (`Horadric-dev` for a dev
/// instance) for the user's list, and in [`ADMIN_FOLDER`] for the
/// administrators'.
pub const FILE: &str = "takeover-programs.json";

/// What [`FILE`], [`LOG_FILE`] and [`ADMIN_LOG_FILE`] were named while the
/// tools were `desktop_`. A machine that runs unattended depends on its
/// lists, so the old names are moved or still read, never dropped.
pub const OLD_FILE: &str = "desktop-programs.json";
pub const OLD_LOG_FILE: &str = "desktop.log";
pub const OLD_ADMIN_LOG_FILE: &str = "desktop-elevated.log";

/// Whether a file under its old name is moved to its new one: only when
/// nothing has the new name, so no file is ever written over.
pub fn moves(new_there: bool, old_there: bool) -> bool {
    old_there && !new_there
}

/// Which of a file's two names is read, where it can not be moved first
/// (only an elevated process can rename in the administrators' folder).
/// `open` answers none for a name nothing has. The new name is the one
/// read when it is there; the old one only when the new is absent, and
/// held to the same checks, since `open` is the same. A new file that is
/// there but refused stops the reading: it is never passed over for the
/// old one.
pub fn read_first<'a, T, E>(
    new: &'a str,
    old: &'a str,
    mut open: impl FnMut(&str) -> Result<Option<T>, E>,
) -> Result<Option<(&'a str, T)>, (&'a str, E)> {
    for name in [new, old] {
        match open(name) {
            Ok(Some(found)) => return Ok(Some((name, found))),
            Ok(None) => {}
            Err(why) => return Err((name, why)),
        }
    }
    Ok(None)
}

/// The folder under `%ProgramData%` that holds the administrators' list
/// and the elevated helper's log. One folder for the installed Horadric
/// and a dev instance alike: the elevated helper must not take its place
/// from the environment, which the session that starts it sets.
pub const ADMIN_FOLDER: &str = "Horadric";

/// The elevated helper's own log, in [`ADMIN_FOLDER`], where the session
/// can not rewrite it.
pub const ADMIN_LOG_FILE: &str = "takeover-elevated.log";

/// Whether sessions may take over programs on the human's own desktop at
/// all, chosen in the tray menu and kept with the app's settings. Off is
/// there because a question is itself a cost: without it any session can
/// put one on the human's screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Every call is refused and the tools are not listed.
    Off,
    /// The human is asked before each program, and before each risky
    /// action. A word this build does not know reads as this one, so a
    /// state file from a newer build still loads.
    #[default]
    #[serde(other)]
    Ask,
}

/// What a session hears of any call while takeover is off.
pub const OFF: &str =
    "Takeover is off. The human turns it on from Horadric's tray menu (Takeover).";

impl Mode {
    /// The word on the wire and in the state file.
    pub fn word(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Ask => "ask",
        }
    }

    /// Reads [`Mode::word`]. Anything else, and no answer at all, is ask:
    /// an app that does not answer in time must not hide the tools.
    pub fn from_word(word: Option<&str>) -> Mode {
        match word {
            Some("off") => Mode::Off,
            _ => Mode::Ask,
        }
    }

    pub fn is_ask(&self) -> bool {
        *self == Mode::Ask
    }
}

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
    /// The entry is for the program run as administrator, and for that
    /// only. Such an entry counts only in the administrators' list.
    pub elevated: bool,
    /// Risky actions the human said yes to ahead of time. Used only from
    /// the administrators' list.
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
    /// Once the session's name, for a pattern meant for one task. A
    /// session can set its own name, and the elevated helper has only the
    /// session's word for it, so it is no longer matched: a pattern that
    /// carries one matches nothing, rather than every session.
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

    /// Whether `act` is this one. Never, for a pattern narrowed to a
    /// session: see [`Approval::session`].
    pub fn matches(&self, act: &Act) -> bool {
        let fits =
            |pattern: &Option<String>, text: &str| pattern.as_deref().is_none_or(|p| glob(p, text));
        self.session.is_none()
            && act.kind == Some(self.action)
            && fits(&self.window, &act.window)
            && fits(&self.control, &act.control)
            && fits(&self.name, &act.name)
            && fits(&self.key, &act.key)
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
            said.push_str(&format!(
                " for sessions {s:?} (never used: sessions are no longer matched)"
            ));
        }
        said
    }

    /// The command that pre-approves this for `program`, as typed in an
    /// elevated shell.
    pub fn command(&self, program: &str, elevated: bool) -> String {
        let mut said = String::from("horadric takeover approve");
        if elevated {
            said.push_str(" --elevated");
        }
        said.push_str(&format!(" {} {}", quoted(program), self.action.word()));
        if let Some(named) = self.name.as_ref().or(self.key.as_ref()) {
            said.push_str(&format!(" {}", quoted(named)));
        }
        for (flag, v) in [
            ("--window", &self.window),
            ("--control", &self.control),
            ("--text", &self.text),
        ] {
            if let Some(v) = v {
                said.push_str(&format!(" {flag} {}", quoted(v)));
            }
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
pub fn pre_approved<'a>(entry: &'a Entry, act: &Act) -> Option<&'a Approval> {
    if entry.level != Level::Operate {
        return None;
    }
    entry.pre_approved.iter().find(|a| a.matches(act))
}

/// Text as one argument of a command line.
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\\\""))
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

/// The entry that covers the program at `path` named `exe`, run as
/// administrator or not as `elevated` says, if any. One listed by path is
/// surer than one listed by name, so it wins whatever the order; otherwise
/// the first in the file. Horadric's own programs are never covered.
pub fn find<'a>(entries: &'a [Entry], path: &str, exe: &str, elevated: bool) -> Option<&'a Entry> {
    if is_horadric(exe) || is_horadric(path) {
        return None;
    }
    let hits = || {
        entries
            .iter()
            .filter(|e| e.elevated == elevated && matches(&e.program, path, exe))
    };
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
            let elevated = match item.get("elevated") {
                None | Some(Value::Null) => false,
                Some(Value::Bool(b)) => *b,
                Some(_) => return Err(at("\"elevated\" is true or false".into())),
            };
            Ok(Entry {
                program: program.trim().to_string(),
                level,
                elevated,
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
            if e.elevated {
                o["elevated"] = json!(true);
            }
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

/// Lists `program` at `level`, run as administrator or not as `elevated`
/// says, in place of any entry naming it the same way, keeping what that
/// entry pre-approved.
pub fn allow(
    entries: &mut Vec<Entry>,
    program: &str,
    level: Level,
    elevated: bool,
) -> Result<(), String> {
    check(program)?;
    let program = program.trim().to_string();
    match entries
        .iter_mut()
        .find(|e| e.elevated == elevated && same(&e.program, &program))
    {
        Some(e) => {
            if level != Level::Operate && !e.pre_approved.is_empty() {
                return Err(format!(
                    "{} has pre-approved actions, which need operate; take them off first \
                     (horadric takeover unapprove)",
                    e.program
                ));
            }
            e.program = program;
            e.level = level;
        }
        None => entries.push(Entry {
            program,
            level,
            elevated,
            pre_approved: Vec::new(),
        }),
    }
    Ok(())
}

/// Adds a pre-approved action to the listed `program`, which must be at
/// operate. Says the entry it went on.
pub fn approve(
    entries: &mut [Entry],
    program: &str,
    elevated: bool,
    approval: Approval,
) -> Result<String, String> {
    approval.check()?;
    if approval.session.is_some() {
        return Err(
            "a pre-approved action can no longer be narrowed to a session: a session sets its \
             own name, so the name proves nothing. Leave \"session\" out"
                .into(),
        );
    }
    let entry = entries
        .iter_mut()
        .find(|e| e.elevated == elevated && same(&e.program, program))
        .ok_or_else(|| {
            format!(
                "{} is not on the administrators' list; allow it there first: horadric takeover \
                 allow {} \"{}\" operate",
                program.trim(),
                if elevated { "--elevated" } else { "--admin" },
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
pub fn unapprove(
    entries: &mut [Entry],
    program: &str,
    elevated: bool,
    n: usize,
) -> Result<String, String> {
    let entry = entries
        .iter_mut()
        .find(|e| e.elevated == elevated && same(&e.program, program))
        .ok_or_else(|| format!("{} is not on the list", program.trim()))?;
    if n == 0 || n > entry.pre_approved.len() {
        return Err(format!(
            "{} has {} pre-approved action(s); `horadric takeover list` numbers them",
            entry.program,
            entry.pre_approved.len()
        ));
    }
    Ok(entry.pre_approved.remove(n - 1).describe())
}

/// Takes `program` off the list, run as administrator or not as
/// `elevated` says: every entry naming it the same way, or, given only a
/// name, every entry whose path ends in that name too, since "deny Acme"
/// means none of it. Says how many went.
pub fn deny(entries: &mut Vec<Entry>, program: &str, elevated: bool) -> usize {
    let before = entries.len();
    let by_name = !is_path(program);
    entries.retain(|e| {
        let gone = e.elevated == elevated
            && (same(&e.program, program)
                || (by_name && e.by_path() && bare(file_name(&e.program)) == bare(program)));
        !gone
    });
    before - entries.len()
}

/// The log of what was allowed and refused, beside the list, so the human
/// can see afterwards which grants came from the list and which from a
/// question.
pub const LOG_FILE: &str = "takeover.log";

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

/// Something on the user's list that the user's list can no longer hold,
/// with the command that puts it on the administrators'.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Misplaced {
    /// What it is, in words.
    pub what: String,
    /// The command to run from an elevated shell.
    pub command: String,
}

/// The user's list as it counts: without the entries for programs that
/// run as administrator and without any pre-approved action, each of
/// which is told back with its command. Any program of the user can write
/// this file, so it may only answer what the user could answer anyway:
/// the approval question of a program that is not elevated.
pub fn user_list(read: List) -> (List, Vec<Misplaced>) {
    let mut misplaced = Vec::new();
    let mut programs = Vec::new();
    for mut e in read.programs {
        if e.elevated {
            misplaced.push(Misplaced {
                what: format!("{} as administrator", e.program),
                command: allow_command(&e.program, e.level, true),
            });
            continue;
        }
        for a in e.pre_approved.drain(..) {
            misplaced.push(Misplaced {
                what: format!("pre-approved in {}: {}", e.program, a.describe()),
                command: a.command(&e.program, false),
            });
        }
        programs.push(e);
    }
    let list = List {
        programs,
        answer_within: read.answer_within,
    };
    (list, misplaced)
}

/// The command that lists `program` on the administrators' list.
pub fn allow_command(program: &str, level: Level, elevated: bool) -> String {
    format!(
        "horadric takeover allow {} {} {}",
        if elevated { "--elevated" } else { "--admin" },
        quoted(program),
        level.word()
    )
}

/// How far the lists let a program in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub level: Option<Level>,
    /// The program runs as administrator and only the user's list names
    /// it, which no longer counts: the command that lists it where it
    /// does.
    pub moved: Option<String>,
}

/// How far the two lists let in the program at `path` named `exe`. Run as
/// administrator, only the administrators' list speaks. Run normally, both
/// do, and the more generous wins: an administrator's word is worth at
/// least the user's.
pub fn level_for(user: &[Entry], admin: &[Entry], path: &str, exe: &str, elevated: bool) -> Listed {
    if elevated {
        let level = find(admin, path, exe, true).map(|e| e.level);
        let moved = match level {
            Some(_) => None,
            None => find(user, path, exe, false).map(|e| {
                let program = if path.trim().is_empty() {
                    &e.program
                } else {
                    path
                };
                allow_command(program, e.level, true)
            }),
        };
        return Listed { level, moved };
    }
    let level = find(user, path, exe, false)
        .map(|e| e.level)
        .max(find(admin, path, exe, false).map(|e| e.level));
    Listed { level, moved: None }
}

/// The pattern on the administrators' list that lets `act` go ahead
/// unasked in the program at `path`, in words. The user's list is never
/// asked: a session could write itself a pattern there.
pub fn pre_approval(
    admin: &[Entry],
    path: &str,
    exe: &str,
    elevated: bool,
    act: &Act,
) -> Option<String> {
    let entry = find(admin, path, exe, elevated)?;
    pre_approved(entry, act).map(Approval::describe)
}

/// Programs that host other things in their windows, so that one yes for
/// the exe would cover every console, every store app or the whole shell.
const HOSTS: [&str; 16] = [
    "applicationframehost",
    "mmc",
    "explorer",
    "rundll32",
    "dllhost",
    "conhost",
    "openconsole",
    "windowsterminal",
    "wwahost",
    "svchost",
    "msiexec",
    "java",
    "javaw",
    "python",
    "pythonw",
    "wscript",
];

/// Whether `exe` is a program that shows other programs' or documents'
/// windows as its own.
pub fn is_host(exe: &str) -> bool {
    HOSTS.contains(&bare(file_name(exe)).as_str())
}

/// What tells one thing a host shows from another, for the grant: the
/// window's AppUserModelID when it has one (a packaged app in
/// ApplicationFrameHost), the console's name for mmc, whose loaded
/// console is not cheaply read, and otherwise the window itself. Empty for
/// a program that is no host.
pub fn hosted(exe: &str, app_id: Option<&str>, title: &str, hwnd: isize) -> String {
    if !is_host(exe) {
        return String::new();
    }
    if let Some(id) = app_id.map(str::trim).filter(|id| !id.is_empty()) {
        return format!("app {}", id.to_lowercase());
    }
    if bare(file_name(exe)) == "mmc" {
        // "Console1 - [Console Root\Services]" is the console "Console1".
        let root = title.split(" - [").next().unwrap_or(title).trim();
        if !root.is_empty() {
            return format!("console {}", root.to_lowercase());
        }
    }
    format!("window {hwnd}")
}

/// What a grant is kept by: the session, the program by its full path
/// (its exe name only when Windows will not tell the path), whether it
/// runs as administrator, and for a host what it shows. Two programs of
/// one name in two folders are two programs, and a file named "x as
/// administrator" is not an elevated x.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GrantKey {
    pub session: String,
    pub program: String,
    pub elevated: bool,
    pub hosted: String,
}

impl GrantKey {
    pub fn new(session: &str, path: &str, exe: &str, elevated: bool, hosted: &str) -> GrantKey {
        let program = if path.trim().is_empty() {
            exe.trim().to_lowercase()
        } else {
            normal(path)
        };
        GrantKey {
            session: session.to_string(),
            program,
            elevated,
            hosted: hosted.to_string(),
        }
    }
}

/// How long a refused question is not asked again: the session is told
/// no at once instead, so it can not have the human asked over and over.
pub const REFUSED_FOR: std::time::Duration = std::time::Duration::from_secs(120);

/// The most questions that wait for the human per session. One more is
/// refused unasked.
pub const MOST_WAITING: usize = 4;

/// Which of the two questions was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Question {
    /// Whether the session may use the program at all, or operate it.
    Approve,
    /// A risky action.
    Risky,
}

/// The questions the human refused, or nobody answered, and when.
#[derive(Debug)]
pub struct Refusals<K> {
    at: std::collections::HashMap<(K, Question), std::time::Instant>,
}

impl<K> Default for Refusals<K> {
    fn default() -> Self {
        Refusals {
            at: std::collections::HashMap::new(),
        }
    }
}

impl<K: std::hash::Hash + Eq + Clone> Refusals<K> {
    /// Keeps that `question` about `key` was refused at `now`.
    pub fn note(&mut self, key: &K, question: Question, now: std::time::Instant) {
        self.at.retain(|_, at| now.duration_since(*at) < REFUSED_FOR);
        self.at.insert((key.clone(), question), now);
    }

    /// How long `question` about `key` is still not asked again, none
    /// when it may be asked.
    pub fn left(
        &self,
        key: &K,
        question: Question,
        now: std::time::Instant,
    ) -> Option<std::time::Duration> {
        let at = self.at.get(&(key.clone(), question))?;
        REFUSED_FOR
            .checked_sub(now.duration_since(*at))
            .filter(|left| !left.is_zero())
    }

    /// The human said yes after all, in a question asked once the hold
    /// was over.
    pub fn clear(&mut self, key: &K, question: Question) {
        self.at.remove(&(key.clone(), question));
    }
}

/// What a session is told when its question is not asked again yet.
pub fn refused_text(question: Question, program: &str, left: std::time::Duration) -> String {
    let what = match question {
        Question::Approve => "using",
        Question::Risky => "a risky action in",
    };
    format!(
        "the human was asked about {what} {program} less than {} s ago and did not say yes, so \
         they are not asked again for another {} s and nothing was done. Do not try again before \
         then: say in the chat what you need and why",
        REFUSED_FOR.as_secs(),
        left.as_secs().max(1)
    )
}

/// What a session is told when too many of its questions wait already.
pub fn crowded_text(program: &str) -> String {
    format!(
        "{MOST_WAITING} questions from this session already wait for the human, so this one \
         about {program} was not asked and nothing was done. Wait for their answers"
    )
}

/// One entry of an access list, as Windows keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ace {
    /// 0 allows, 1 denies; Windows has more kinds.
    pub kind: u8,
    pub flags: u8,
    pub mask: u32,
    /// Whom it is for, as `S-1-5-32-544`. Empty for a kind not read.
    pub sid: String,
}

/// Who owns a file or folder and who may do what with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub owner: Option<String>,
    /// None when the file has no access list at all, which lets everyone
    /// do everything.
    pub dacl: Option<Vec<Ace>>,
}

const SID_SYSTEM: &str = "S-1-5-18";
const SID_ADMINISTRATORS: &str = "S-1-5-32-544";
const SID_USERS: &str = "S-1-5-32-545";
/// The service that installs Windows' own parts.
const SID_TRUSTED_INSTALLER: &str =
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";

/// Every right that changes a file or folder, its content, its name or
/// who may touch it: write data, append, write extended attributes,
/// delete a child, write attributes, delete, write the access list, take
/// ownership, and the generic write and all.
const WRITES: u32 =
    0x2 | 0x4 | 0x10 | 0x40 | 0x100 | 0x1_0000 | 0x4_0000 | 0x8_0000 | 0x4000_0000 | 0x1000_0000;
const FULL_CONTROL: u32 = 0x001F_01FF;
/// Read and execute, with reading the access list and waiting on it.
const READ_ONLY: u32 = 0x0012_00A9;

const SE_DACL_PRESENT: u16 = 0x0004;
const SE_DACL_PROTECTED: u16 = 0x1000;
const SE_SELF_RELATIVE: u16 = 0x8000;
const ACE_ALLOWED: u8 = 0;
const ACE_DENIED: u8 = 1;
/// Files and folders made inside inherit the entry.
const INHERIT_BOTH: u8 = 0x1 | 0x2;

fn administrators_own(sid: &str) -> bool {
    [SID_SYSTEM, SID_ADMINISTRATORS, SID_TRUSTED_INSTALLER].contains(&sid)
}

/// Whether only administrators (and Windows itself) could have written
/// what `d` guards, or why not. The owner counts, since an owner may
/// always rewrite the access list. Entries that deny are not counted on,
/// and an entry of a kind this does not read fails it, so anything not
/// understood is taken for unsafe.
pub fn admins_only(d: &Descriptor) -> Result<(), String> {
    match d.owner.as_deref() {
        Some(owner) if administrators_own(owner) => {}
        Some(owner) => {
            return Err(format!(
                "it is owned by {owner}, not by the administrators, and an owner can always \
                 change who may write"
            ))
        }
        None => return Err("it has no owner Horadric can read".into()),
    }
    let Some(dacl) = &d.dacl else {
        return Err("it has no access list, so everyone may write it".into());
    };
    for ace in dacl {
        match ace.kind {
            ACE_DENIED => {}
            ACE_ALLOWED if administrators_own(&ace.sid) || ace.mask & WRITES == 0 => {}
            ACE_ALLOWED => {
                return Err(format!(
                    "{} may write it, who is not the administrators",
                    ace.sid
                ))
            }
            kind => {
                return Err(format!(
                    "its access list has an entry of kind {kind}, which Horadric does not read"
                ))
            }
        }
    }
    Ok(())
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The SID that starts `b`, as text.
fn sid_text(b: &[u8]) -> Option<String> {
    let (revision, count) = (*b.first()?, usize::from(*b.get(1)?));
    let authority = b
        .get(2..8)?
        .iter()
        .fold(0u64, |a, &byte| (a << 8) | u64::from(byte));
    let mut text = format!("S-{revision}-{authority}");
    for i in 0..count {
        text.push_str(&format!("-{}", u32_at(b, 8 + 4 * i)?));
    }
    Some(text)
}

/// A SID as Windows keeps it.
fn sid_bytes(text: &str) -> Vec<u8> {
    let parts: Vec<u64> = text
        .split('-')
        .skip(1)
        .map(|p| p.parse().expect("a SID written here"))
        .collect();
    let mut b = vec![parts[0] as u8, (parts.len() - 2) as u8];
    b.extend_from_slice(&parts[1].to_be_bytes()[2..]);
    for sub in &parts[2..] {
        b.extend_from_slice(&(*sub as u32).to_le_bytes());
    }
    b
}

/// Reads a security descriptor in the self relative form Windows hands
/// out, with the owner and the access list asked for.
pub fn parse_descriptor(b: &[u8]) -> Result<Descriptor, String> {
    let short = || "the security descriptor is cut short".to_string();
    if b.first() != Some(&1) {
        return Err("the security descriptor is of a revision Horadric does not read".into());
    }
    let control = u16_at(b, 2).ok_or_else(short)?;
    if control & SE_SELF_RELATIVE == 0 {
        return Err("the security descriptor is not self relative".into());
    }
    let owner_at = u32_at(b, 4).ok_or_else(short)? as usize;
    let dacl_at = u32_at(b, 16).ok_or_else(short)? as usize;
    let owner = match owner_at {
        0 => None,
        at => Some(sid_text(b.get(at..).ok_or_else(short)?).ok_or_else(short)?),
    };
    if control & SE_DACL_PRESENT == 0 || dacl_at == 0 {
        return Ok(Descriptor { owner, dacl: None });
    }
    let acl = b.get(dacl_at..).ok_or_else(short)?;
    let count = u16_at(acl, 4).ok_or_else(short)?;
    let mut aces = Vec::new();
    let mut at = 8;
    for _ in 0..count {
        let ace = acl.get(at..).ok_or_else(short)?;
        let (kind, flags) = (*ace.first().ok_or_else(short)?, *ace.get(1).ok_or_else(short)?);
        let size = usize::from(u16_at(ace, 2).ok_or_else(short)?);
        if size < 4 || size > ace.len() {
            return Err(short());
        }
        let known = kind == ACE_ALLOWED || kind == ACE_DENIED;
        aces.push(Ace {
            kind,
            flags,
            mask: u32_at(ace, 4).unwrap_or(0),
            sid: match known {
                true => sid_text(ace.get(8..size).ok_or_else(short)?).ok_or_else(short)?,
                false => String::new(),
            },
        });
        at += size;
    }
    Ok(Descriptor {
        owner,
        dacl: Some(aces),
    })
}

/// The security descriptor of the administrators' folder (`folder`) or of
/// a file in it, self relative: owned by the administrators, full control
/// for them and for Windows, reading for the users, and nothing inherited
/// from the folder above, where every user may make folders.
pub fn admin_descriptor(folder: bool) -> Vec<u8> {
    let flags = if folder { INHERIT_BOTH } else { 0 };
    let mut aces = Vec::new();
    let mut count = 0u16;
    for (sid, mask) in [
        (SID_SYSTEM, FULL_CONTROL),
        (SID_ADMINISTRATORS, FULL_CONTROL),
        (SID_USERS, READ_ONLY),
    ] {
        let sid = sid_bytes(sid);
        aces.extend_from_slice(&[ACE_ALLOWED, flags]);
        aces.extend_from_slice(&((8 + sid.len()) as u16).to_le_bytes());
        aces.extend_from_slice(&mask.to_le_bytes());
        aces.extend_from_slice(&sid);
        count += 1;
    }
    let owner = sid_bytes(SID_ADMINISTRATORS);
    let (owner_at, group_at) = (20u32, 20 + owner.len() as u32);
    let dacl_at = group_at + owner.len() as u32;
    let mut b = vec![1, 0];
    b.extend_from_slice(&(SE_DACL_PRESENT | SE_DACL_PROTECTED | SE_SELF_RELATIVE).to_le_bytes());
    for offset in [owner_at, group_at, 0, dacl_at] {
        b.extend_from_slice(&offset.to_le_bytes());
    }
    b.extend_from_slice(&owner);
    b.extend_from_slice(&owner);
    // The list's revision, its size with this header, and how many entries.
    b.extend_from_slice(&[2, 0]);
    b.extend_from_slice(&((8 + aces.len()) as u16).to_le_bytes());
    b.extend_from_slice(&count.to_le_bytes());
    b.extend_from_slice(&[0, 0]);
    b.extend_from_slice(&aces);
    b
}

/// Why the administrators' list is not used, as the human reads it.
pub fn unsafe_text(what: &str, why: &str) -> String {
    format!(
        "{what} is not used: {why}. Only administrators may be able to write it. From an \
         elevated shell, delete it and list the programs again with horadric takeover allow"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const STUDIO: &str = r"C:\Program Files\Acme\Acme Studio\AcmeStudio.exe";

    fn entry(program: &str, level: Level) -> Entry {
        Entry {
            program: program.into(),
            level,
            elevated: false,
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
                { "program": "C:\\Program Files\\Acme\\Acme Studio\\AcmeStudio.exe", "level": "operate" },
                { "program": " notepad.exe ", "level": "Look" }
            ]
        }"#;
        assert_eq!(
            programs(text),
            Ok(vec![
                entry(STUDIO, Level::Operate),
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
        assert!(matches(STUDIO, STUDIO, "AcmeStudio.exe"));
        assert!(matches(
            "c:/program files/acme/acme studio/acmestudio.exe",
            STUDIO,
            "AcmeStudio.exe"
        ));
        assert!(!matches(
            STUDIO,
            r"C:\Users\x\Downloads\AcmeStudio.exe",
            "AcmeStudio.exe"
        ));
        assert!(!matches(STUDIO, "", "AcmeStudio.exe"));
    }

    #[test]
    fn a_name_matches_that_exe_anywhere() {
        let elsewhere = r"D:\Tools\AcmeStudio.exe";
        assert!(matches("AcmeStudio.exe", elsewhere, "AcmeStudio.exe"));
        assert!(matches("acmestudio", elsewhere, "AcmeStudio.exe"));
        assert!(matches("AcmeStudio", elsewhere, ""));
        assert!(!matches("Acme", elsewhere, "AcmeStudio.exe"));
        assert!(!matches("AcmeStudio.exe", "", "?"));
        assert!(!matches("notepad.exe", r"C:\Windows\notepad.exe.bak", ""));
    }

    #[test]
    fn a_path_entry_wins_over_a_name_entry() {
        let list = vec![
            entry("AcmeStudio.exe", Level::Operate),
            entry(STUDIO, Level::Look),
        ];
        assert_eq!(
            find(&list, STUDIO, "AcmeStudio.exe", false).map(|e| e.level),
            Some(Level::Look)
        );
        assert_eq!(
            find(&list, r"D:\AcmeStudio.exe", "AcmeStudio.exe", false).map(|e| e.level),
            Some(Level::Operate)
        );
        assert_eq!(find(&list, r"C:\Windows\notepad.exe", "notepad.exe", false), None);
    }

    #[test]
    fn horadric_is_never_covered() {
        let list = vec![
            entry("horadric.exe", Level::Operate),
            entry("horadricw", Level::Operate),
        ];
        assert_eq!(find(&list, r"C:\x\horadric.exe", "horadric.exe", false), None);
        assert_eq!(find(&list, r"C:\x\horadricw.exe", "HoradricW.exe", false), None);
        assert!(check("horadric.exe").is_err());
        assert!(check(r"C:\x\horadric-host-1.exe").is_err());
        assert!(programs(r#"{"programs": [{"program": "horadric", "level": "look"}]}"#).is_err());
        assert!(!is_horadric("horadrical.exe"));
    }

    #[test]
    fn allowing_replaces_the_same_program() {
        let mut list = vec![];
        allow(&mut list, STUDIO, Level::Look, false).unwrap();
        allow(&mut list, "notepad", Level::Look, false).unwrap();
        allow(&mut list, &STUDIO.to_uppercase(), Level::Operate, false).unwrap();
        allow(&mut list, "NOTEPAD.EXE", Level::Operate, false).unwrap();
        assert_eq!(
            list,
            vec![
                entry(&STUDIO.to_uppercase(), Level::Operate),
                entry("NOTEPAD.EXE", Level::Operate)
            ]
        );
        assert!(allow(&mut list, " ", Level::Look, false).is_err());
        assert!(allow(&mut list, "?", Level::Look, false).is_err());
        assert!(allow(&mut list, r"C:\dir\", Level::Look, false).is_err());
    }

    #[test]
    fn denying_a_name_takes_its_paths_too() {
        let mut list = vec![
            entry(STUDIO, Level::Operate),
            entry("AcmeStudio.exe", Level::Look),
            entry("notepad.exe", Level::Look),
        ];
        assert_eq!(deny(&mut list, "acmestudio", false), 2);
        assert_eq!(list, vec![entry("notepad.exe", Level::Look)]);
        assert_eq!(deny(&mut list, "calc.exe", false), 0);
        let mut list = vec![
            entry(STUDIO, Level::Operate),
            entry("AcmeStudio", Level::Look),
        ];
        assert_eq!(deny(&mut list, STUDIO, false), 1);
        assert_eq!(list, vec![entry("AcmeStudio", Level::Look)]);
    }

    #[test]
    fn the_log_says_where_a_grant_came_from() {
        let at = "2026-11-11T10:00:00Z";
        assert_eq!(
            log_line(
                at,
                "fix plc",
                Source::List(Level::Operate),
                STUDIO,
                "click\nOK"
            ),
            format!("{at}  fix plc  operate from the list, not asked  {STUDIO}  (click OK)\n")
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
            entry(STUDIO, Level::Operate),
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
                "program": "AcmeStudio.exe",
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
        assert!(pre[1]
            .describe()
            .starts_with(r#"press Enter in window "Import solution" for sessions "OM01347*" (never"#));
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
        assert!(delete.matches(&hit));
        // Another window, control, name or kind of action is not it.
        for other in [
            act(Kind::Click, "Acme Studio", "Button", "_Delete", ""),
            act(Kind::Click, "Open Project", "MenuItem", "_Delete", ""),
            act(Kind::Click, "Open Project", "Button", "Delete All", ""),
            act(Kind::Press, "Open Project", "Button", "_Delete", "Delete"),
            Act::default(),
        ] {
            assert!(!delete.matches(&other), "{other:?}");
        }

        let mut enter = approval(Kind::Press);
        enter.key = Some("Enter".into());
        enter.window = Some("Import solution".into());
        let import = act(Kind::Press, "Import solution", "", "File name:", "enter");
        assert!(enter.matches(&import));
        assert!(!enter.matches(&act(Kind::Press, "Transfer", "", "", "Enter")));
    }

    #[test]
    fn a_pre_approval_can_be_for_one_task_and_one_question() {
        let mut upload = approval(Kind::Click);
        upload.name = Some("Transfer from Controller".into());
        let a = act(
            Kind::Click,
            "Synchronization",
            "Button",
            "Transfer From Controller",
            "",
        );
        assert!(upload.matches(&a));
        // A session names itself, so a pattern for one session's name is
        // a pattern for whoever takes that name: it matches nobody.
        upload.session = Some("OM01347 backup*".into());
        assert!(!upload.matches(&a));
        upload.session = Some("*".into());
        assert!(!upload.matches(&a));
        let mut list = vec![entry(STUDIO, Level::Operate)];
        assert!(approve(&mut list, STUDIO, false, upload).is_err());

        let mut yes = approval(Kind::Click);
        yes.name = Some("Yes".into());
        yes.text = Some("*delete the project*".into());
        let mut confirm = act(Kind::Click, "Acme Studio", "Button", "Yes", "");
        assert!(!yes.matches(&confirm));
        confirm.texts = vec!["Do you want to delete the project \"OM01347\"?".into()];
        assert!(yes.matches(&confirm));
        confirm.texts = vec!["Transfer to the Controller?".into()];
        assert!(!yes.matches(&confirm));
    }

    #[test]
    fn only_an_operate_entry_pre_approves() {
        let mut ok = approval(Kind::Click);
        ok.name = Some("OK".into());
        let mut e = entry("a.exe", Level::Operate);
        e.pre_approved.push(ok.clone());
        let a = act(Kind::Click, "w", "Button", "OK", "");
        assert_eq!(pre_approved(&e, &a), Some(&ok));
        e.level = Level::Look;
        assert_eq!(pre_approved(&e, &a), None);
    }

    #[test]
    fn approving_and_unapproving_change_one_entry() {
        let mut list = vec![entry(STUDIO, Level::Operate), entry("notepad", Level::Look)];
        let mut del = approval(Kind::Click);
        del.name = Some("_Delete".into());
        assert_eq!(
            approve(&mut list, &STUDIO.to_lowercase(), false, del.clone()),
            Ok(STUDIO.into())
        );
        // The same twice is kept once.
        approve(&mut list, STUDIO, false, del.clone()).unwrap();
        assert_eq!(list[0].pre_approved, vec![del.clone()]);
        assert!(approve(&mut list, "notepad", false, del.clone()).is_err());
        assert!(approve(&mut list, "calc.exe", false, del.clone()).is_err());
        assert!(approve(&mut list, STUDIO, false, approval(Kind::Click)).is_err());
        // Allowing again keeps them, and will not lower it below operate.
        allow(&mut list, STUDIO, Level::Operate, false).unwrap();
        assert_eq!(list[0].pre_approved.len(), 1);
        assert!(allow(&mut list, STUDIO, Level::Look, false).is_err());
        assert!(unapprove(&mut list, STUDIO, false, 2).is_err());
        assert!(unapprove(&mut list, STUDIO, false, 0).is_err());
        assert_eq!(
            unapprove(&mut list, STUDIO, false, 1),
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

    fn elevated(program: &str, level: Level) -> Entry {
        Entry {
            elevated: true,
            ..entry(program, level)
        }
    }

    #[test]
    fn the_user_s_list_keeps_only_what_a_user_may_answer() {
        let mut del = approval(Kind::Click);
        del.name = Some("_Delete".into());
        del.window = Some("Open Project".into());
        let mut acme = entry(STUDIO, Level::Operate);
        acme.pre_approved.push(del);
        let read = List {
            programs: vec![
                acme,
                elevated("regedit.exe", Level::Look),
                entry("notepad", Level::Look),
            ],
            answer_within: Some(30),
        };
        let (list, misplaced) = user_list(read);
        assert_eq!(
            list.programs,
            vec![entry(STUDIO, Level::Operate), entry("notepad", Level::Look)]
        );
        assert_eq!(list.answer_within, Some(30));
        assert_eq!(misplaced.len(), 2);
        assert_eq!(
            misplaced[0].command,
            format!(
                r#"horadric takeover approve "{STUDIO}" click "_Delete" --window "Open Project""#
            )
        );
        assert_eq!(
            misplaced[1].command,
            r#"horadric takeover allow --elevated "regedit.exe" look"#
        );
        // Nothing to move, nothing told.
        let (_, none) = user_list(List {
            programs: vec![entry("notepad", Level::Operate)],
            answer_within: None,
        });
        assert!(none.is_empty());
    }

    #[test]
    fn an_elevated_program_is_listed_only_by_the_administrators() {
        let user = vec![entry("AcmeStudio.exe", Level::Operate)];
        let none: Vec<Entry> = Vec::new();
        // The user's list named it by name, which once covered its
        // elevated self. Now it says where to list it instead.
        let l = level_for(&user, &none, STUDIO, "AcmeStudio.exe", true);
        assert_eq!(l.level, None);
        assert_eq!(
            l.moved,
            Some(format!(
                r#"horadric takeover allow --elevated "{STUDIO}" operate"#
            ))
        );
        let admin = vec![elevated(STUDIO, Level::Look)];
        let l = level_for(&user, &admin, STUDIO, "AcmeStudio.exe", true);
        assert_eq!((l.level, l.moved), (Some(Level::Look), None));
        // The entry for the elevated program says nothing of the normal
        // one, and the other way round.
        let l = level_for(&none, &admin, STUDIO, "AcmeStudio.exe", false);
        assert_eq!((l.level, l.moved), (None, None));
        let plain = vec![entry(STUDIO, Level::Operate)];
        assert_eq!(
            level_for(&none, &plain, STUDIO, "AcmeStudio.exe", true).level,
            None
        );
    }

    #[test]
    fn a_normal_program_is_listed_by_either_and_the_more_generous_wins() {
        let user = vec![entry("notepad", Level::Look)];
        let admin = vec![entry("notepad.exe", Level::Operate)];
        let at = r"C:\Windows\notepad.exe";
        assert_eq!(
            level_for(&user, &[], at, "notepad.exe", false).level,
            Some(Level::Look)
        );
        assert_eq!(
            level_for(&user, &admin, at, "notepad.exe", false).level,
            Some(Level::Operate)
        );
        assert_eq!(level_for(&[], &[], at, "notepad.exe", false).level, None);
    }

    #[test]
    fn a_risky_action_is_pre_approved_only_on_the_administrators_list() {
        let mut ok = approval(Kind::Click);
        ok.name = Some("Yes".into());
        let mut plain = entry("a.exe", Level::Operate);
        plain.pre_approved.push(ok.clone());
        let mut high = elevated("a.exe", Level::Operate);
        high.pre_approved.push(ok);
        let a = act(Kind::Click, "w", "Button", "Yes", "");
        let admin = vec![plain];
        assert_eq!(
            pre_approval(&admin, r"C:\a.exe", "a.exe", false, &a),
            Some(r#"click "Yes""#.to_string())
        );
        // The pattern for the normal program is not one for the elevated.
        assert_eq!(pre_approval(&admin, r"C:\a.exe", "a.exe", true, &a), None);
        assert!(pre_approval(&[high], r"C:\a.exe", "a.exe", true, &a).is_some());
        assert_eq!(pre_approval(&[], r"C:\a.exe", "a.exe", false, &a), None);
    }

    #[test]
    fn the_elevated_flag_is_kept_in_the_file_and_tells_entries_apart() {
        let mut list = vec![];
        allow(&mut list, STUDIO, Level::Look, false).unwrap();
        allow(&mut list, STUDIO, Level::Operate, true).unwrap();
        assert_eq!(list.len(), 2);
        let text = file(&list);
        assert_eq!(text.matches("\"elevated\": true").count(), 1);
        assert_eq!(programs(&text), Ok(list.clone()));
        assert!(programs(
            r#"{"programs": [{"program": "a.exe", "level": "look", "elevated": "yes"}]}"#
        )
        .is_err());
        assert_eq!(deny(&mut list, STUDIO, true), 1);
        assert_eq!(list, vec![entry(STUDIO, Level::Look)]);
    }

    #[test]
    fn a_grant_is_kept_by_path_and_by_whether_it_runs_elevated() {
        let key = |path: &str, exe: &str, high: bool| GrantKey::new("s1", path, exe, high, "");
        assert_eq!(
            key(STUDIO, "AcmeStudio.exe", false),
            key(&STUDIO.to_uppercase().replace('\\', "/"), "x", false)
        );
        // The same name in another folder is another program.
        assert_ne!(
            key(STUDIO, "AcmeStudio.exe", false),
            key(r"C:\Users\x\Downloads\AcmeStudio.exe", "AcmeStudio.exe", false)
        );
        // Elevated is a field, not part of a name a file could carry.
        assert_ne!(
            key(STUDIO, "AcmeStudio.exe", false),
            key(STUDIO, "AcmeStudio.exe", true)
        );
        assert_ne!(
            key("", "a.exe as administrator", false),
            key("", "a.exe", true)
        );
        assert_eq!(key("", "A.EXE", false), key("", "a.exe ", false));
        assert_ne!(
            GrantKey::new("s1", STUDIO, "", false, ""),
            GrantKey::new("s2", STUDIO, "", false, "")
        );
    }

    #[test]
    fn a_host_is_granted_by_what_it_shows() {
        assert!(is_host("mmc.exe") && is_host(r"C:\Windows\explorer.exe"));
        assert!(is_host("ApplicationFrameHost.exe") && is_host("RUNDLL32"));
        assert!(!is_host("notepad.exe") && !is_host("AcmeStudio.exe"));
        assert_eq!(hosted("notepad.exe", Some("x"), "t", 7), "");
        assert_eq!(
            hosted(
                "ApplicationFrameHost.exe",
                Some("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"),
                "Calculator",
                7
            ),
            "app microsoft.windowscalculator_8wekyb3d8bbwe!app"
        );
        // A console is told by its name, whatever node is open in it.
        assert_eq!(
            hosted("mmc.exe", None, "Computer Management", 7),
            "console computer management"
        );
        assert_eq!(
            hosted("mmc.exe", Some(" "), r"Console1 - [Console Root\Services]", 7),
            "console console1"
        );
        assert_ne!(
            hosted("mmc.exe", None, "Event Viewer", 7),
            hosted("mmc.exe", None, "Computer Management", 7)
        );
        // Anything else a host shows is asked per window.
        assert_eq!(hosted("explorer.exe", None, "Documents", 7), "window 7");
        assert_ne!(
            hosted("explorer.exe", None, "Documents", 7),
            hosted("explorer.exe", None, "Documents", 8)
        );
    }

    #[test]
    fn a_refused_question_is_not_asked_again_for_two_minutes() {
        use std::time::{Duration, Instant};
        let mut refused = Refusals::default();
        let start = Instant::now();
        let s = Duration::from_secs;
        assert_eq!(refused.left(&"a", Question::Approve, start), None);
        refused.note(&"a", Question::Approve, start);
        assert_eq!(
            refused.left(&"a", Question::Approve, start),
            Some(REFUSED_FOR)
        );
        assert_eq!(
            refused.left(&"a", Question::Approve, start + s(100)),
            Some(s(20))
        );
        assert_eq!(refused.left(&"a", Question::Approve, start + s(120)), None);
        // Another program, and the other question, are not held by it.
        assert_eq!(refused.left(&"b", Question::Approve, start), None);
        assert_eq!(refused.left(&"a", Question::Risky, start), None);
        // An old one is dropped when the next is kept.
        refused.note(&"b", Question::Risky, start + s(500));
        assert_eq!(refused.at.len(), 1);
        refused.clear(&"b", Question::Risky);
        assert_eq!(refused.left(&"b", Question::Risky, start + s(500)), None);
        let told = refused_text(Question::Approve, "a.exe", s(20));
        assert!(told.contains("another 20 s") && told.contains("nothing was done"));
        assert!(crowded_text("a.exe").contains("4 questions"));
    }

    #[test]
    fn the_administrators_descriptor_reads_back_as_theirs_alone() {
        for folder in [true, false] {
            let d = parse_descriptor(&admin_descriptor(folder)).unwrap();
            assert_eq!(d.owner.as_deref(), Some(SID_ADMINISTRATORS));
            let dacl = d.dacl.clone().unwrap();
            assert_eq!(
                dacl.iter().map(|a| a.sid.as_str()).collect::<Vec<_>>(),
                vec![SID_SYSTEM, SID_ADMINISTRATORS, SID_USERS]
            );
            assert_eq!(dacl[2].mask, READ_ONLY);
            assert!(dacl.iter().all(|a| a.flags == if folder { 3 } else { 0 }));
            assert_eq!(admins_only(&d), Ok(()));
        }
        // A descriptor cut anywhere fails rather than reading as safe.
        let whole = admin_descriptor(true);
        for cut in 0..whole.len() {
            assert!(parse_descriptor(&whole[..cut]).is_err(), "{cut}");
        }
    }

    #[test]
    fn a_file_a_user_could_have_written_is_refused() {
        let safe = parse_descriptor(&admin_descriptor(false)).unwrap();
        let user = "S-1-5-21-1-2-3-1001";
        // The user owns it: an owner rewrites the access list at will.
        let mut d = safe.clone();
        d.owner = Some(user.into());
        assert!(admins_only(&d).unwrap_err().contains("owned by"));
        d.owner = None;
        assert!(admins_only(&d).is_err());
        // No access list at all.
        let mut d = safe.clone();
        d.dacl = None;
        assert!(admins_only(&d).unwrap_err().contains("everyone"));
        // Each right that writes, for someone who is no administrator.
        for mask in [
            0x2, 0x4, 0x10, 0x40, 0x100, 0x1_0000, 0x4_0000, 0x8_0000, 0x4000_0000, 0x1000_0000,
            FULL_CONTROL,
        ] {
            for sid in [user, SID_USERS, "S-1-1-0", "S-1-5-11", "S-1-3-0"] {
                let mut d = safe.clone();
                d.dacl.as_mut().unwrap().push(Ace {
                    kind: 0,
                    flags: 0,
                    mask,
                    sid: sid.into(),
                });
                assert!(admins_only(&d).is_err(), "{sid} {mask:#x}");
            }
        }
        // Reading is fine for anyone, and a deny entry is never leaned on.
        let mut d = safe.clone();
        let dacl = d.dacl.as_mut().unwrap();
        dacl.push(Ace {
            kind: 0,
            flags: 0,
            mask: READ_ONLY,
            sid: "S-1-1-0".into(),
        });
        dacl.push(Ace {
            kind: 1,
            flags: 0,
            mask: FULL_CONTROL,
            sid: user.into(),
        });
        assert_eq!(admins_only(&d), Ok(()));
        d.dacl.as_mut().unwrap().insert(
            0,
            Ace {
                kind: 0,
                flags: 0,
                mask: 0x2,
                sid: user.into(),
            },
        );
        assert!(admins_only(&d).is_err());
        // A kind of entry not read is not trusted.
        let mut d = safe.clone();
        d.dacl.as_mut().unwrap().push(Ace {
            kind: 9,
            flags: 0,
            mask: 0,
            sid: String::new(),
        });
        assert!(admins_only(&d).unwrap_err().contains("kind 9"));
        // Windows itself and its installer are as good as administrators.
        let mut d = safe;
        d.owner = Some(SID_TRUSTED_INSTALLER.into());
        d.dacl.as_mut().unwrap().push(Ace {
            kind: 0,
            flags: 0,
            mask: FULL_CONTROL,
            sid: SID_SYSTEM.into(),
        });
        assert_eq!(admins_only(&d), Ok(()));
    }

    #[test]
    fn a_sid_reads_back_as_written() {
        for sid in [SID_SYSTEM, SID_ADMINISTRATORS, SID_TRUSTED_INSTALLER, "S-1-1-0"] {
            assert_eq!(sid_text(&sid_bytes(sid)).as_deref(), Some(sid));
        }
        assert_eq!(sid_text(&[1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0]), None);
    }

    #[test]
    fn the_takeover_mode_reads_as_ask_unless_it_says_off() {
        assert_eq!(Mode::default(), Mode::Ask);
        for m in [Mode::Off, Mode::Ask] {
            assert_eq!(Mode::from_word(Some(m.word())), m);
            let kept = serde_json::to_string(&m).unwrap();
            assert_eq!(kept, format!("{:?}", m.word()));
            assert_eq!(serde_json::from_str::<Mode>(&kept).unwrap(), m);
        }
        assert_eq!(Mode::from_word(None), Mode::Ask);
        assert_eq!(Mode::from_word(Some("always")), Mode::Ask);
        // A newer build's word must not make the whole state file unreadable.
        assert_eq!(serde_json::from_str::<Mode>("\"always\"").unwrap(), Mode::Ask);
        assert!(OFF.starts_with("Takeover is off."));
    }

    #[test]
    fn the_new_name_is_read_and_the_old_only_when_the_new_is_absent() {
        type Opened = Result<Option<&'static str>, &'static str>;
        let read = |new: Opened, old: Opened| {
            let mut asked = Vec::new();
            let found = read_first(FILE, OLD_FILE, |name| {
                asked.push(name.to_string());
                if name == FILE {
                    new
                } else {
                    old
                }
            });
            (found, asked)
        };
        // Both there: the new one, and the old is not even opened.
        let (found, asked) = read(Ok(Some("new")), Ok(Some("old")));
        assert_eq!(found, Ok(Some((FILE, "new"))));
        assert_eq!(asked, [FILE]);
        // Only the old one, as on a machine set up before the rename: its
        // list still answers.
        let (found, asked) = read(Ok(None), Ok(Some("old")));
        assert_eq!(found, Ok(Some((OLD_FILE, "old"))));
        assert_eq!(asked, [FILE, OLD_FILE]);
        assert_eq!(read(Ok(None), Ok(None)).0, Ok(None));
        // The old one is held to the same checks.
        assert_eq!(read(Ok(None), Err("a user's")).0, Err((OLD_FILE, "a user's")));
        // A new one that is refused is not passed over for the old.
        let (found, asked) = read(Err("a user's"), Ok(Some("old")));
        assert_eq!(found, Err((FILE, "a user's")));
        assert_eq!(asked, [FILE]);
    }

    #[test]
    fn an_old_name_is_moved_only_where_nothing_has_the_new_one() {
        assert!(moves(false, true));
        assert!(!moves(true, true), "the new file would be written over");
        assert!(!moves(true, false));
        assert!(!moves(false, false));
        for (new, old) in [
            (FILE, OLD_FILE),
            (LOG_FILE, OLD_LOG_FILE),
            (ADMIN_LOG_FILE, OLD_ADMIN_LOG_FILE),
        ] {
            assert_eq!(new, old.replace("desktop", "takeover"));
        }
    }
}
