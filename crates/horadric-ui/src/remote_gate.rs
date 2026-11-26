//! The human's say on an agent watching and driving another machine's
//! desktop. `horadric mcp` asks here before a remote tool does anything,
//! and the human is asked once per project and destination.
//!
//! The answers live in memory only: a file could be edited by the agent
//! the question is about. A yes holds until Horadric restarts.

use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use horadric_hooks::listener::Reply;
use serde_json::{json, Value};

use crate::browse::error;
use crate::vault::run::{self, Answer};

/// How long a no holds, so an agent that asks again at once does not put
/// the question back on the human's screen.
const REFUSED_FOR: Duration = Duration::from_secs(5 * 60);

/// How long the human has to answer. The agent's call waits 90 seconds,
/// so this must end well before that.
const WAIT: Duration = Duration::from_secs(60);

/// The longest destination shown in the question.
const SHOWN: usize = 100;

#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Allowed,
    Refused,
    AlreadyAsking,
    Ask,
}

/// A project's key and a destination.
type Pair = (String, String);

struct Gate {
    allowed: Vec<Pair>,
    refused: Vec<(Pair, Instant)>,
    asking: Vec<Pair>,
}

static GATE: Mutex<Gate> = Mutex::new(Gate::new());

fn gate() -> std::sync::MutexGuard<'static, Gate> {
    GATE.lock().unwrap_or_else(|e| e.into_inner())
}

impl Gate {
    const fn new() -> Gate {
        Gate {
            allowed: Vec::new(),
            refused: Vec::new(),
            asking: Vec::new(),
        }
    }

    /// What to do about `project` reaching `dest` at `now`. `Ask` marks
    /// the question as open, so the caller must end it with [`Gate::yes`],
    /// [`Gate::no`] or [`Gate::unanswered`].
    fn decide(&mut self, project: &str, dest: &str, now: Instant) -> Decision {
        let is = |p: &Pair| p.0 == project && p.1 == dest;
        if self.allowed.iter().any(is) {
            return Decision::Allowed;
        }
        self.refused
            .retain(|(_, at)| now.saturating_duration_since(*at) < REFUSED_FOR);
        if self.refused.iter().any(|(p, _)| is(p)) {
            return Decision::Refused;
        }
        // One question per project at a time, whatever the destination:
        // an agent naming a new one each call must not pile questions up
        // until the human presses yes to make them stop.
        if self.asking.iter().any(|p| p.0 == project) {
            return Decision::AlreadyAsking;
        }
        self.asking.push((project.to_string(), dest.to_string()));
        Decision::Ask
    }

    fn unanswered(&mut self, project: &str, dest: &str) {
        self.asking.retain(|p| !(p.0 == project && p.1 == dest));
    }

    fn yes(&mut self, project: &str, dest: &str) {
        self.unanswered(project, dest);
        self.refused
            .retain(|(p, _)| !(p.0 == project && p.1 == dest));
        if !self.allowed.iter().any(|p| p.0 == project && p.1 == dest) {
            self.allowed.push((project.to_string(), dest.to_string()));
        }
    }

    fn no(&mut self, project: &str, dest: &str, now: Instant) {
        self.unanswered(project, dest);
        self.refused
            .retain(|(p, _)| !(p.0 == project && p.1 == dest));
        self.refused
            .push(((project.to_string(), dest.to_string()), now));
    }

    /// Drops the yes for `dest` in every project. A no stays: closing a
    /// remote desktop must not bring a refused question back early.
    fn forget(&mut self, dest: &str) {
        self.allowed.retain(|p| p.1 != dest);
    }
}

/// The destination a call names, as the gate keeps it: trimmed and
/// otherwise exactly as given, since ssh tells `Host` and `host` apart in
/// its config. None when there is none.
fn destination(body: &Value) -> Option<&str> {
    let dest = body["ssh"].as_str()?.trim();
    (!dest.is_empty()).then_some(dest)
}

/// The destination as the question shows it. The agent chooses it, so it
/// must not be able to add lines to the question, turn its text around or
/// push the buttons off the screen.
fn shown(dest: &str) -> String {
    let unsafe_char = |c: char| {
        c.is_control()
            || matches!(c,
                '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
                | '\u{feff}')
    };
    let mut out: String = dest
        .chars()
        .take(SHOWN)
        .map(|c| if unsafe_char(c) { '?' } else { c })
        .collect();
    if dest.chars().count() > SHOWN {
        out.push('\u{2026}');
    }
    out
}

fn refused(dest: &str) -> Value {
    error(&format!(
        "the human said no to driving {} from this project; ask them before trying again",
        shown(dest)
    ))
}

/// What a call gets without the human being asked, or the destination to
/// ask about. None when the call is not for the gate.
enum Step {
    Answer(Value),
    Ask(String),
}

fn step(gate: &mut Gate, project: &str, body: &Value, now: Instant) -> Option<Step> {
    let op = body["op"].as_str()?;
    if op != "remote_allow" && op != "remote_forget" {
        return None;
    }
    let Some(dest) = destination(body) else {
        return Some(Step::Answer(error("say which machine: ssh")));
    };
    if op == "remote_forget" {
        gate.forget(dest);
        return Some(Step::Answer(json!({ "forgotten": true })));
    }
    Some(match gate.decide(project, dest, now) {
        Decision::Allowed => Step::Answer(json!({ "allowed": true })),
        Decision::Refused => Step::Answer(refused(dest)),
        Decision::AlreadyAsking => Step::Answer(error(
            "the human is being asked already about a remote desktop for this project; wait \
             for their answer, then try again",
        )),
        Decision::Ask => Step::Ask(dest.to_string()),
    })
}

/// Takes `remote_allow` and `remote_forget`, and gives the reply back for
/// any other call. Call it on the UI thread, and only for a session that
/// showed its key: the question names the session as the one asking.
pub fn call(project: &str, session: &str, body: &Value, reply: Reply) -> Option<Reply> {
    let step = step(&mut gate(), project, body, Instant::now());
    let dest = match step {
        None => return Some(reply),
        Some(Step::Answer(a)) => {
            reply.send(a);
            return None;
        }
        Some(Step::Ask(dest)) => dest,
    };
    let who = run::asker(session, Some(&crate::project::project_name(project)));
    let text = format!(
        "{who} wants to watch and drive the desktop of {}: it can see the screen, click and \
         type there.\n\nAllow holds for this project until Horadric restarts.",
        shown(&dest)
    );
    let asked = run::ask_allow(session, "Remote desktop", &text);
    let project = project.to_string();
    let waiter = {
        let (project, dest, reply) = (project.clone(), dest.clone(), reply.clone());
        move || {
            let answer = asked.wait(WAIT);
            let mut gate = gate();
            match answer {
                Some(Answer::Once | Answer::Always) => {
                    gate.yes(&project, &dest);
                    reply.send(json!({ "allowed": true }));
                }
                Some(Answer::No) => {
                    gate.no(&project, &dest, Instant::now());
                    reply.send(refused(&dest));
                }
                None => {
                    gate.unanswered(&project, &dest);
                    reply.send(error(
                        "the human did not answer whether this project may drive that \
                         desktop; ask them, then try again",
                    ));
                }
            }
        }
    };
    if thread::Builder::new()
        .name("remote-ask".into())
        .spawn(waiter)
        .is_err()
    {
        // Nobody hears the answer now, so the pair must not stay open for
        // good. The question may still show; its answer changes nothing.
        gate().unanswered(&project, &dest);
        reply.send(error("Horadric could not ask the human"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(dest: &str) -> Value {
        json!({ "op": "remote_allow", "ssh": dest })
    }

    #[test]
    fn remote_gate_asks_once_then_remembers_a_yes() {
        let mut g = Gate::new();
        let now = Instant::now();
        assert_eq!(g.decide("p", "me@box", now), Decision::Ask);
        assert_eq!(g.decide("p", "me@box", now), Decision::AlreadyAsking);
        g.yes("p", "me@box");
        assert_eq!(g.decide("p", "me@box", now), Decision::Allowed);
        // Another project, or another machine, is another question.
        assert_eq!(g.decide("q", "me@box", now), Decision::Ask);
        g.unanswered("q", "me@box");
        assert_eq!(g.decide("p", "me@other", now), Decision::Ask);
    }

    #[test]
    fn remote_gate_holds_a_no_for_five_minutes() {
        let mut g = Gate::new();
        let now = Instant::now();
        assert_eq!(g.decide("p", "box", now), Decision::Ask);
        g.no("p", "box", now);
        assert_eq!(g.decide("p", "box", now), Decision::Refused);
        let almost = now + REFUSED_FOR - Duration::from_secs(1);
        assert_eq!(g.decide("p", "box", almost), Decision::Refused);
        // The no is this project's and this machine's only.
        assert_eq!(g.decide("q", "box", almost), Decision::Ask);
        assert_eq!(g.decide("p", "box", now + REFUSED_FOR), Decision::Ask);
        g.yes("p", "box");
        assert_eq!(g.decide("p", "box", now + REFUSED_FOR), Decision::Allowed);
    }

    #[test]
    fn remote_gate_remembers_nothing_when_unanswered() {
        let mut g = Gate::new();
        let now = Instant::now();
        assert_eq!(g.decide("p", "box", now), Decision::Ask);
        g.unanswered("p", "box");
        assert_eq!(g.decide("p", "box", now), Decision::Ask);
    }

    #[test]
    fn remote_gate_asks_one_question_per_project_at_a_time() {
        let mut g = Gate::new();
        let now = Instant::now();
        assert_eq!(g.decide("p", "a", now), Decision::Ask);
        assert_eq!(g.decide("p", "b", now), Decision::AlreadyAsking);
        assert_eq!(g.decide("q", "b", now), Decision::Ask);
        g.no("p", "a", now);
        assert_eq!(g.decide("p", "b", now), Decision::Ask);
    }

    #[test]
    fn remote_gate_forgets_a_destination_in_every_project() {
        let mut g = Gate::new();
        let now = Instant::now();
        for (p, d) in [("p", "box"), ("q", "box"), ("p", "other")] {
            assert_eq!(g.decide(p, d, now), Decision::Ask);
            g.yes(p, d);
        }
        g.forget("box");
        assert_eq!(g.decide("p", "other", now), Decision::Allowed);
        assert_eq!(g.decide("p", "box", now), Decision::Ask);
        assert_eq!(g.decide("q", "box", now), Decision::Ask);
    }

    #[test]
    fn remote_gate_forget_keeps_a_no() {
        let mut g = Gate::new();
        let now = Instant::now();
        g.decide("p", "box", now);
        g.no("p", "box", now);
        g.forget("box");
        assert_eq!(g.decide("p", "box", now), Decision::Refused);
    }

    #[test]
    fn remote_gate_compares_the_destination_trimmed_and_exactly() {
        assert_eq!(destination(&allow("  me@box \n")), Some("me@box"));
        assert_eq!(destination(&allow("   ")), None);
        assert_eq!(destination(&json!({ "op": "remote_allow" })), None);
        assert_eq!(destination(&json!({ "ssh": 7 })), None);
        let mut g = Gate::new();
        let now = Instant::now();
        assert!(matches!(
            step(&mut g, "p", &allow(" me@box "), now),
            Some(Step::Ask(d)) if d == "me@box"
        ));
        g.yes("p", "me@box");
        assert!(matches!(
            step(&mut g, "p", &allow("me@box"), now),
            Some(Step::Answer(a)) if a == json!({ "allowed": true })
        ));
        // No case folding: ssh may know `Box` as another machine.
        assert!(matches!(
            step(&mut g, "p", &allow("me@Box"), now),
            Some(Step::Ask(_))
        ));
    }

    #[test]
    fn remote_gate_answers_its_two_ops_and_no_other() {
        let mut g = Gate::new();
        let now = Instant::now();
        assert!(step(&mut g, "p", &json!({ "op": "info" }), now).is_none());
        assert!(step(&mut g, "p", &json!({ "ssh": "box" }), now).is_none());
        for op in ["remote_allow", "remote_forget"] {
            for body in [json!({ "op": op }), json!({ "op": op, "ssh": " " })] {
                assert!(matches!(
                    step(&mut g, "p", &body, now),
                    Some(Step::Answer(a)) if a["error"].is_string()
                ));
            }
        }
        g.yes("p", "box");
        g.yes("q", "box");
        assert!(matches!(
            step(&mut g, "p", &json!({ "op": "remote_forget", "ssh": "box " }), now),
            Some(Step::Answer(a)) if a == json!({ "forgotten": true })
        ));
        assert!(g.allowed.is_empty());
        g.decide("p", "box", now);
        g.no("p", "box", now);
        let Some(Step::Answer(a)) = step(&mut g, "p", &allow("box"), now) else {
            panic!("a refused pair is answered at once");
        };
        assert_eq!(
            a["error"],
            "the human said no to driving box from this project; ask them before trying again"
        );
    }

    #[test]
    fn remote_gate_shows_a_destination_safely() {
        assert_eq!(shown("me@box"), "me@box");
        assert_eq!(shown("a\nb\tc\u{202e}d\u{0}"), "a?b?c?d?");
        let long = "x".repeat(SHOWN + 50);
        let cut = shown(&long);
        assert_eq!(cut.chars().count(), SHOWN + 1);
        assert!(cut.ends_with('\u{2026}'));
        assert_eq!(shown(&"y".repeat(SHOWN)), "y".repeat(SHOWN));
    }
}
