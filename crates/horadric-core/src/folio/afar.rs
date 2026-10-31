//! A Folio served to a Town Portal phone or a linked brother, which know
//! only terminals: the conversation as text lines for their screen, the
//! request that waits as the `ask` a phone already answers, and what they
//! type turned into prompts, answers and interrupts. A page or a brother
//! that knows nothing of Folios works unchanged, since the keys its ask
//! card and its keyboard send mean here what they mean on Claude Code's
//! own prompt.

use std::time::SystemTime;

use serde_json::{json, Value};

use super::{
    control_response, prompt_line, AskState, Block, Ending, Folio, Notice, Priority, Reply,
    StepState,
};
use crate::ask::{Ask, Choice, Kind};

/// The most lines a screen holds, as a terminal's screen sends at most
/// 500 lines of scrollback.
pub const SCREEN_LINES: usize = 500;

/// How many lines of a failed step's output show under it.
const FAILED_TAIL: usize = 3;
/// How many lines of what an ask shows go under its first line.
const ASK_LINES: usize = 12;

const ESC: &str = "\u{1b}";

/// What typing from afar asks of the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    /// A line for the CLI: an answer, or a prompt folded into the turn
    /// that runs.
    Write(String),
    /// A prompt that starts a turn. Should it not arrive, no turn started
    /// and the Folio is to be told with [`Folio::end`].
    Start(String),
    Interrupt,
}

/// The conversation as a terminal's screen lines, the newest
/// [`SCREEN_LINES`] of them, with what was typed from afar and not yet
/// sent as the last line.
pub fn lines(f: &Folio, draft: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for b in &f.blocks {
        let before = out.len();
        block(b, &mut out);
        if out.len() > before {
            out.push(String::new());
        }
    }
    if f.busy && f.pending_asks().is_empty() {
        let doing = f.status.as_deref().unwrap_or("working");
        out.push(format!("\u{2026} {doing}"));
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    if !draft.is_empty() {
        out.push(String::new());
        out.extend(prefixed("> ", draft));
    }
    let cut = out.len().saturating_sub(SCREEN_LINES);
    out.drain(..cut);
    out
}

fn block(b: &Block, out: &mut Vec<String>) {
    match b {
        Block::Prompt(text) => out.extend(prefixed("> ", text)),
        Block::Images(list) => out.push(format!(
            "  [{} image{}]",
            list.len(),
            if list.len() == 1 { "" } else { "s" }
        )),
        Block::Reply(text) => out.extend(text.trim_end().lines().map(str::to_string)),
        Block::Thinking(_) => out.push("\u{2234} Thinking".into()),
        Block::Tools(steps) => {
            for s in steps {
                let mark = match s.state {
                    StepState::Running => "\u{2026}",
                    StepState::Done => "\u{2713}",
                    StepState::Failed => "\u{2717}",
                    StepState::Stopped => "\u{25a0}",
                };
                out.push(format!("{mark} {} {}", s.tool, one_line(&s.summary)));
                if let Some(lane) = &s.lane {
                    out.push(format!(
                        "    {} \u{b7} {} steps",
                        lane.agent_type,
                        lane.steps.len()
                    ));
                }
                if s.state == StepState::Failed {
                    let tail: Vec<&str> = s.output.trim_end().lines().collect();
                    let from = tail.len().saturating_sub(FAILED_TAIL);
                    out.extend(tail[from..].iter().map(|l| format!("    {l}")));
                }
            }
        }
        Block::Ask {
            tool,
            summary,
            shown,
            suggestions,
            state,
            ..
        } => {
            out.push(format!("? Allow {tool}: {}", one_line(summary)));
            if shown.trim() != summary.trim() {
                let shown: Vec<&str> = shown.lines().collect();
                out.extend(shown.iter().take(ASK_LINES).map(|l| format!("    {l}")));
                if shown.len() > ASK_LINES {
                    out.push("    \u{2026}".into());
                }
            }
            out.push(match state {
                AskState::Waiting if has_rules(suggestions) => "  1 Yes \u{b7} 2 Always \u{b7} Esc No".into(),
                AskState::Waiting => "  1 Yes \u{b7} Esc No".into(),
                other => format!("  {}", settled(*other)),
            });
        }
        Block::Question {
            questions,
            answers,
            state,
            ..
        } => {
            for (i, q) in questions.iter().enumerate() {
                out.extend(prefixed("? ", &q.question));
                if *state == AskState::Waiting {
                    for (n, o) in q.options.iter().enumerate() {
                        out.push(format!("  {} {}", n + 1, o.label));
                    }
                } else if let Some(a) = answers.get(i).filter(|a| !a.is_empty()) {
                    out.push(format!("  \u{2192} {a}"));
                }
            }
            if *state == AskState::Waiting {
                out.push("  Type an answer, or a number, then Enter \u{b7} Esc declines".into());
            } else if answers.iter().all(String::is_empty) {
                out.push(format!("  {}", settled(*state)));
            }
        }
        Block::Plan {
            plan,
            state,
            feedback,
            ..
        } => {
            out.push("Plan:".into());
            out.extend(plan.trim_end().lines().map(|l| format!("  {l}")));
            out.push(match state {
                AskState::Waiting => {
                    "  1 Approve \u{b7} 2 Approve, accept edits \u{b7} Esc or type to keep planning"
                        .into()
                }
                AskState::Allowed => "  approved".into(),
                AskState::Denied if !feedback.is_empty() => {
                    format!("  sent back: {}", one_line(feedback))
                }
                other => format!("  {}", settled(*other)),
            });
        }
        Block::Footer {
            duration_ms,
            ending,
            ..
        } => {
            let secs = duration_ms / 1000;
            let took = if secs >= 60 {
                format!("{} min {:02} s", secs / 60, secs % 60)
            } else {
                format!("{secs} s")
            };
            let how = match ending {
                Ending::Done => "done",
                Ending::Stopped => "stopped",
                Ending::Failed => "failed",
            };
            out.push(format!("\u{2500} {how} in {took}"));
        }
        Block::Notice(n) => match n {
            Notice::Compacted { trigger, .. } => {
                out.push(format!("\u{2500} conversation compacted ({trigger})"));
            }
            Notice::Retrying {
                attempt,
                max_retries,
                error,
                ..
            } => out.push(format!(
                "\u{2500} retrying, {attempt} of {max_retries}: {}",
                one_line(error)
            )),
            Notice::CommandOutput(text) | Notice::Info(text) => {
                out.extend(text.trim_end().lines().map(str::to_string));
            }
            Notice::Refused(text) => out.push(format!("\u{2500} refused: {}", one_line(text))),
        },
        Block::Raw(_) => {}
    }
}

fn settled(state: AskState) -> &'static str {
    match state {
        AskState::Waiting => "waiting",
        AskState::Allowed => "allowed",
        AskState::Denied => "denied",
        AskState::NotNeeded => "no longer asked",
    }
}

fn has_rules(suggestions: &Value) -> bool {
    suggestions.as_array().is_some_and(|a| !a.is_empty())
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text`'s lines, the first after `mark` and the rest lined up under it.
fn prefixed(mark: &str, text: &str) -> Vec<String> {
    let pad = " ".repeat(mark.chars().count());
    text.trim_end()
        .lines()
        .enumerate()
        .map(|(i, l)| format!("{}{l}", if i == 0 { mark } else { &pad }))
        .collect()
}

/// The oldest request waiting on the human as the `ask` a phone shows,
/// its options' keys those [`typed`] takes, or None when nothing waits.
/// `since` is when the session began to wait.
pub fn ask(f: &Folio, since: SystemTime) -> Option<Ask> {
    let id = *f.pending_asks().first()?;
    let b = f.blocks.iter().find(|b| request_of(b) == Some(id))?;
    match b {
        Block::Ask {
            tool,
            input,
            suggestions,
            ..
        } => Some(Ask::permission(
            tool,
            Some(input),
            has_rules(suggestions),
            since,
        )),
        Block::Question { questions, .. } => {
            let input = json!({"questions": questions.iter().map(|q| json!({
                "question": q.question,
                "multiSelect": q.multi_select,
                "options": q.options.iter().map(|o| json!({"label": o.label})).collect::<Vec<_>>(),
            })).collect::<Vec<_>>()});
            Ask::question(&input, since)
        }
        Block::Plan { plan, .. } => {
            let mut a = Ask::permission("ExitPlanMode", Some(&json!({"plan": plan})), false, since);
            a.options = [
                ("Approve", "1"),
                ("Approve, accept edits", "2"),
                ("Keep planning", ESC),
            ]
            .map(|(label, keys)| Choice {
                label: label.into(),
                keys: keys.into(),
            })
            .to_vec();
            Some(a)
        }
        _ => None,
    }
}

fn request_of(b: &Block) -> Option<&str> {
    match b {
        Block::Ask { request_id, .. }
        | Block::Question { request_id, .. }
        | Block::Plan { request_id, .. } => Some(request_id),
        _ => None,
    }
}

/// Takes what a phone or a brother typed, `data` as one `input` carries
/// it, into `draft` and the session. A digit alone answers the request
/// that waits, as on Claude Code's prompt; Enter sends the draft, as a
/// prompt or, while a request waits, as the words of its answer, and with
/// nothing typed picks the first choice; Escape declines what waits, or
/// stops the turn; Ctrl+C stops the turn.
pub fn typed(f: &mut Folio, draft: &mut String, data: &str) -> Vec<Typed> {
    let mut acts = Vec::new();
    if data.len() == 1 && draft.is_empty() && data.as_bytes()[0].is_ascii_digit() {
        if let Some(line) = pick(f, data) {
            acts.push(Typed::Write(line));
            return acts;
        }
    }
    let mut chars = data.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                // A key's escape sequence, such as an arrow, moves nothing here.
                if let Some('[' | 'O') = chars.peek() {
                    chars.next();
                    while let Some(&n) = chars.peek() {
                        chars.next();
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                    continue;
                }
                draft.clear();
                match decline(f) {
                    Some(line) => acts.push(Typed::Write(line)),
                    None if f.busy => acts.push(Typed::Interrupt),
                    None => {}
                }
            }
            '\r' | '\n' => {
                let text = std::mem::take(draft);
                if let Some(act) = enter(f, text.trim()) {
                    acts.push(act);
                }
            }
            '\u{3}' => {
                draft.clear();
                if f.busy {
                    acts.push(Typed::Interrupt);
                }
            }
            '\u{15}' => draft.clear(),
            '\u{7f}' | '\u{8}' => {
                draft.pop();
            }
            '\t' => draft.push(' '),
            c if c.is_control() => {}
            c => draft.push(c),
        }
    }
    acts
}

/// Picks choice `digit` of the request that waits, numbered as [`ask`]
/// numbers them. None when nothing waits or it has no such choice.
fn pick(f: &mut Folio, digit: &str) -> Option<String> {
    let n: usize = digit.parse().ok()?;
    let id = f.pending_asks().first()?.to_string();
    let b = f.blocks.iter().find(|b| request_of(b) == Some(&id))?;
    match b {
        Block::Ask { suggestions, .. } => {
            let always = has_rules(suggestions);
            let reply = match (n, always) {
                (1, _) => Reply::Allow,
                (2, true) => Reply::Always,
                (2, false) | (3, true) => Reply::Deny(""),
                _ => return None,
            };
            f.reply(&id, reply)
        }
        Block::Question { questions, .. } => {
            let [q] = questions.as_slice() else {
                return None;
            };
            if q.multi_select {
                return None;
            }
            let label = q.options.get(n.checked_sub(1)?)?.label.clone();
            f.answer_question(&id, &[vec![label]])
        }
        Block::Plan { .. } => match n {
            1 => f.approve_plan(&id, None),
            2 => f.approve_plan(&id, Some("acceptEdits")),
            _ => None,
        },
        _ => None,
    }
}

/// Enter with `text` typed: the words of an answer while a request waits,
/// else a prompt.
fn enter(f: &mut Folio, text: &str) -> Option<Typed> {
    let Some(id) = f.pending_asks().first().map(|s| s.to_string()) else {
        if text.is_empty() {
            return None;
        }
        if f.busy {
            return Some(Typed::Write(f.queue_prompt(text, &[], Priority::Next)));
        }
        f.push_prompt(text);
        return Some(Typed::Start(prompt_line(text)));
    };
    if text.is_empty() {
        return pick(f, "1").map(Typed::Write);
    }
    let b = f.blocks.iter().find(|b| request_of(b) == Some(&id))?;
    let line = match b {
        Block::Ask { .. } => f.reply(&id, Reply::Deny(text)),
        // One line a question, in order; the last line answers the rest.
        Block::Question { questions, .. } => {
            let said: Vec<&str> = text.lines().map(str::trim).collect();
            let picks: Vec<Vec<String>> = (0..questions.len())
                .map(|i| vec![said.get(i).or(said.last()).unwrap_or(&"").to_string()])
                .collect();
            f.answer_question(&id, &picks)
        }
        Block::Plan { .. } => f.keep_planning(&id, text),
        _ => None,
    };
    line.map(Typed::Write)
}

/// Declines the request that waits: no to a permission, a question left
/// unanswered, a plan sent back. None when nothing waits.
fn decline(f: &mut Folio) -> Option<String> {
    let id = f.pending_asks().first()?.to_string();
    let b = f.blocks.iter_mut().find(|b| request_of(b) == Some(&id))?;
    match b {
        Block::Ask { .. } => f.reply(&id, Reply::Deny("")),
        Block::Question { state, .. } => {
            *state = AskState::Denied;
            let input = f.input_of_request(&id).unwrap_or_else(|| json!({}));
            Some(control_response(&id, false, input))
        }
        Block::Plan { .. } => f.keep_planning(&id, ""),
        _ => None,
    }
}

/// Whether `ask` is one a phone answers with its options' keys, which
/// [`typed`] takes.
pub fn answerable(ask: &Ask) -> bool {
    ask.kind != Kind::Input && !ask.options.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asking(tool: &str, input: Value, suggestions: Value) -> Folio {
        let mut f = Folio::new();
        f.push_prompt("go");
        f.blocks.push(Block::Ask {
            request_id: "r1".into(),
            tool: tool.into(),
            summary: "ls".into(),
            shown: "ls".into(),
            input,
            suggestions,
            state: AskState::Waiting,
        });
        f
    }

    #[test]
    fn the_screen_shows_prompt_reply_and_what_is_typed() {
        let mut f = Folio::new();
        f.push_prompt("hello\nthere");
        f.blocks.push(Block::Reply("Hi.\n\nHow can I help?\n".into()));
        f.busy = false;
        let l = lines(&f, "next");
        assert_eq!(
            l,
            ["> hello", "  there", "", "Hi.", "", "How can I help?", "", "> next"]
        );
        f.busy = true;
        assert_eq!(lines(&f, "").last().unwrap(), "\u{2026} working");
    }

    #[test]
    fn the_screen_keeps_the_newest_lines() {
        let mut f = Folio::new();
        let long: Vec<String> = (0..600).map(|i| i.to_string()).collect();
        f.blocks.push(Block::Reply(long.join("\n")));
        let l = lines(&f, "");
        assert_eq!(l.len(), SCREEN_LINES);
        assert_eq!(l.last().unwrap(), "599");
    }

    #[test]
    fn a_permission_asks_with_the_keys_that_answer_it() {
        let mut f = asking("Bash", json!({"command": "ls"}), json!([{"type": "addRules"}]));
        let a = ask(&f, SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(a.detail, "ls");
        let keys: Vec<&str> = a.options.iter().map(|o| o.keys.as_str()).collect();
        assert_eq!(keys, ["1", "2", ESC]);
        let mut draft = String::new();
        let acts = typed(&mut f, &mut draft, "2");
        assert!(matches!(&acts[..], [Typed::Write(l)] if l.contains("updatedPermissions")));
        assert!(f.pending_asks().is_empty());
        assert!(ask(&f, SystemTime::UNIX_EPOCH).is_none());
    }

    #[test]
    fn escape_denies_and_words_deny_with_a_reason() {
        let mut f = asking("Bash", json!({"command": "ls"}), Value::Null);
        let mut draft = String::new();
        let acts = typed(&mut f, &mut draft, ESC);
        assert!(matches!(&acts[..], [Typed::Write(l)] if l.contains("deny")));
        let mut f = asking("Bash", json!({"command": "ls"}), Value::Null);
        typed(&mut f, &mut draft, "use dir instead");
        let acts = typed(&mut f, &mut draft, "\r");
        assert!(matches!(&acts[..], [Typed::Write(l)] if l.contains("use dir instead")));
    }

    #[test]
    fn a_digit_inside_a_message_is_not_an_answer() {
        let mut f = asking("Bash", json!({}), Value::Null);
        let mut draft = String::new();
        assert!(typed(&mut f, &mut draft, "1 more").is_empty());
        assert_eq!(draft, "1 more");
        assert_eq!(f.pending_asks(), ["r1"]);
    }

    #[test]
    fn typing_then_enter_starts_a_turn_or_steers_the_one_running() {
        let mut f = Folio::new();
        let mut draft = String::new();
        assert!(typed(&mut f, &mut draft, "fix it").is_empty());
        let acts = typed(&mut f, &mut draft, "\r");
        assert!(matches!(&acts[..], [Typed::Start(l)] if l.contains("fix it")));
        assert!(f.busy && draft.is_empty());
        let acts = typed(&mut f, &mut draft, "and test\r");
        assert!(matches!(&acts[..], [Typed::Write(l)] if l.contains("and test")));
        assert_eq!(typed(&mut f, &mut draft, "\u{1b}"), [Typed::Interrupt]);
        // An arrow key is not Escape.
        assert!(typed(&mut f, &mut draft, "\u{1b}[A").is_empty());
    }

    #[test]
    fn a_plan_is_approved_by_number_and_sent_back_with_words() {
        let mut f = Folio::new();
        f.blocks.push(Block::Plan {
            request_id: "p".into(),
            tool_use_id: "t".into(),
            plan: "1. do it".into(),
            state: AskState::Waiting,
            feedback: String::new(),
        });
        let a = ask(&f, SystemTime::UNIX_EPOCH).unwrap();
        assert!(answerable(&a));
        assert_eq!(a.options.len(), 3);
        let mut back = f.clone_blocks_for_test();
        let mut draft = String::new();
        let acts = typed(&mut f, &mut draft, "2");
        assert!(matches!(&acts[..], [Typed::Write(l)] if l.contains("acceptEdits")));
        typed(&mut back, &mut draft, "smaller steps\r");
        assert!(matches!(&back.blocks[0], Block::Plan { state: AskState::Denied, feedback, .. } if feedback == "smaller steps"));
    }

    #[test]
    fn a_question_is_answered_by_number_or_declined() {
        let mut f = Folio::new();
        let q = super::super::Question {
            question: "Which?".into(),
            options: ["red", "blue"]
                .map(|l| super::super::QuestionOption {
                    label: l.into(),
                    ..Default::default()
                })
                .to_vec(),
            ..Default::default()
        };
        f.blocks.push(Block::Question {
            request_id: "q".into(),
            tool_use_id: "t".into(),
            questions: vec![q],
            answers: Vec::new(),
            state: AskState::Waiting,
        });
        let mut no = f.clone_blocks_for_test();
        let a = ask(&f, SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(a.options[1].keys, "2");
        let mut draft = String::new();
        let acts = typed(&mut f, &mut draft, "2");
        assert!(matches!(&acts[..], [Typed::Write(l)] if l.contains("blue")));
        let acts = typed(&mut no, &mut draft, ESC);
        assert!(matches!(&acts[..], [Typed::Write(l)] if l.contains("deny")));
        assert!(no.pending_asks().is_empty());
    }

    impl Folio {
        fn clone_blocks_for_test(&self) -> Folio {
            Folio {
                blocks: self.blocks.clone(),
                ..Folio::default()
            }
        }
    }
}
