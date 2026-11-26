//! The app's side of an agent using a desktop program, through `horadric
//! mcp`. The MCP process does the seeing and the clicking; before each call
//! it asks here whether it may, and the human decides.
//!
//! What the human allows is kept per session and per program, by its exe,
//! for the life of the app: a program's dialogs are the same program, so
//! allowing Sysmac Studio covers its Transfer dialog too. A risky action is
//! never kept. Each one is asked, since one wrong click there can stop a
//! machine, unless the human pre-approved that very action on the list.
//!
//! The human can also list programs ahead of time, in
//! `desktop-programs.json` (see [`horadric_core::desktop_programs`]). A
//! listed program is let in that far for every session without the
//! approval question, and the file is read again whenever it changes. A
//! risky action is asked all the same, unless it matches one of the
//! program's pre-approved patterns. Each grant from the list is shown once
//! per session and program and written to `desktop.log`, with the answers
//! to the questions and every pre-approved risky action beside it.
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
//! the questions. It is never kept, and what it lets through unasked is
//! still held for Esc, which stops it and turns bypass off.

use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::time::{Instant, SystemTime};

use std::time::Duration;

use horadric_core::desktop_programs::{self as listed, Act, Entry, Kind as ActKind, Level, Source};
use horadric_hooks::listener::{BrowserCall, Reply};
use serde_json::{json, Value};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::WindowsAndMessaging::{
    GetLastActivePopup, GetWindowRect, IsIconic, IsWindow, IsWindowVisible,
};

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

/// What bypass makes of a decision: a question it would ask becomes an
/// allowed call, true when held for the human's Esc. Nothing else moves:
/// what is allowed goes as ever, and a refusal stays one.
pub(super) fn bypassed(decision: Decision, bypass: bool) -> (Decision, bool) {
    match decision {
        Decision::AskApprove | Decision::AskRisky if bypass => (Decision::Allow, true),
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

/// The button Enter presses at first: the least that does what was asked.
pub(super) fn approve_default(action: Action) -> usize {
    match action {
        Action::See => 1,
        Action::Operate => 0,
    }
}

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
    match (why, action) {
        (Refusal::OnlyLook, _) => format!(
            "the human allowed only looking at {exe}, not operating it; ask them in the chat if \
             you need to operate it"
        ),
        (Refusal::Refused, Action::See) => format!("the human refused letting you see {exe}"),
        (Refusal::Refused, Action::Operate) if risky => {
            format!("the human refused this change in {exe}")
        }
        (Refusal::Refused, Action::Operate) => format!("the human refused operating {exe}"),
        (Refusal::NoAnswer(secs), _) => {
            let what = if risky {
                "this change"
            } else {
                "letting you use it"
            };
            format!(
                "no technician answered Horadric's question about {what} in {exe} within \
                 {secs} s, so nothing was done. Nobody may be at this machine: ask in the chat. \
                 If this action is safe to do without asking, the human can pre-approve it on \
                 the list (horadric desktop approve)"
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
    (
        "Windows will ask for administrator rights".to_string(),
        format!(
            "{session} wants to reach {program}, which runs as administrator. Horadric's helper \
             needs administrator rights for that: Yes lets it, and you still approve each \
             program and risky action here; No keeps it out."
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

/// The key a program is held by: its plain exe name, whether reached
/// through the helper or not.
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
}

impl Pending {
    fn key(&self) -> (String, String) {
        grant_key(&self.session, &self.request.exe)
    }

    /// The program as the log names it: its path when known.
    fn program(&self) -> &str {
        let r = &self.request;
        if r.path.is_empty() {
            &r.exe
        } else {
            &r.path
        }
    }

    /// What it does, as the log says it.
    fn what(&self) -> String {
        let r = &self.request;
        what_where(&r.what, &r.act, &r.title)
    }

    fn refuse(&self, why: Refusal) {
        let r = &self.request;
        self.reply
            .send(json!({ "error": refusal_text(why, r.action, r.risky, &r.exe) }));
    }
}

fn grant_key(session: &str, exe: &str) -> (String, String) {
    (session.to_string(), exe.to_lowercase())
}

/// The exe name the list is matched against. A program reached through
/// the helper is named "X.exe as administrator", which keeps its answered
/// grant apart, but it is the same program on the list.
pub(super) fn listed_name<'a>(exe: &'a str, path: &'a str) -> &'a str {
    match path.rsplit(['\\', '/']).next() {
        Some(name) if !name.trim().is_empty() => name,
        _ => exe.strip_suffix(" as administrator").unwrap_or(exe),
    }
}

/// The list as last read, and the file's stamp then, so it is read again
/// only when it changes.
#[derive(Default)]
struct Listed {
    stamp: Option<(SystemTime, u64)>,
    list: listed::List,
}

/// What the human allowed, and the calls waiting to be asked.
#[derive(Default)]
pub(super) struct Desktop {
    grants: HashMap<(String, String), Grant>,
    waiting: VecDeque<Pending>,
    list: Listed,
    /// The session, program and level each grant from the list was shown
    /// for, so it is shown once and not on every screenshot.
    told: HashSet<(String, String, Level)>,
    /// The session that last operated each program, by [`program_key`].
    drivers: HashMap<String, String>,
    /// The session and program of the question on screen.
    asking: Option<(String, String)>,
}

impl Desktop {
    /// Reads the list again if its file changed. A list that can not be
    /// read counts as empty, so the human is asked, and says why once per
    /// version of the file.
    fn refresh(&mut self) -> Option<String> {
        let file = horadric_hooks::tasks::desktop_programs_file()?;
        let stamp = std::fs::metadata(&file)
            .ok()
            .map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()));
        if stamp == self.list.stamp {
            return None;
        }
        self.list.stamp = stamp;
        let text = match stamp {
            Some(_) => std::fs::read_to_string(&file).unwrap_or_default(),
            None => String::new(),
        };
        match listed::parse(&text) {
            Ok(list) => {
                self.list.list = list;
                None
            }
            Err(why) => {
                self.list.list = listed::List::default();
                Some(format!("{}: {why}", file.display()))
            }
        }
    }

    fn entry_of(&self, r: &Request) -> Option<&Entry> {
        let name = listed_name(&r.exe, &r.path);
        listed::find(&self.list.list.programs, &r.path, name)
    }

    fn level_of(&self, r: &Request) -> Option<Level> {
        self.entry_of(r).map(|e| e.level)
    }

    /// How long a question waits for the human, in seconds.
    fn answer_within(&self) -> u64 {
        self.list.list.answer_within()
    }
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
            let (title, text) = elevating_notice(&self.name_of(&call.session), &call.body);
            self.toasts.show(Kind::Waiting, &title, &text);
            return call.reply.send(json!({}));
        }
        let request = match parse(&call.body) {
            Ok(r) => r,
            Err(e) => return call.reply.send(json!({ "error": e })),
        };
        let pending = Pending {
            session: call.session,
            request,
            reply: call.reply,
            since: Instant::now(),
        };
        if let Some(why) = self.desktop.refresh() {
            let name = self.name_of(&pending.session);
            log(&name, Source::Unreadable, &why, "");
            self.toasts.show(
                Kind::Failed,
                "The list of allowed desktop programs can not be read",
                &format!("{why}. Horadric asks instead. `horadric desktop list` shows it."),
            );
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
        if let (Decision::Allow, held) = bypassed(asked, ghost::bypass()) {
            return self.allow(pending, held);
        }
        self.desktop.waiting.push_back(pending);
        post(self.notify.0 as isize, WM_HORADRIC_DESKTOP, 0);
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

    /// The pattern on the list that lets this risky call go ahead unasked,
    /// in words, when the session may operate the program.
    fn pre_approval(&self, p: &Pending, own: Grant) -> Option<String> {
        let r = &p.request;
        let entry = self.desktop.entry_of(r)?;
        if !r.risky || effective(own, Some(entry.level)) != Grant::Operate {
            return None;
        }
        listed::pre_approved(entry, &r.act, &self.name_of(&p.session)).map(|a| a.describe())
    }

    /// What becomes of a call with what the human answered and what the
    /// list allows, before bypass. Going ahead on the list alone is shown
    /// once and logged, so the human can tell it was not asked; a risky
    /// action going ahead on a pre-approved pattern is logged every time.
    fn decision_for(&mut self, p: &Pending) -> Decision {
        let own = self.grant_of(p);
        let level = self.desktop.level_of(&p.request);
        let r = &p.request;
        let pre = self.pre_approval(p, own);
        let risky = r.risky && pre.is_none();
        let decision = decide(effective(own, level), r.action, risky);
        if let Some(pattern) = pre {
            let name = self.name_of(&p.session);
            log(&name, Source::PreApproved(pattern), p.program(), &p.what());
        }
        if let Some(level) = level.filter(|_| by_the_list(own, level, r.action, risky)) {
            let told = (p.session.clone(), p.program().to_lowercase(), level);
            if self.desktop.told.insert(told) {
                let name = self.name_of(&p.session);
                let program = program_name(listed_name(&r.exe, &r.path));
                log(&name, Source::List(level), p.program(), &p.what());
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
            log(&name, Source::Bypass, p.program(), &p.what());
        }
        let r = p.request;
        let name = self.name_of(&p.session);
        let ident = self.ident_of(&p.session);
        let program = program_name(&r.exe).to_string();
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
    /// those that what was allowed since now covers.
    fn next_question(&mut self) -> Option<Question> {
        while let Some(p) = self.desktop.waiting.pop_front() {
            if !p.reply.wanted() {
                // Its agent gave up waiting behind other questions, and
                // was told the call failed: asking now would act late.
                continue;
            }
            let asked = self.decision_for(&p);
            match bypassed(asked, ghost::bypass()) {
                (Decision::Allow, held) => self.allow(p, held),
                (Decision::Refuse(why), _) => self.refuse(p, why),
                (ask, _) => {
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
        let pressed = match pressed_of(answer, q.within) {
            Ok(pressed) => pressed,
            Err(why) => {
                let source = Source::NoAnswer {
                    risky: q.risky,
                    secs: q.within,
                };
                log(&q.name, source, q.pending.program(), &q.pending.what());
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
                q.pending.program(),
                &q.pending.what(),
            );
            decision
        } else {
            // Only what the human answers is kept here. The list is read
            // afresh on every call, so taking a program off it takes
            // effect at once.
            let (grant, decision) = approved(self.grant_of(&q.pending), r.action, r.risky, pressed);
            self.desktop.grants.insert(q.pending.key(), grant);
            let level = match pressed {
                Some(0) => Some(Level::Operate),
                Some(1) => Some(Level::Look),
                _ => None,
            };
            log(
                &q.name,
                Source::Asked(level),
                q.pending.program(),
                &q.pending.what(),
            );
            decision
        };
        match decision {
            Decision::Allow => self.allow(q.pending, false),
            Decision::Refuse(why) => self.refuse(q.pending, why),
            Decision::AskRisky => {
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
    let program = program_name(&r.exe);
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
                 {program} allowed, unless this very action is pre-approved on the list \
                 (horadric desktop approve). Unanswered after {} s, the session is told nobody \
                 answered.",
                what_or(&q.pending.what(), "act"),
                q.ident,
                q.within
            ),
            buttons: &RISKY,
            default: 1,
        }
    } else {
        OwnedDialog {
            tone: Tone::Question,
            title: format!("{} wants to use {program}", q.name),
            text: format!(
                "First it wants to {}{window}.\n\nSession {}.\n\nLooking is screenshots and reading the \
                 window. Operating is clicking and typing in it, and in its dialogs. What you \
                 allow lasts until Horadric restarts. To allow it for every session ahead of \
                 time, list it: horadric desktop allow \"{}\" operate (or look). Unanswered \
                 after {} s, the session is told nobody answered.",
                what_or(&r.what, "look at it"),
                q.ident,
                if r.path.is_empty() {
                    listed_name(&r.exe, "")
                } else {
                    &r.path
                },
                q.within
            ),
            buttons: &APPROVE,
            default: approve_default(r.action),
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
    fn bypass_lets_a_question_through_held_and_leaves_the_rest() {
        for ask in [Decision::AskApprove, Decision::AskRisky] {
            assert_eq!(bypassed(ask, true), (Decision::Allow, true));
            assert_eq!(bypassed(ask, false), (ask, false));
        }
        for bypass in [true, false] {
            assert_eq!(bypassed(Decision::Allow, bypass), (Decision::Allow, false));
            for why in [Refusal::Refused, Refusal::OnlyLook] {
                let no = Decision::Refuse(why);
                assert_eq!(bypassed(no, bypass), (no, false));
            }
        }
        // A risky click in a program never approved goes through bypass.
        let asked = decide(Grant::None, Action::Operate, true);
        assert_eq!(bypassed(asked, true), (Decision::Allow, true));
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
        // The same program, run as administrator or not.
        assert_eq!(
            program_key("SysmacStudio.exe as administrator", ""),
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
    fn the_list_is_matched_on_the_plain_exe_name() {
        assert_eq!(
            listed_name(
                "SysmacStudio.exe as administrator",
                r"C:\O\SysmacStudio.exe"
            ),
            "SysmacStudio.exe"
        );
        assert_eq!(
            listed_name("SysmacStudio.exe as administrator", ""),
            "SysmacStudio.exe"
        );
        assert_eq!(listed_name("a.exe", ""), "a.exe");
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
    fn the_default_button_is_the_least_that_does_it() {
        assert_eq!(APPROVE[approve_default(Action::See)], "Only let it look");
        assert_eq!(APPROVE[approve_default(Action::Operate)], "Let it operate");
    }

    #[test]
    fn the_notice_before_windows_asks_says_who_wants_what() {
        let body = json!({
            "op": "desktop.elevating",
            "window": {"exe": "SysmacStudio.exe", "title": "Sysmac Studio - Line 2"},
        });
        let (title, text) = elevating_notice("plc fix", &body);
        assert!(title.contains("administrator rights"), "{title}");
        assert!(
            text.starts_with("plc fix wants to reach SysmacStudio, which runs as administrator"),
            "{text}"
        );
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
    fn grants_are_kept_by_exe_whatever_its_case() {
        assert_eq!(
            grant_key("s1", "Sysmac Studio.exe"),
            grant_key("s1", "SYSMAC STUDIO.EXE")
        );
        assert_ne!(
            grant_key("s1", "Sysmac Studio.exe"),
            grant_key("s2", "Sysmac Studio.exe")
        );
    }

    #[test]
    fn a_refusal_tells_the_agent_what_was_refused() {
        let exe = "Sysmac Studio.exe";
        assert_eq!(
            refusal_text(Refusal::Refused, Action::Operate, false, exe),
            "the human refused operating Sysmac Studio.exe"
        );
        assert_eq!(
            refusal_text(Refusal::Refused, Action::See, false, exe),
            "the human refused letting you see Sysmac Studio.exe"
        );
        assert_eq!(
            refusal_text(Refusal::Refused, Action::Operate, true, exe),
            "the human refused this change in Sysmac Studio.exe"
        );
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
