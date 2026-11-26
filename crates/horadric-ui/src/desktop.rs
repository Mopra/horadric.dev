//! The app's side of an agent using a desktop program, through `horadric
//! mcp`. The MCP process does the seeing and the clicking; before each call
//! it asks here whether it may, and the human decides.
//!
//! What the human allows is kept per session and per program, by the
//! program's full path and whether it runs as administrator, for the life
//! of the app: a program's dialogs are the same program, so allowing
//! Sysmac Studio covers its Transfer dialog too. A program that only
//! hosts other things (mmc, ApplicationFrameHost, explorer) is kept by
//! what it shows. A risky action is never kept. Each one is asked, since
//! one wrong click there can stop a machine, unless an administrator
//! pre-approved that very action.
//!
//! Programs can be listed ahead of time, in two files named
//! `desktop-programs.json` (see [`horadric_core::desktop_programs`]). The
//! user's, beside the app's state, any program can write, so it only
//! stands in for the approval question of a program that is not elevated.
//! The administrators', which only an elevated shell can write and which
//! is checked for that on every read, lists the programs that run as
//! administrator and holds the pre-approved risky actions. Each grant
//! from a list is shown once per session and program and written to
//! `desktop.log`, with the answers to the questions and every
//! pre-approved risky action beside it.
//!
//! The app's word is not what guards a program that runs as
//! administrator: `horadric desktop-helper`, which alone can act there,
//! asks the human itself. So what is risky in such a program is asked by
//! the helper, not here as well.
//!
//! A question refused, or left unanswered, is not asked again for two
//! minutes, and only a few wait per session, so a session can not keep
//! the human answering.
//!
//! A question is put aside from the program it is about (see
//! [`crate::dialog::show_aside`]), so the program's own dialogs stay in
//! sight and in reach, and it waits only so long: nobody answering is told
//! to the session as no answer, not as a refusal, so on a machine with
//! nobody at it the session asks in the chat instead of waiting.
//!
//! One session drives a program at a time. While a session that operated
//! a program is still at work, another session's operate call in the same
//! program is refused with who holds it, as is one while another session's
//! question about it waits; looking stays open to every session. Sessions
//! are named by id and start as well as by name, in the questions and on
//! the plate, since two can share a name.
//!
//! Asking runs the dialog's modal loop, which must not hold the app, so a
//! question waits on [`Desktop`] and is asked from the window procedure.
//!
//! What is allowed is shown while it happens, see [`super::ghost`]: a
//! click waits for the ghost cursor to reach it, and the human can stop it
//! on the way.
//!
//! The plate's Bypass lets a human who trusts the work and watches it skip
//! the risky question in a program the session may already operate. It
//! never lets a session into a program, and never counts for one that
//! runs as administrator. It is never kept, and what it lets through
//! unasked is still held for Esc, which stops it and turns bypass off.

use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::time::{Instant, SystemTime};

use std::time::Duration;

use horadric_core::desktop_programs::{
    self as listed, Act, GrantKey, Kind as ActKind, Level, Refusals, Source,
};
use horadric_hooks::listener::{BrowserCall, Reply};
use serde_json::{json, Value};
use windows::core::GUID;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, PROPERTYKEY, RECT};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenIntegrityLevel,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    GetLastActivePopup, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible,
};

/// The administrators' list on disk. One file for the app and for the
/// binary's elevated helper and `desktop` command, see its notes.
#[path = "desktop_admin.rs"]
mod desktop_admin;

use super::ghost::{self, Allowed};
use super::{ask_aside, post, with_app, App, WM_HORADRIC_DESKTOP};
use crate::dialog::{Answer, Aside, Dialog, Tone};
use crate::toast::Kind;

/// How far the human lets a session into a program.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Grant {
    #[default]
    None,
    See,
    Operate,
}

/// What a call wants to do in a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    /// Screenshots, reading what is in it.
    See,
    /// Clicks, typing, picking from lists.
    Operate,
}

/// What to do with a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Decision {
    Allow,
    /// The program is not allowed this far yet.
    AskApprove,
    /// The program is allowed, but this action could change something that
    /// matters.
    AskRisky,
    Refuse(Refusal),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// Said no, or put the question away.
    Refused,
    /// Asked to operate, allowed only to look.
    OnlyLook,
    /// Nobody answered within so many seconds.
    NoAnswer(u64),
    /// The same question was refused or left unanswered a moment ago, and
    /// is not asked again for so many seconds.
    Held(listed::Question, u64),
    /// Too many of the session's questions wait already.
    Crowded,
}

/// The approval question's buttons, in order.
const APPROVE: [&str; 3] = ["Let it operate", "Only let it look", "Refuse"];
/// The risky question's buttons, in order.
const RISKY: [&str; 2] = ["Do it", "Refuse"];

/// Whether a call may go ahead with what is already allowed, or what to ask.
pub(super) fn decide(grant: Grant, action: Action, risky: bool) -> Decision {
    match action {
        Action::See if grant >= Grant::See => Decision::Allow,
        Action::Operate if grant == Grant::Operate && risky => Decision::AskRisky,
        Action::Operate if grant == Grant::Operate => Decision::Allow,
        _ => Decision::AskApprove,
    }
}

/// A level on the list as the grant it stands for.
pub(super) fn grant_of_level(level: Option<Level>) -> Grant {
    match level {
        None => Grant::None,
        Some(Level::Look) => Grant::See,
        Some(Level::Operate) => Grant::Operate,
    }
}

/// What a session may do in a program: what the human answered for it, or
/// what the list allows every session, whichever is more. Neither covers a
/// risky action: [`decide`] asks about that whatever the grant.
pub(super) fn effective(own: Grant, level: Option<Level>) -> Grant {
    own.max(grant_of_level(level))
}

/// Whether a call goes ahead only because of the list, so it is worth
/// telling the human it was not asked.
pub(super) fn by_the_list(own: Grant, level: Option<Level>, action: Action, risky: bool) -> bool {
    decide(own, action, risky) != Decision::Allow
        && decide(effective(own, level), action, risky) == Decision::Allow
}

/// What bypass makes of a decision: the risky question becomes an allowed
/// call, true when held for the human's Esc. That question is only ever
/// asked of a session that may operate the program already, so bypass
/// never lets a session into a program: the approval question is asked
/// with bypass on as with it off. Nor does it count for a program that
/// runs as administrator. Nothing else moves: what is allowed goes as
/// ever, and a refusal stays one.
pub(super) fn bypassed(decision: Decision, bypass: bool, elevated: bool) -> (Decision, bool) {
    match decision {
        Decision::AskRisky if bypass && !elevated => (Decision::Allow, true),
        d => (d, false),
    }
}

/// The grant the approval question leaves, and what then becomes of the
/// call. `pressed` is the button, None for put away. A refusal leaves the
/// grant as it was: the next call asks again rather than being refused
/// unseen.
pub(super) fn approved(
    grant: Grant,
    action: Action,
    risky: bool,
    pressed: Option<usize>,
) -> (Grant, Decision) {
    match pressed {
        Some(0) => (Grant::Operate, decide(Grant::Operate, action, risky)),
        Some(1) => {
            let grant = grant.max(Grant::See);
            match decide(grant, action, risky) {
                Decision::Allow => (grant, Decision::Allow),
                _ => (grant, Decision::Refuse(Refusal::OnlyLook)),
            }
        }
        _ => (grant, Decision::Refuse(Refusal::Refused)),
    }
}

/// What becomes of a risky call after its question.
pub(super) fn risky_answered(pressed: Option<usize>) -> Decision {
    match pressed {
        Some(0) => Decision::Allow,
        _ => Decision::Refuse(Refusal::Refused),
    }
}

/// How long a question still waits, `within` seconds from when its call
/// came, `waited` ago. Never so short the human can not read it.
pub(super) fn time_left(within: u64, waited: Duration) -> Duration {
    Duration::from_secs(within)
        .saturating_sub(waited)
        .max(Duration::from_secs(5))
}

/// The button an answer pressed, if any, and the refusal for none at all.
/// Nobody answering refuses the call too, but says so, so the session
/// asks in the chat rather than taking it for the human's no.
pub(super) fn pressed_of(answer: Answer, secs: u64) -> Result<Option<usize>, Refusal> {
    match answer {
        Answer::Pressed(i) => Ok(Some(i)),
        Answer::Dismissed => Ok(None),
        Answer::Unanswered => Err(Refusal::NoAnswer(secs)),
    }
}

/// The button Enter presses at first: the one that refuses, in every
/// question. A key meant for another window must never let an agent in.
pub(super) const APPROVE_DEFAULT: usize = 2;
/// The same for the risky question.
pub(super) const RISKY_DEFAULT: usize = 1;

/// A program's name as the human knows it, from its exe.
pub(super) fn program_name(exe: &str) -> &str {
    let cut = exe.len().saturating_sub(4);
    match exe.get(cut..) {
        Some(ext) if cut > 0 && ext.eq_ignore_ascii_case(".exe") => &exe[..cut],
        _ => exe,
    }
}

/// Why a call was refused, written to the agent so it stops asking and
/// says so in the chat instead.
pub(super) fn refusal_text(why: Refusal, action: Action, risky: bool, exe: &str) -> String {
    // Said with each no, so the agent does not try the same again at once.
    let hold = format!(
        ". They are not asked about it again for {} s: say in the chat what you need instead of \
         trying again",
        listed::REFUSED_FOR.as_secs()
    );
    match (why, action) {
        (Refusal::OnlyLook, _) => format!(
            "the human allowed only looking at {exe}, not operating it{hold}"
        ),
        (Refusal::Refused, Action::See) => {
            format!("the human refused letting you see {exe}{hold}")
        }
        (Refusal::Refused, Action::Operate) if risky => {
            format!("the human refused this change in {exe}{hold}")
        }
        (Refusal::Refused, Action::Operate) => {
            format!("the human refused operating {exe}{hold}")
        }
        (Refusal::Held(question, secs), _) => {
            listed::refused_text(question, exe, Duration::from_secs(secs))
        }
        (Refusal::Crowded, _) => listed::crowded_text(exe),
        (Refusal::NoAnswer(secs), _) => {
            let what = if risky {
                "this change"
            } else {
                "letting you use it"
            };
            format!(
                "no technician answered Horadric's question about {what} in {exe} within \
                 {secs} s, so nothing was done, and the question is not asked again for {} s. \
                 Nobody may be at this machine: ask in the chat. If this action is safe to do \
                 without asking, an administrator can pre-approve it from an elevated shell \
                 (horadric desktop approve)",
                listed::REFUSED_FOR.as_secs()
            )
        }
    }
}

/// The notice shown as a session's `horadric mcp` is about to have Windows
/// ask for administrator rights, from a `desktop.elevating` body: a
/// question from Windows the human did not see coming would look like
/// something else wanting in. Its title and its text.
pub(super) fn elevating_notice(session: &str, body: &Value) -> (String, String) {
    let window = &body["window"];
    let exe = window["exe"].as_str().map(str::trim).unwrap_or_default();
    let title = window["title"].as_str().map(str::trim).unwrap_or_default();
    let program = match (exe, title) {
        ("" | "?", "") => "a program".to_string(),
        ("" | "?", t) => format!("the program of {t:?}"),
        (e, _) => program_name(e).to_string(),
    };
    let only = match window["path"].as_str().map(str::trim) {
        Some(path) if !path.is_empty() => format!(" ({path})"),
        _ => String::new(),
    };
    // Windows names the file it starts but not that it is no installed
    // Horadric: a build from a source tree is whatever was last built.
    let dev = match body["dev_helper"].as_str().map(str::trim) {
        Some(exe) if !exe.is_empty() => format!(
            " The helper Windows will name is a development build, not the installed Horadric: \
             {exe}."
        ),
        _ => String::new(),
    };
    (
        "Windows will ask for administrator rights".to_string(),
        format!(
            "{session} wants to reach {program}, which runs as administrator. Windows will ask \
             to let Horadric's helper run as administrator, for that one program{only}. The \
             helper then asks you itself, in a window of its own, before the first click or key \
             there and before every risky action. Operating a program that runs as \
             administrator can not be limited to that program: its Open and Save dialogs can \
             start others with its rights. No keeps the helper out.{dev}"
        ),
    )
}

/// The tile's last line while a session operates a program.
pub(super) fn operating_line(program: &str, what: &str) -> String {
    match what.trim() {
        "" => format!("operating {program}"),
        what => format!("operating {program}: {what}"),
    }
}

/// The short word a refusal leaves in the history.
pub(super) fn refusal_word(why: Refusal) -> &'static str {
    match why {
        Refusal::Refused => "refused by the human",
        Refusal::OnlyLook => "only looking was allowed",
        Refusal::NoAnswer(_) => "no answer",
        Refusal::Held(..) => "not asked again so soon",
        Refusal::Crowded => "too many questions waiting",
    }
}

/// When a session started, as a local clock reads it: `local_now` is
/// seconds into today on that clock, `ago` how long since it started.
pub(super) fn started_clock(local_now: u64, ago: u64) -> String {
    let at = (local_now + 86_400 - ago % 86_400) % 86_400;
    format!("{:02}:{:02}", at / 3600, (at % 3600) / 60)
}

/// A session as the human tells it from another of the same name.
pub(super) fn ident_line(id: &str, started: Option<&str>) -> String {
    match started {
        Some(at) => format!("id {id}, started {at}"),
        None => format!("id {id}"),
    }
}

/// What a session is told when another holds the program it would operate.
pub(super) fn busy_text(program: &str, name: &str, ident: &str, asking: bool) -> String {
    let how = if asking {
        "is waiting for the human's answer to operate it"
    } else {
        "operates it and is still at work"
    };
    format!(
        "{program} is held by another session, {name:?} ({ident}), which {how}. One session \
         drives a program at a time, so nothing was done. Wait until that session is done, or \
         ask the human in the chat; looking at the program still works"
    )
}

/// The key a program is held by, for one session driving it at a time:
/// its exe name, run as administrator or not.
pub(super) fn program_key(exe: &str, path: &str) -> String {
    listed_name(exe, path).to_lowercase()
}

/// A call as `horadric mcp` sent it.
#[derive(Debug, PartialEq)]
pub(super) struct Request {
    pub title: String,
    pub exe: String,
    /// The exe's full path, empty when the MCP process could not tell or
    /// is from before paths were sent.
    pub path: String,
    /// The program's window, for the screen the Thinking plate goes on.
    pub hwnd: isize,
    pub action: Action,
    pub what: String,
    pub risky: bool,
    /// Where the click goes, in physical screen pixels, for the ghost.
    pub point: Option<(i32, i32)>,
    /// The agent's own sentence on why.
    pub reason: String,
    /// An operate action in parts, for the pre-approved patterns. Empty
    /// for looking, and from an MCP process before parts were sent.
    pub act: Act,
    /// The call says the program runs as administrator, reached through
    /// the elevated helper. The session's word: it makes the app
    /// stricter, never laxer.
    pub administrator: bool,
}

/// A finished action, as `desktop.done` tells it.
#[derive(Debug, PartialEq)]
pub(super) struct Done {
    pub what: String,
    pub ok: bool,
    pub outcome: String,
}

/// Reads a `desktop.done` body. Nothing waits on it, so a part missing
/// is left empty rather than refused.
pub(super) fn parse_done(body: &Value) -> Done {
    let text = |k: &str| {
        body.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Done {
        what: text("what"),
        ok: body.get("ok").and_then(Value::as_bool).unwrap_or(true),
        outcome: text("outcome"),
    }
}

/// Reads the parts of an operate action. Anything missing is left empty,
/// which no pattern matches but a wildcard.
pub(super) fn act_of(v: Option<&Value>) -> Act {
    let Some(v) = v else {
        return Act::default();
    };
    let text = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Act {
        kind: v
            .get("kind")
            .and_then(Value::as_str)
            .and_then(|k| ActKind::parse(k).ok()),
        window: text("window"),
        control: text("control"),
        name: text("name"),
        key: text("key"),
        texts: v
            .get("texts")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// What the log and the question say a call does: its sentence, and where,
/// when that is a window of its own, with what a small dialog asks. The
/// human writes a pattern from it.
pub(super) fn what_where(what: &str, act: &Act, title: &str) -> String {
    let mut said = what.trim().to_string();
    let window = act.window.trim();
    if !window.is_empty() && window != title.trim() {
        said.push_str(&format!(" in window {window:?}"));
    }
    if let Some(text) = act.texts.iter().map(|t| t.trim()).find(|t| !t.is_empty()) {
        let short: String = text.chars().take(100).collect();
        said.push_str(&format!(" saying {short:?}"));
    }
    said
}

fn point_of(v: Option<&Value>) -> Option<(i32, i32)> {
    let v = v?;
    let at = |k: &str| v.get(k)?.as_i64().and_then(|n| i32::try_from(n).ok());
    Some((at("x")?, at("y")?))
}

/// Reads a `desktop.allow` body, or says what is wrong with it.
pub(super) fn parse(body: &Value) -> Result<Request, String> {
    if body.get("op").and_then(Value::as_str) != Some("desktop.allow") {
        return Err("unknown desktop call".into());
    }
    let window = body.get("window").ok_or("say which window")?;
    let exe = window
        .get("exe")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .ok_or("say which program: the window's exe")?;
    let action = match body.get("action").and_then(Value::as_str) {
        Some("see") => Action::See,
        Some("operate") => Action::Operate,
        _ => return Err("say what to do: see or operate".into()),
    };
    let text = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or_default().to_string();
    Ok(Request {
        title: text(window.get("title")),
        exe: exe.to_string(),
        path: text(window.get("path")).trim().to_string(),
        hwnd: window
            .get("hwnd")
            .and_then(Value::as_i64)
            .and_then(|h| isize::try_from(h).ok())
            .unwrap_or(0),
        action,
        what: text(body.get("what")),
        risky: body.get("risky").and_then(Value::as_bool).unwrap_or(false),
        point: point_of(body.get("point")),
        reason: text(body.get("reason")),
        act: match action {
            Action::Operate => act_of(body.get("act")),
            Action::See => Act::default(),
        },
        administrator: body.get("administrator").and_then(Value::as_bool) == Some(true),
    })
}

/// A call waiting for the human, with where its answer goes.
pub(super) struct Pending {
    session: String,
    request: Request,
    reply: Reply,
    /// When it came, so a question asked after others waits only the rest
    /// of its time.
    since: Instant,
    /// The program runs as administrator: the call says so, or the app
    /// saw it. The user's list, a grant for the normal program and bypass
    /// all stop short of it.
    elevated: bool,
    /// The app saw for itself that the program runs above it. Nothing
    /// that is not elevated can then act in it, whatever the app answers,
    /// so its risky actions are the elevated helper's to ask about. A
    /// call's own word for it is never enough to skip a question here.
    verified: bool,
    /// What tells this window from the others of a host, see
    /// [`listed::hosted`].
    hosted: String,
}

impl Pending {
    fn key(&self) -> GrantKey {
        let r = &self.request;
        GrantKey::new(&self.session, &r.path, &r.exe, self.elevated, &self.hosted)
    }

    /// The program as the log names it: its path when known.
    fn program(&self) -> String {
        let r = &self.request;
        let named = if r.path.is_empty() { &r.exe } else { &r.path };
        if self.elevated {
            format!("{named} as administrator")
        } else {
            named.clone()
        }
    }

    /// The program as the questions and the agent's answers name it.
    fn shown(&self) -> String {
        shown_program(&self.request.exe, self.elevated)
    }

    /// What it does, as the log says it.
    fn what(&self) -> String {
        let r = &self.request;
        what_where(&r.what, &r.act, &r.title)
    }

    fn refuse(&self, why: Refusal) {
        let r = &self.request;
        self.reply
            .send(json!({ "error": refusal_text(why, r.action, r.risky, &self.shown()) }));
    }
}

/// A program's name with how it runs, when that is as administrator.
pub(super) fn shown_program(exe: &str, elevated: bool) -> String {
    if elevated {
        format!("{exe} (as administrator)")
    } else {
        exe.to_string()
    }
}

/// The exe name the lists are matched against: the file of the path when
/// the call gave one.
pub(super) fn listed_name<'a>(exe: &'a str, path: &'a str) -> &'a str {
    match path.rsplit(['\\', '/']).next() {
        Some(name) if !name.trim().is_empty() => name,
        _ => exe,
    }
}

/// Something the human is told about the lists, once.
struct Notice {
    title: &'static str,
    text: String,
    /// It also goes to the log, as a list that could not be read.
    unreadable: bool,
}

/// The two lists as last read, and each file's stamp then, so they are
/// read again only when they change.
#[derive(Default)]
struct Lists {
    user_stamp: Option<(SystemTime, u64)>,
    /// The user's list as it counts, see [`listed::user_list`].
    user: listed::List,
    admin_stamp: Option<(SystemTime, u64)>,
    admin: listed::List,
    /// Why the administrators' list is not used, as last told. While set
    /// the file is read on every call: mending who may write it does not
    /// change its stamp.
    admin_refused: Option<String>,
}

fn stamp_of(file: &std::path::Path) -> Option<(SystemTime, u64)> {
    std::fs::metadata(file)
        .ok()
        .map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()))
}

/// What the human allowed, and the calls waiting to be asked.
#[derive(Default)]
pub(super) struct Desktop {
    grants: HashMap<GrantKey, Grant>,
    /// The questions refused of late, which are not asked again yet.
    refused: Refusals<GrantKey>,
    waiting: VecDeque<Pending>,
    lists: Lists,
    /// The session, program and level each grant from the list was shown
    /// for, so it is shown once and not on every screenshot.
    told: HashSet<(String, String, Level)>,
    /// The elevated programs the human was told the user's list no longer
    /// covers.
    moved: HashSet<String>,
    /// The session that last operated each program, by [`program_key`].
    drivers: HashMap<String, String>,
    /// The session and program of the question on screen.
    asking: Option<(String, String)>,
    /// When each session last said Windows is about to ask for a helper.
    elevating: HashMap<String, Instant>,
}

/// The least time between two notices that Windows will ask, per session.
const ELEVATING_APART: Duration = Duration::from_secs(20);

impl Desktop {
    /// Reads each list again if its file changed, and says what the human
    /// should hear of it. A list that can not be read, or that a program
    /// that is not elevated could have written, counts as empty, so the
    /// human is asked.
    fn refresh(&mut self) -> Vec<Notice> {
        let mut notices = Vec::new();
        if let Some(file) = horadric_hooks::tasks::desktop_programs_file() {
            let stamp = stamp_of(&file);
            if stamp != self.lists.user_stamp {
                self.lists.user_stamp = stamp;
                let at = |why: String| format!("{}: {why}", file.display());
                let text = match std::fs::read_to_string(&file) {
                    Ok(text) => Ok(text),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
                    Err(e) => Err(at(e.to_string())),
                };
                match text.and_then(|t| listed::parse(&t).map_err(at)) {
                    Ok(read) => {
                        let (list, misplaced) = listed::user_list(read);
                        self.lists.user = list;
                        if let Some(first) = misplaced.first() {
                            notices.push(Notice {
                                title: "Part of your list of desktop programs is no longer used",
                                text: misplaced_text(first, misplaced.len()),
                                unreadable: false,
                            });
                        }
                    }
                    Err(why) => {
                        self.lists.user = listed::List::default();
                        notices.push(Notice {
                            title: "The list of allowed desktop programs can not be read",
                            text: format!(
                                "{why}. Horadric asks instead. `horadric desktop list` shows it."
                            ),
                            unreadable: true,
                        });
                    }
                }
            }
        }
        let stamp = desktop_admin::file().as_deref().and_then(stamp_of);
        if stamp != self.lists.admin_stamp || self.lists.admin_refused.is_some() {
            self.lists.admin_stamp = stamp;
            let (list, refused) = match desktop_admin::read() {
                desktop_admin::Read::List(list) => (list, None),
                desktop_admin::Read::Missing => (listed::List::default(), None),
                desktop_admin::Read::Refused(why) => (listed::List::default(), Some(why)),
            };
            self.lists.admin = list;
            if let Some(why) = refused.as_ref().filter(|w| Some(*w) != self.lists.admin_refused.as_ref()) {
                notices.push(Notice {
                    title: "The administrators' list of desktop programs is not used",
                    text: format!("{why}. Horadric asks instead."),
                    unreadable: true,
                });
            }
            self.lists.admin_refused = refused;
        }
        notices
    }

    /// How far the lists let the program of `p` in.
    fn listed(&self, p: &Pending) -> listed::Listed {
        let r = &p.request;
        listed::level_for(
            &self.lists.user.programs,
            &self.lists.admin.programs,
            &r.path,
            listed_name(&r.exe, &r.path),
            p.elevated,
        )
    }

    /// How long a question waits for the human, in seconds: what the
    /// administrators' list says, else the user's.
    fn answer_within(&self) -> u64 {
        listed::List {
            programs: Vec::new(),
            answer_within: self
                .lists
                .admin
                .answer_within
                .or(self.lists.user.answer_within),
        }
        .answer_within()
    }
}

/// What the human is told of entries the user's list can no longer hold.
fn misplaced_text(first: &listed::Misplaced, count: usize) -> String {
    let more = match count {
        1 => String::new(),
        n => format!(" (and {} more, `horadric desktop list` shows each)", n - 1),
    };
    format!(
        "{}{more}: any program can write the list in %APPDATA%, so it no longer covers programs \
         that run as administrator or risky actions. Those are asked again until an administrator \
         lists them, from an elevated shell: {}",
        first.what, first.command
    )
}

/// How a window's program stands to the app, as the app sees it itself.
struct Seen {
    /// The program runs above the app, or will not say how it runs. None
    /// when the window is gone or was not named.
    above: Option<bool>,
    /// The window's AppUserModelID, asked for a host's window only.
    app_id: Option<String>,
}

/// The integrity level of process `pid`, or of the app when none: what
/// Windows walls input and UI Automation by.
fn level_of(pid: Option<u32>) -> Option<u32> {
    unsafe {
        let process = match pid {
            Some(id) => OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, id).ok()?,
            None => GetCurrentProcess(),
        };
        let mut token = HANDLE::default();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token);
        if pid.is_some() {
            let _ = CloseHandle(process);
        }
        opened.ok()?;
        let mut len = 0;
        let _ = GetTokenInformation(token, TokenIntegrityLevel, None, 0, &mut len);
        // In u64s, so the label's pointer inside is aligned.
        let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
        let read = GetTokenInformation(
            token,
            TokenIntegrityLevel,
            Some(buf.as_mut_ptr().cast()),
            len,
            &mut len,
        );
        let _ = CloseHandle(token);
        read.ok()?;
        let sid = (*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL)).Label.Sid;
        let count = GetSidSubAuthorityCount(sid);
        if count.is_null() || *count == 0 {
            return None;
        }
        let rid = GetSidSubAuthority(sid, u32::from(*count) - 1);
        (!rid.is_null()).then(|| *rid)
    }
}

/// Looks at the window a call names. The call's own word for how the
/// program runs is the session's, and a session can send what it likes.
fn seen_of(hwnd: isize, exe: &str) -> Seen {
    let h = HWND(hwnd as *mut _);
    let mut pid = 0u32;
    if hwnd == 0 || !unsafe { IsWindow(Some(h)) }.as_bool() {
        return Seen {
            above: None,
            app_id: None,
        };
    }
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    let above = match (level_of(None), level_of(Some(pid))) {
        (Some(me), Some(them)) => them > me,
        // A program that will not say may be above, as the MCP process
        // takes it too.
        _ => true,
    };
    let app_id = listed::is_host(exe)
        .then(|| unsafe { app_id_of(h) })
        .flatten();
    Seen {
        above: Some(above),
        app_id,
    }
}

/// The AppUserModelID Windows keeps on a window, which for a packaged app
/// shown by ApplicationFrameHost names the app.
unsafe fn app_id_of(h: HWND) -> Option<String> {
    let store: IPropertyStore = SHGetPropertyStoreForWindow(h).ok()?;
    // PKEY_AppUserModel_ID.
    let key = PROPERTYKEY {
        fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
        pid: 5,
    };
    let value = store.GetValue(&key).ok()?;
    let text = PropVariantToStringAlloc(&value).ok()?;
    let id = text.to_string().ok();
    CoTaskMemFree(Some(text.0 as *const core::ffi::c_void));
    id.filter(|id| !id.trim().is_empty())
}

/// Adds a line to `desktop.log`. A log that can not be written loses the
/// line, never the call.
pub(super) fn log(session: &str, source: Source, program: &str, what: &str) {
    let Some(file) = horadric_hooks::tasks::desktop_log_file() else {
        return;
    };
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let line = listed::log_line(
        &horadric_core::tasks::utc(secs),
        session,
        source,
        program,
        what,
    );
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .and_then(|mut f| f.write_all(line.as_bytes()));
}

/// A question about to be asked, outside the app's borrow.
pub(super) struct Question {
    pending: Pending,
    /// The session's name, for the title.
    name: String,
    /// Its id and start.
    ident: String,
    risky: bool,
    /// How long it waits for the human, in seconds.
    within: u64,
    /// What is left of that after waiting behind other questions.
    left: Duration,
}

thread_local! {
    /// A question is on screen. The dialog's loop dispatches the message
    /// that asks the next one, and that one must wait its turn, not open a
    /// second dialog on top: the loop asking takes it once this is
    /// answered. Kept off the app, which may be borrowed when it is cleared.
    static ASKING: Cell<bool> = const { Cell::new(false) };
}

impl App {
    /// A `desktop.` call from `horadric mcp`. The caller has shown the
    /// session's key already, see [`App::browser_call`].
    pub(super) fn desktop_call(&mut self, call: BrowserCall) {
        let known = self
            .shared
            .registry
            .lock()
            .is_ok_and(|r| r.get(&call.session).is_some());
        if !known {
            return call
                .reply
                .send(json!({ "error": "this session is not one of Horadric's" }));
        }
        if call.body.get("op").and_then(Value::as_str) == Some("desktop.done") {
            let d = parse_done(&call.body);
            ghost::done(&self.shared, &d.what, d.ok, &d.outcome);
            return call.reply.send(json!({}));
        }
        if call.body.get("op").and_then(Value::as_str) == Some("desktop.elevating") {
            // Windows is asked for a helper at most every minute or two,
            // so a session that says this more often is not believed.
            let now = Instant::now();
            let lately = self
                .desktop
                .elevating
                .insert(call.session.clone(), now)
                .is_some_and(|last| now.duration_since(last) < ELEVATING_APART);
            if !lately {
                let (title, text) = elevating_notice(&self.name_of(&call.session), &call.body);
                self.toasts.show(Kind::Waiting, &title, &text);
            }
            return call.reply.send(json!({}));
        }
        let request = match parse(&call.body) {
            Ok(r) => r,
            Err(e) => return call.reply.send(json!({ "error": e })),
        };
        let seen = seen_of(request.hwnd, &request.exe);
        let pending = Pending {
            session: call.session,
            elevated: request.administrator || seen.above == Some(true),
            verified: seen.above == Some(true),
            hosted: listed::hosted(
                &request.exe,
                seen.app_id.as_deref(),
                &request.title,
                request.hwnd,
            ),
            request,
            reply: call.reply,
            since: Instant::now(),
        };
        for notice in self.desktop.refresh() {
            if notice.unreadable {
                let name = self.name_of(&pending.session);
                log(&name, Source::Unreadable, &notice.text, "");
            }
            self.toasts.show(Kind::Failed, notice.title, &notice.text);
        }
        if let Some(why) = self.held_by_another(&pending) {
            ghost::refused(
                &self.shared,
                &pending.request.what,
                "another session holds the program",
            );
            return pending.reply.send(json!({ "error": why }));
        }
        let asked = self.decision_for(&pending);
        match bypassed(asked, ghost::bypass(), pending.elevated) {
            (Decision::Allow, held) => return self.allow(pending, held),
            (Decision::Refuse(why), _) => return self.refuse(pending, why),
            (ask, _) => {
                if let Some(why) = self.not_asked(&pending, ask, true) {
                    return self.refuse(pending, why);
                }
            }
        }
        self.desktop.waiting.push_back(pending);
        post(self.notify.0 as isize, WM_HORADRIC_DESKTOP, 0);
    }

    /// Why a question is not put to the human: the same one was refused,
    /// or went unanswered, a moment ago, or (`arriving`, for a call just
    /// come) too many of the session's questions wait already. Without
    /// this a session could keep a question on the human's screen for as
    /// long as it liked.
    fn not_asked(&self, p: &Pending, ask: Decision, arriving: bool) -> Option<Refusal> {
        let question = match ask {
            Decision::AskRisky => listed::Question::Risky,
            _ => listed::Question::Approve,
        };
        if let Some(left) = self
            .desktop
            .refused
            .left(&p.key(), question, Instant::now())
        {
            return Some(Refusal::Held(question, left.as_secs()));
        }
        let waiting = self
            .desktop
            .waiting
            .iter()
            .filter(|w| w.session == p.session)
            .count();
        (arriving && waiting >= listed::MOST_WAITING).then_some(Refusal::Crowded)
    }

    /// A session's id and when it started, as the human reads them.
    pub(super) fn ident_of(&self, id: &str) -> String {
        let created = self
            .shared
            .registry
            .lock()
            .ok()
            .and_then(|r| r.get(id).map(|s| s.created));
        let started = created.map(|c| {
            let ago = SystemTime::now()
                .duration_since(c)
                .map_or(0, |d| d.as_secs());
            let t = unsafe { GetLocalTime() };
            let local =
                u64::from(t.wHour) * 3600 + u64::from(t.wMinute) * 60 + u64::from(t.wSecond);
            started_clock(local, ago)
        });
        ident_line(id, started.as_deref())
    }

    /// Why an operate call must wait for another session: one still at
    /// work that operated the program last, or one whose question about
    /// operating it waits.
    fn held_by_another(&self, p: &Pending) -> Option<String> {
        let r = &p.request;
        if r.action != Action::Operate {
            return None;
        }
        let key = program_key(&r.exe, &r.path);
        let program = program_name(listed_name(&r.exe, &r.path));
        let asking = self
            .desktop
            .asking
            .as_ref()
            .filter(|(s, k)| *k == key && *s != p.session)
            .map(|(s, _)| s.clone())
            .or_else(|| {
                self.desktop
                    .waiting
                    .iter()
                    .find(|w| {
                        let wr = &w.request;
                        w.session != p.session
                            && wr.action == Action::Operate
                            && program_key(&wr.exe, &wr.path) == key
                    })
                    .map(|w| w.session.clone())
            });
        if let Some(other) = asking {
            return Some(busy_text(
                program,
                &self.name_of(&other),
                &self.ident_of(&other),
                true,
            ));
        }
        let other = self
            .desktop
            .drivers
            .get(&key)
            .filter(|s| **s != p.session)?;
        let working = self
            .shared
            .registry
            .lock()
            .ok()
            .map(|reg| ghost::turn_of(reg.get(other).map(|s| &s.phase)))
            == Some(ghost::Turn::Working);
        working.then(|| busy_text(program, &self.name_of(other), &self.ident_of(other), false))
    }

    /// What the human answered for this session and program.
    fn grant_of(&self, p: &Pending) -> Grant {
        self.desktop
            .grants
            .get(&p.key())
            .copied()
            .unwrap_or_default()
    }

    /// The pattern on the administrators' list that lets this risky call
    /// go ahead unasked, in words, when the session may operate the
    /// program. The user's list holds none that count.
    fn pre_approval(&self, p: &Pending, grant: Grant) -> Option<String> {
        let r = &p.request;
        if !r.risky || grant != Grant::Operate {
            return None;
        }
        listed::pre_approval(
            &self.desktop.lists.admin.programs,
            &r.path,
            listed_name(&r.exe, &r.path),
            p.elevated,
            &r.act,
        )
    }

    /// What becomes of a call with what the human answered and what the
    /// lists allow, before bypass. Going ahead on a list alone is shown
    /// once and logged, so the human can tell it was not asked; a risky
    /// action going ahead on a pre-approved pattern is logged every time.
    fn decision_for(&mut self, p: &Pending) -> Decision {
        let own = self.grant_of(p);
        let found = self.desktop.listed(p);
        let level = found.level;
        let r = &p.request;
        if let Some(command) = found.moved {
            if self.desktop.moved.insert(p.program().to_lowercase()) {
                self.toasts.show(
                    Kind::Failed,
                    "Your list no longer covers a program that runs as administrator",
                    &format!(
                        "{} runs as administrator, and any program can write the list in \
                         %APPDATA%, so its entry there is not used and you are asked. To allow it \
                         ahead of time, run from an elevated shell: {command}",
                        program_name(listed_name(&r.exe, &r.path))
                    ),
                );
            }
        }
        // Nothing that is not elevated can act in a program the app saw
        // running above it, so what is risky there is asked by the
        // elevated helper, which reads the window itself, and not twice.
        let pre = match p.verified {
            true => None,
            false => self.pre_approval(p, effective(own, level)),
        };
        let risky = r.risky && pre.is_none() && !p.verified;
        let decision = decide(effective(own, level), r.action, risky);
        if let Some(pattern) = pre {
            let name = self.name_of(&p.session);
            log(&name, Source::PreApproved(pattern), &p.program(), &p.what());
        }
        if let Some(level) = level.filter(|_| by_the_list(own, level, r.action, risky)) {
            let told = (p.session.clone(), p.program().to_lowercase(), level);
            if self.desktop.told.insert(told) {
                let name = self.name_of(&p.session);
                let program = shown_program(program_name(listed_name(&r.exe, &r.path)), p.elevated);
                log(&name, Source::List(level), &p.program(), &p.what());
                let may = match level {
                    Level::Look => "look at",
                    Level::Operate => "operate",
                };
                self.toasts.show(
                    Kind::Info,
                    &format!("{name} may {may} {program}"),
                    "Allowed ahead of time on the list of desktop programs, so not asked. \
                     Risky actions are still asked every time.",
                );
            }
        }
        decision
    }

    /// Lets a call go ahead. Operating is shown, so the human always knows
    /// a program is being driven; looking is not, since screenshots come
    /// many to a turn.
    ///
    /// The Thinking plate and the frame round the screens say the rest, so
    /// the toast comes once, as control begins. `unasked` is a call only
    /// bypass let through, held for Esc before it is answered.
    fn allow(&mut self, p: Pending, unasked: bool) {
        if unasked {
            let name = self.name_of(&p.session);
            log(&name, Source::Bypass, &p.program(), &p.what());
        }
        let name = self.name_of(&p.session);
        let ident = self.ident_of(&p.session);
        let program = shown_program(program_name(&p.request.exe), p.elevated);
        let r = p.request;
        if r.action == Action::Operate {
            self.desktop
                .drivers
                .insert(program_key(&r.exe, &r.path), p.session.clone());
        }
        if r.action == Action::Operate {
            // The tile says it until the agent's next tool event replaces
            // it. The tiles redraw on the next tick.
            if let Ok(mut reg) = self.shared.registry.lock() {
                if let Some(s) = reg.get_mut(&p.session) {
                    s.last_line = operating_line(&program, &r.what);
                }
            }
        }
        let call = Allowed {
            session: p.session.clone(),
            ident,
            name: name.clone(),
            program: program.clone(),
            reason: r.reason,
            what: r.what.clone(),
            point: r.point,
            hwnd: r.hwnd,
            unasked,
        };
        let started = ghost::allowed(&self.shared, call, p.reply);
        if started && r.action == Action::Operate {
            self.toasts
                .show(Kind::Info, &format!("{name} in {program}"), &r.what);
        }
    }

    /// Refuses a call, and keeps it in the history.
    fn refuse(&mut self, p: Pending, why: Refusal) {
        ghost::refused(&self.shared, &p.request.what, refusal_word(why));
        p.refuse(why);
    }

    /// The next waiting call that needs the human, answering on the way
    /// those that what was allowed since now covers, and refusing those
    /// whose question the human has just refused.
    fn next_question(&mut self) -> Option<Question> {
        while let Some(p) = self.desktop.waiting.pop_front() {
            if !p.reply.wanted() {
                // Its agent gave up waiting behind other questions, and
                // was told the call failed: asking now would act late.
                continue;
            }
            let asked = self.decision_for(&p);
            match bypassed(asked, ghost::bypass(), p.elevated) {
                (Decision::Allow, held) => self.allow(p, held),
                (Decision::Refuse(why), _) => self.refuse(p, why),
                (ask, _) => {
                    if let Some(why) = self.not_asked(&p, ask, false) {
                        self.refuse(p, why);
                        continue;
                    }
                    let r = &p.request;
                    self.desktop.asking = Some((p.session.clone(), program_key(&r.exe, &r.path)));
                    return Some(Question {
                        name: self.name_of(&p.session),
                        ident: self.ident_of(&p.session),
                        risky: ask == Decision::AskRisky,
                        within: self.desktop.answer_within(),
                        left: time_left(self.desktop.answer_within(), p.since.elapsed()),
                        pending: p,
                    });
                }
            }
        }
        None
    }

    /// Bypass was turned on: the calls waiting for a question go through
    /// it now, rather than after the human answers each.
    pub(super) fn desktop_bypass_on(&self) {
        if !self.desktop.waiting.is_empty() {
            post(self.notify.0 as isize, WM_HORADRIC_DESKTOP, 0);
        }
    }

    /// Acts on the human's answer. A question back when there is another
    /// to ask about the same call: allowed to operate, it may still be a
    /// risky one.
    fn answered(&mut self, mut q: Question, answer: Answer) -> Option<Question> {
        self.desktop.asking = None;
        let r = &q.pending.request;
        let key = q.pending.key();
        let question = if q.risky {
            listed::Question::Risky
        } else {
            listed::Question::Approve
        };
        let pressed = match pressed_of(answer, q.within) {
            Ok(pressed) => pressed,
            Err(why) => {
                let source = Source::NoAnswer {
                    risky: q.risky,
                    secs: q.within,
                };
                log(&q.name, source, &q.pending.program(), &q.pending.what());
                // Nobody is there: the next call hears so at once rather
                // than waiting a minute each.
                self.desktop.refused.note(&key, question, Instant::now());
                self.refuse(q.pending, why);
                return None;
            }
        };
        let decision = if q.risky {
            let decision = risky_answered(pressed);
            let done = decision == Decision::Allow;
            log(
                &q.name,
                Source::Risky(done),
                &q.pending.program(),
                &q.pending.what(),
            );
            decision
        } else {
            // Only what the human answers is kept here. The lists are read
            // afresh on every call, so taking a program off one takes
            // effect at once.
            let (grant, decision) = approved(self.grant_of(&q.pending), r.action, r.risky, pressed);
            self.desktop.grants.insert(key.clone(), grant);
            let level = match pressed {
                Some(0) => Some(Level::Operate),
                Some(1) => Some(Level::Look),
                _ => None,
            };
            log(
                &q.name,
                Source::Asked(level),
                &q.pending.program(),
                &q.pending.what(),
            );
            decision
        };
        match decision {
            Decision::Refuse(Refusal::Refused | Refusal::OnlyLook) => {
                self.desktop.refused.note(&key, question, Instant::now());
            }
            _ => self.desktop.refused.clear(&key, question),
        }
        match decision {
            Decision::Allow => self.allow(q.pending, false),
            Decision::Refuse(why) => self.refuse(q.pending, why),
            // The helper asks about what is risky in a program the app
            // saw running above it.
            Decision::AskRisky if q.pending.verified => self.allow(q.pending, false),
            Decision::AskRisky => {
                if let Some(why) = self.not_asked(&q.pending, Decision::AskRisky, false) {
                    self.refuse(q.pending, why);
                    return None;
                }
                q.risky = true;
                let r = &q.pending.request;
                self.desktop.asking =
                    Some((q.pending.session.clone(), program_key(&r.exe, &r.path)));
                return Some(q);
            }
            Decision::AskApprove => self.refuse(q.pending, Refusal::Refused),
        }
        None
    }
}

/// Asks about the waiting calls one after another, outside the app's
/// borrow. Runs from `WM_HORADRIC_DESKTOP`.
pub(super) fn ask_waiting(hwnd: HWND) {
    if ASKING.get() {
        return;
    }
    let Some(first) = with_app(App::next_question) else {
        // The app is busy in a loop of its own. Try again once it is not.
        post(hwnd.0 as isize, WM_HORADRIC_DESKTOP, 0);
        return;
    };
    ASKING.set(true);
    let mut next = first;
    while let Some(q) = next {
        let aside = Aside {
            avoid: windows_of(q.pending.request.hwnd),
            within: q.left,
        };
        let answer = ask_owned(&dialog(&q), &aside);
        next = match with_app(|app| app.answered(q, answer)) {
            Some(again @ Some(_)) => again,
            Some(None) => with_app(App::next_question).flatten(),
            // Not reachable from here, where nothing holds the app, but a
            // call left unanswered would hang the agent for the listener's
            // whole wait.
            None => None,
        };
    }
    ASKING.set(false);
}

/// Where the program asked about is on screen, with the dialog it shows
/// last, as left, top, right, bottom, so the question keeps clear of both.
/// Nothing when it is gone, hidden or minimised.
fn windows_of(hwnd: isize) -> Option<[i32; 4]> {
    let h = HWND(hwnd as *mut _);
    if hwnd == 0 {
        return None;
    }
    let rect = |h: HWND| {
        let mut r = RECT::default();
        unsafe { GetWindowRect(h, &mut r) }
            .ok()
            .map(|()| [r.left, r.top, r.right, r.bottom])
    };
    unsafe {
        if !IsWindow(Some(h)).as_bool() || !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
            return None;
        }
    }
    let main = rect(h)?;
    let popup = unsafe { GetLastActivePopup(h) };
    let shown = popup != h && unsafe { IsWindowVisible(popup) }.as_bool();
    Some(match rect(popup).filter(|_| shown) {
        Some(d) => union(main, d),
        None => main,
    })
}

/// The smallest rectangle holding both.
pub(super) fn union(a: [i32; 4], b: [i32; 4]) -> [i32; 4] {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

fn dialog(q: &Question) -> OwnedDialog {
    let r = &q.pending.request;
    let elevated = q.pending.elevated;
    let program = &shown_program(program_name(&r.exe), elevated);
    let window = match r.title.trim() {
        "" => String::new(),
        t => format!(" in the window \"{t}\""),
    };
    if q.risky {
        OwnedDialog {
            tone: Tone::Warning,
            title: format!("{} wants to change something in {program}", q.name),
            text: format!(
                "It wants to {}{window}.\n\nSession {}.\n\nThis is asked every time, even with \
                 {program} allowed, unless an administrator pre-approved this very action \
                 (horadric desktop approve, from an elevated shell). Unanswered after {} s, the \
                 session is told nobody answered.",
                what_or(&q.pending.what(), "act"),
                q.ident,
                q.within
            ),
            buttons: &RISKY,
            default: RISKY_DEFAULT,
        }
    } else {
        OwnedDialog {
            tone: Tone::Question,
            title: format!("{} wants to use {program}", q.name),
            text: format!(
                "First it wants to {}{window}.\n\nSession {}.\n\nLooking is screenshots and reading the \
                 window. Operating is clicking and typing in it, and in its dialogs. What you \
                 allow lasts until Horadric restarts.{} To allow it for every session ahead of \
                 time, list it: horadric desktop allow {}\"{}\" operate (or look){}. Unanswered \
                 after {} s, the session is told nobody answered.",
                what_or(&r.what, "look at it"),
                q.ident,
                if elevated {
                    " It runs as administrator, so Horadric's helper asks you once more itself, \
                     in a window of its own, before the first click or key and before each risky \
                     action."
                } else {
                    ""
                },
                if elevated { "--elevated " } else { "" },
                if r.path.is_empty() {
                    listed_name(&r.exe, "")
                } else {
                    &r.path
                },
                if elevated {
                    ", from an elevated shell"
                } else {
                    ""
                },
                q.within
            ),
            buttons: &APPROVE,
            default: APPROVE_DEFAULT,
        }
    }
}

fn what_or<'a>(what: &'a str, or: &'a str) -> &'a str {
    match what.trim() {
        "" => or,
        what => what,
    }
}

/// A [`Dialog`]'s owned parts, built inside the borrow and shown outside.
struct OwnedDialog {
    tone: Tone,
    title: String,
    text: String,
    buttons: &'static [&'static str],
    default: usize,
}

fn ask_owned(d: &OwnedDialog, aside: &Aside) -> Answer {
    let dialog = Dialog {
        tone: d.tone,
        title: &d.title,
        text: &d.text,
        buttons: d.buttons,
        default: d.default,
        check: None,
    };
    ask_aside(&dialog, aside)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looking_needs_at_least_see() {
        assert_eq!(
            decide(Grant::None, Action::See, false),
            Decision::AskApprove
        );
        assert_eq!(decide(Grant::See, Action::See, false), Decision::Allow);
        assert_eq!(decide(Grant::Operate, Action::See, false), Decision::Allow);
        // Looking changes nothing, so it is never risky.
        assert_eq!(decide(Grant::See, Action::See, true), Decision::Allow);
    }

    #[test]
    fn operating_needs_operate_and_a_risky_action_is_always_asked() {
        assert_eq!(
            decide(Grant::None, Action::Operate, false),
            Decision::AskApprove
        );
        assert_eq!(
            decide(Grant::See, Action::Operate, false),
            Decision::AskApprove
        );
        assert_eq!(
            decide(Grant::Operate, Action::Operate, false),
            Decision::Allow
        );
        assert_eq!(
            decide(Grant::None, Action::Operate, true),
            Decision::AskApprove
        );
        assert_eq!(
            decide(Grant::Operate, Action::Operate, true),
            Decision::AskRisky
        );
    }

    #[test]
    fn bypass_skips_only_the_risky_question_of_a_program_already_operated() {
        let risky = Decision::AskRisky;
        assert_eq!(bypassed(risky, true, false), (Decision::Allow, true));
        assert_eq!(bypassed(risky, false, false), (risky, false));
        // Never for a program that runs as administrator.
        assert_eq!(bypassed(risky, true, true), (risky, false));
        // It never lets a session into a program.
        let approve = Decision::AskApprove;
        for elevated in [true, false] {
            assert_eq!(bypassed(approve, true, elevated), (approve, false));
        }
        for bypass in [true, false] {
            assert_eq!(
                bypassed(Decision::Allow, bypass, false),
                (Decision::Allow, false)
            );
            for why in [Refusal::Refused, Refusal::OnlyLook] {
                let no = Decision::Refuse(why);
                assert_eq!(bypassed(no, bypass, false), (no, false));
            }
        }
        // A risky click in a program never approved is asked about the
        // program first, bypass or not.
        for grant in [Grant::None, Grant::See] {
            let asked = decide(grant, Action::Operate, true);
            assert_eq!(bypassed(asked, true, false), (Decision::AskApprove, false));
        }
    }

    #[test]
    fn a_session_is_told_apart_by_its_id_and_start() {
        // 10:30:00 now, started 28 minutes and 5 seconds ago.
        assert_eq!(started_clock(37_800, 1_685), "10:01");
        // Started before midnight.
        assert_eq!(started_clock(600, 1_200), "23:50");
        assert_eq!(
            ident_line("fix-plc-3721", Some("10:01")),
            "id fix-plc-3721, started 10:01"
        );
        assert_eq!(ident_line("fix-plc-3721", None), "id fix-plc-3721");
    }

    #[test]
    fn a_held_program_is_named_with_who_holds_it() {
        let said = busy_text(
            "Sysmac Studio",
            "Fix PLC",
            "id fix-plc-3721, started 10:01",
            false,
        );
        assert!(said.starts_with("Sysmac Studio is held by another session, \"Fix PLC\""));
        assert!(said.contains("id fix-plc-3721, started 10:01"));
        assert!(said.contains("still at work"));
        assert!(busy_text("a", "b", "c", true).contains("waiting for the human"));
        assert_eq!(
            program_key("SysmacStudio.exe", ""),
            program_key("SYSMACSTUDIO.EXE", r"C:\O\SysmacStudio.exe")
        );
    }

    #[test]
    fn the_list_never_lets_a_risky_action_through() {
        for own in [Grant::None, Grant::See, Grant::Operate] {
            for level in [None, Some(Level::Look), Some(Level::Operate)] {
                let grant = effective(own, level);
                assert_ne!(
                    decide(grant, Action::Operate, true),
                    Decision::Allow,
                    "{own:?} {level:?}"
                );
                assert!(!by_the_list(own, level, Action::Operate, true));
            }
        }
        assert_eq!(
            decide(
                effective(Grant::None, Some(Level::Operate)),
                Action::Operate,
                true
            ),
            Decision::AskRisky
        );
    }

    #[test]
    fn the_list_stands_in_for_the_approval_question() {
        let operate = Some(Level::Operate);
        let look = Some(Level::Look);
        assert_eq!(effective(Grant::None, operate), Grant::Operate);
        assert_eq!(effective(Grant::None, look), Grant::See);
        assert_eq!(effective(Grant::None, None), Grant::None);
        // What the human answered is never lowered by the list.
        assert_eq!(effective(Grant::Operate, look), Grant::Operate);
        assert!(by_the_list(Grant::None, operate, Action::Operate, false));
        assert!(by_the_list(Grant::None, look, Action::See, false));
        assert!(!by_the_list(Grant::None, look, Action::Operate, false));
        assert!(!by_the_list(Grant::See, look, Action::See, false));
        assert!(!by_the_list(Grant::None, None, Action::See, false));
    }

    #[test]
    fn the_list_is_matched_on_the_file_of_the_path() {
        assert_eq!(
            listed_name("other.exe", r"C:\O\SysmacStudio.exe"),
            "SysmacStudio.exe"
        );
        assert_eq!(listed_name("a.exe", ""), "a.exe");
        // A name is only a name: nothing in it says how a program runs.
        assert_eq!(
            listed_name("a.exe as administrator", ""),
            "a.exe as administrator"
        );
        assert_eq!(shown_program("a.exe", true), "a.exe (as administrator)");
        assert_eq!(shown_program("a.exe", false), "a.exe");
    }

    #[test]
    fn letting_it_operate_allows_and_still_asks_about_a_risky_one() {
        assert_eq!(
            approved(Grant::None, Action::Operate, false, Some(0)),
            (Grant::Operate, Decision::Allow)
        );
        assert_eq!(
            approved(Grant::See, Action::See, false, Some(0)),
            (Grant::Operate, Decision::Allow)
        );
        assert_eq!(
            approved(Grant::None, Action::Operate, true, Some(0)),
            (Grant::Operate, Decision::AskRisky)
        );
    }

    #[test]
    fn only_looking_refuses_an_operate_but_keeps_the_look() {
        assert_eq!(
            approved(Grant::None, Action::See, false, Some(1)),
            (Grant::See, Decision::Allow)
        );
        assert_eq!(
            approved(Grant::None, Action::Operate, false, Some(1)),
            (Grant::See, Decision::Refuse(Refusal::OnlyLook))
        );
    }

    #[test]
    fn refusing_or_putting_away_leaves_the_grant() {
        for pressed in [Some(2), None] {
            assert_eq!(
                approved(Grant::See, Action::Operate, false, pressed),
                (Grant::See, Decision::Refuse(Refusal::Refused))
            );
            assert_eq!(
                approved(Grant::None, Action::See, false, pressed),
                (Grant::None, Decision::Refuse(Refusal::Refused))
            );
        }
    }

    #[test]
    fn a_risky_action_goes_ahead_only_on_do_it() {
        assert_eq!(risky_answered(Some(0)), Decision::Allow);
        assert_eq!(risky_answered(Some(1)), Decision::Refuse(Refusal::Refused));
        assert_eq!(risky_answered(None), Decision::Refuse(Refusal::Refused));
    }

    #[test]
    fn the_default_button_of_every_question_refuses() {
        assert_eq!(APPROVE[APPROVE_DEFAULT], "Refuse");
        assert_eq!(RISKY[RISKY_DEFAULT], "Refuse");
    }

    #[test]
    fn a_question_held_back_says_for_how_long() {
        let held = Refusal::Held(listed::Question::Approve, 45);
        let told = refusal_text(held, Action::Operate, false, "a.exe");
        assert!(told.contains("another 45 s") && told.contains("a.exe"), "{told}");
        assert!(refusal_text(Refusal::Crowded, Action::See, false, "a.exe").contains("already wait"));
        assert_ne!(refusal_word(held), refusal_word(Refusal::Crowded));
    }

    #[test]
    fn the_notice_before_windows_asks_says_who_wants_what() {
        let body = json!({
            "op": "desktop.elevating",
            "window": {"exe": "SysmacStudio.exe", "title": "Sysmac Studio - Line 2"},
        });
        let (title, text) = elevating_notice("plc fix", &body);
        assert!(title.contains("administrator rights"), "{title}");
        // What the helper really does, and what it can not promise.
        assert!(text.contains("asks you itself") && text.contains("can not be limited"));
        assert!(!text.contains("approve each program and risky action here"));
        let mut with_path = body.clone();
        with_path["window"]["path"] = json!(r"C:\O\SysmacStudio.exe");
        assert!(elevating_notice("s", &with_path)
            .1
            .contains(r"for that one program (C:\O\SysmacStudio.exe)"));
        assert!(
            text.starts_with("plc fix wants to reach SysmacStudio, which runs as administrator"),
            "{text}"
        );
        assert!(!text.contains("development build"));
        let mut dev = body.clone();
        dev["dev_helper"] = json!(r"C:\src\target\debug\horadric.exe");
        assert!(elevating_notice("s", &dev)
            .1
            .contains(r"not the installed Horadric: C:\src\target\debug\horadric.exe."));
        let unknown = json!({"window": {"exe": "?", "title": "Setup"}});
        assert!(elevating_notice("s", &unknown)
            .1
            .contains("the program of \"Setup\""));
        assert!(elevating_notice("s", &json!({}))
            .1
            .contains("reach a program,"));
    }

    #[test]
    fn a_program_is_named_without_its_exe() {
        assert_eq!(program_name("Sysmac Studio.exe"), "Sysmac Studio");
        assert_eq!(program_name("NOTEPAD.EXE"), "NOTEPAD");
        assert_eq!(program_name("tool"), "tool");
        assert_eq!(program_name(".exe"), ".exe");
        assert_eq!(program_name("æble.exe"), "æble");
        assert_eq!(program_name("æ"), "æ");
    }

    #[test]
    fn a_call_s_word_for_administrator_is_read_as_a_field() {
        let mut body = json!({
            "op": "desktop.allow",
            "window": {"exe": "a.exe as administrator"},
            "action": "see",
        });
        // The name says nothing.
        assert!(!parse(&body).unwrap().administrator);
        body["administrator"] = json!(true);
        assert!(parse(&body).unwrap().administrator);
        body["administrator"] = json!("yes");
        assert!(!parse(&body).unwrap().administrator);
    }

    #[test]
    fn a_refusal_tells_the_agent_what_was_refused() {
        let exe = "Sysmac Studio.exe";
        let said = |risky, action| refusal_text(Refusal::Refused, action, risky, exe);
        assert!(said(false, Action::Operate)
            .starts_with("the human refused operating Sysmac Studio.exe. They are not asked"));
        assert!(said(false, Action::See)
            .starts_with("the human refused letting you see Sysmac Studio.exe. "));
        assert!(said(true, Action::Operate)
            .starts_with("the human refused this change in Sysmac Studio.exe. "));
        assert!(said(true, Action::Operate).contains("120 s"));
        assert!(refusal_text(Refusal::OnlyLook, Action::Operate, false, exe)
            .starts_with("the human allowed only looking at Sysmac Studio.exe"));
    }

    #[test]
    fn the_tile_says_what_is_being_operated() {
        assert_eq!(
            operating_line("Sysmac Studio", "click Button \"Transfer\""),
            "operating Sysmac Studio: click Button \"Transfer\""
        );
        assert_eq!(
            operating_line("Sysmac Studio", "  "),
            "operating Sysmac Studio"
        );
    }

    #[test]
    fn what_falls_back_when_not_said() {
        assert_eq!(what_or(" ", "look at it"), "look at it");
        assert_eq!(what_or("read the Watch", "look at it"), "read the Watch");
    }

    #[test]
    fn a_request_is_read_from_its_body() {
        let body = json!({
            "op": "desktop.allow",
            "window": {
                "hwnd": 1,
                "title": "Watch",
                "exe": "Sysmac Studio.exe",
                "path": r"C:\x\Sysmac Studio.exe",
                "pid": 2,
            },
            "action": "operate",
            "what": "click Button \"OK\"",
            "risky": true,
            "point": {"x": -1200, "y": 340},
            "reason": "Confirm the transfer the human asked for.",
        });
        assert_eq!(
            parse(&body),
            Ok(Request {
                title: "Watch".into(),
                exe: "Sysmac Studio.exe".into(),
                path: r"C:\x\Sysmac Studio.exe".into(),
                hwnd: 1,
                action: Action::Operate,
                what: "click Button \"OK\"".into(),
                risky: true,
                point: Some((-1200, 340)),
                reason: "Confirm the transfer the human asked for.".into(),
                act: Act::default(),
                administrator: false,
            })
        );
    }

    #[test]
    fn an_operate_call_s_parts_are_read_for_the_patterns() {
        let body = json!({
            "op": "desktop.allow",
            "window": {"exe": "SysmacStudio.exe", "title": "Proj - Sysmac Studio"},
            "action": "operate",
            "what": "click Button \"Yes\"",
            "risky": true,
            "act": {
                "kind": "click",
                "window": "Sysmac Studio",
                "control": "Button",
                "name": "Yes",
                "key": "",
                "texts": ["Delete the project?", 3],
            },
        });
        let r = parse(&body).unwrap();
        assert_eq!(
            r.act,
            Act {
                kind: Some(ActKind::Click),
                window: "Sysmac Studio".into(),
                control: "Button".into(),
                name: "Yes".into(),
                key: String::new(),
                texts: vec!["Delete the project?".into()],
            }
        );
        assert_eq!(
            what_where(&r.what, &r.act, &r.title),
            "click Button \"Yes\" in window \"Sysmac Studio\" saying \"Delete the project?\""
        );
        // Looking has no parts, whatever is sent.
        let mut see = body.clone();
        see["action"] = json!("see");
        assert_eq!(parse(&see).unwrap().act, Act::default());
        // An unknown kind matches no pattern.
        see["action"] = json!("operate");
        see["act"]["kind"] = json!("drag");
        assert_eq!(parse(&see).unwrap().act.kind, None);
    }

    #[test]
    fn the_window_is_said_only_when_it_is_not_the_main_one() {
        let act = Act {
            window: "Proj - Sysmac Studio".into(),
            ..Act::default()
        };
        assert_eq!(
            what_where(
                "click MenuItem \"Transfer...\"",
                &act,
                "Proj - Sysmac Studio"
            ),
            "click MenuItem \"Transfer...\""
        );
        assert_eq!(what_where("look", &Act::default(), "x"), "look");
    }

    #[test]
    fn a_question_waits_the_rest_of_its_time() {
        assert_eq!(time_left(60, Duration::ZERO), Duration::from_secs(60));
        assert_eq!(
            time_left(60, Duration::from_secs(20)),
            Duration::from_secs(40)
        );
        assert_eq!(
            time_left(60, Duration::from_secs(90)),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn nobody_answering_is_not_a_refusal() {
        assert_eq!(pressed_of(Answer::Pressed(1), 60), Ok(Some(1)));
        assert_eq!(pressed_of(Answer::Dismissed, 60), Ok(None));
        assert_eq!(
            pressed_of(Answer::Unanswered, 45),
            Err(Refusal::NoAnswer(45))
        );
        let told = refusal_text(
            Refusal::NoAnswer(45),
            Action::Operate,
            true,
            "SysmacStudio.exe",
        );
        assert!(told.starts_with("no technician answered"), "{told}");
        assert!(told.contains("within 45 s") && told.contains("ask in the chat"));
        assert!(!told.contains("refused"));
        assert_eq!(refusal_word(Refusal::NoAnswer(45)), "no answer");
    }

    #[test]
    fn a_question_keeps_clear_of_the_program_and_its_dialog() {
        assert_eq!(union([0, 0, 10, 10], [5, -5, 20, 8]), [0, -5, 20, 10]);
    }

    #[test]
    fn a_request_without_a_point_or_reason_has_none() {
        let mut body = json!({
            "op": "desktop.allow",
            "window": {"exe": "a.exe"},
            "action": "see",
            "point": null,
        });
        let r = parse(&body).unwrap();
        assert_eq!((r.point, r.reason.as_str(), r.hwnd), (None, "", 0));
        // From before paths were sent.
        assert_eq!(r.path, "");
        body["point"] = json!({"x": 5});
        assert_eq!(parse(&body).unwrap().point, None);
        body["point"] = json!({"x": 5, "y": 1e12});
        assert_eq!(parse(&body).unwrap().point, None);
    }

    #[test]
    fn a_done_is_read_from_its_body() {
        let body = json!({
            "op": "desktop.done",
            "window": {"exe": "a.exe"},
            "what": "click OK",
            "ok": false,
            "outcome": "the button was gone",
        });
        assert_eq!(
            parse_done(&body),
            Done {
                what: "click OK".into(),
                ok: false,
                outcome: "the button was gone".into(),
            }
        );
        let bare = parse_done(&json!({"op": "desktop.done"}));
        assert!(bare.ok && bare.what.is_empty() && bare.outcome.is_empty());
    }

    #[test]
    fn a_refusal_leaves_a_short_word_in_the_history() {
        assert_eq!(refusal_word(Refusal::Refused), "refused by the human");
        assert_ne!(
            refusal_word(Refusal::OnlyLook),
            refusal_word(Refusal::Refused)
        );
    }

    #[test]
    fn a_request_missing_what_matters_is_refused() {
        let ok = json!({"op": "desktop.allow", "window": {"exe": "a.exe"}, "action": "see"});
        let risky = parse(&ok).map(|r| r.risky);
        assert_eq!(risky, Ok(false));
        let mut no_exe = ok.clone();
        no_exe["window"]["exe"] = json!(" ");
        assert!(parse(&no_exe).is_err());
        let mut no_action = ok.clone();
        no_action["action"] = json!("delete");
        assert!(parse(&no_action).is_err());
        let mut other = ok.clone();
        other["op"] = json!("desktop.click");
        assert!(parse(&other).is_err());
        assert!(parse(&json!({"op": "desktop.allow", "action": "see"})).is_err());
    }
}
