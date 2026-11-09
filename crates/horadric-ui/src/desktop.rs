//! The app's side of an agent using a desktop program, through `horadric
//! mcp`. The MCP process does the seeing and the clicking; before each call
//! it asks here whether it may, and the human decides.
//!
//! What the human allows is kept per session and per program, by its exe,
//! for the life of the app: a program's dialogs are the same program, so
//! allowing Sysmac Studio covers its Transfer dialog too. A risky action is
//! never kept. Each one is asked, since one wrong click there can stop a
//! machine.
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
use std::collections::{HashMap, VecDeque};

use horadric_hooks::listener::{BrowserCall, Reply};
use serde_json::{json, Value};
use windows::Win32::Foundation::HWND;

use super::ghost::{self, Allowed};
use super::{ask, post, with_app, App, WM_HORADRIC_DESKTOP};
use crate::dialog::{Dialog, Tone};
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
    }
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
    }
}

/// A call as `horadric mcp` sent it.
#[derive(Debug, PartialEq)]
pub(super) struct Request {
    pub title: String,
    pub exe: String,
    /// The program's window, for the screen the Thinking plate goes on.
    pub hwnd: isize,
    pub action: Action,
    pub what: String,
    pub risky: bool,
    /// Where the click goes, in physical screen pixels, for the ghost.
    pub point: Option<(i32, i32)>,
    /// The agent's own sentence on why.
    pub reason: String,
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
    })
}

/// A call waiting for the human, with where its answer goes.
pub(super) struct Pending {
    session: String,
    request: Request,
    reply: Reply,
}

impl Pending {
    fn key(&self) -> (String, String) {
        grant_key(&self.session, &self.request.exe)
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

/// What the human allowed, and the calls waiting to be asked.
#[derive(Default)]
pub(super) struct Desktop {
    grants: HashMap<(String, String), Grant>,
    waiting: VecDeque<Pending>,
}

/// A question about to be asked, outside the app's borrow.
pub(super) struct Question {
    pending: Pending,
    /// The session's name, for the title.
    name: String,
    risky: bool,
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
        let request = match parse(&call.body) {
            Ok(r) => r,
            Err(e) => return call.reply.send(json!({ "error": e })),
        };
        let pending = Pending {
            session: call.session,
            request,
            reply: call.reply,
        };
        let grant = self.grant_of(&pending);
        let asked = decide(grant, pending.request.action, pending.request.risky);
        if let (Decision::Allow, held) = bypassed(asked, ghost::bypass()) {
            return self.allow(pending, held);
        }
        self.desktop.waiting.push_back(pending);
        post(self.notify.0 as isize, WM_HORADRIC_DESKTOP, 0);
    }

    fn grant_of(&self, p: &Pending) -> Grant {
        self.desktop
            .grants
            .get(&p.key())
            .copied()
            .unwrap_or_default()
    }

    /// Lets a call go ahead. Operating is shown, so the human always knows
    /// a program is being driven; looking is not, since screenshots come
    /// many to a turn.
    ///
    /// The Thinking plate and the frame round the screens say the rest, so
    /// the toast comes once, as control begins. `unasked` is a call only
    /// bypass let through, held for Esc before it is answered.
    fn allow(&mut self, p: Pending, unasked: bool) {
        let r = p.request;
        let name = self.name_of(&p.session);
        let program = program_name(&r.exe).to_string();
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
            let grant = self.grant_of(&p);
            let asked = decide(grant, p.request.action, p.request.risky);
            match bypassed(asked, ghost::bypass()) {
                (Decision::Allow, held) => self.allow(p, held),
                (Decision::Refuse(why), _) => self.refuse(p, why),
                (ask, _) => {
                    return Some(Question {
                        name: self.name_of(&p.session),
                        risky: ask == Decision::AskRisky,
                        pending: p,
                    })
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
    fn answered(&mut self, mut q: Question, pressed: Option<usize>) -> Option<Question> {
        let r = &q.pending.request;
        let decision = if q.risky {
            risky_answered(pressed)
        } else {
            let (grant, decision) = approved(self.grant_of(&q.pending), r.action, r.risky, pressed);
            self.desktop.grants.insert(q.pending.key(), grant);
            decision
        };
        match decision {
            Decision::Allow => self.allow(q.pending, false),
            Decision::Refuse(why) => self.refuse(q.pending, why),
            Decision::AskRisky => {
                q.risky = true;
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
        let pressed = ask_owned(&dialog(&q));
        next = match with_app(|app| app.answered(q, pressed)) {
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
                "It wants to {}{window}.\n\nThis is asked every time, even with {program} \
                 allowed.",
                what_or(&r.what, "act")
            ),
            buttons: &RISKY,
            default: 1,
        }
    } else {
        OwnedDialog {
            tone: Tone::Question,
            title: format!("{} wants to use {program}", q.name),
            text: format!(
                "First it wants to {}{window}.\n\nLooking is screenshots and reading the \
                 window. Operating is clicking and typing in it, and in its dialogs. What you \
                 allow lasts until Horadric restarts.",
                what_or(&r.what, "look at it")
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

fn ask_owned(d: &OwnedDialog) -> Option<usize> {
    ask(&Dialog {
        tone: d.tone,
        title: &d.title,
        text: &d.text,
        buttons: d.buttons,
        default: d.default,
        check: None,
    })
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
            "window": {"hwnd": 1, "title": "Watch", "exe": "Sysmac Studio.exe", "pid": 2},
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
                hwnd: 1,
                action: Action::Operate,
                what: "click Button \"OK\"".into(),
                risky: true,
                point: Some((-1200, 340)),
                reason: "Confirm the transfer the human asked for.".into(),
            })
        );
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
