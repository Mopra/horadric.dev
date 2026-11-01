//! A Folio's conversation as paragraphs of plain text, each with what it
//! is, for a face that draws it with the platform's own text view rather
//! than laying it out itself: the Mac's (`mac::folio`). Windows draws the
//! blocks as cards in `folio::transcript` and needs none of this.
//!
//! Pure, so how each block reads is tested here on any machine.

use horadric_core::folio::{
    api_error_title, AskState, Block, Ending, Folio, Notice, Step, StepState,
};

/// What a paragraph is, which the face turns into a font and a colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// What the human said.
    Prompt,
    Reply,
    Thinking,
    /// One tool step, or a subagent's.
    Step,
    StepFailed,
    /// Something that waits on the human, or did.
    Ask,
    Notice,
    Error,
    /// The end of a turn.
    Footer,
    /// A `!` command and its output.
    Shell,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paragraph {
    pub style: Style,
    pub text: String,
}

/// How much of a thinking block shows: enough to see where it went.
const THINKING_SHOWN: usize = 400;
/// How much of a step's or a shell's output shows.
const OUTPUT_SHOWN: usize = 600;

/// The whole conversation, oldest first, then what Claude does now.
pub fn paragraphs(folio: &Folio) -> Vec<Paragraph> {
    let mut out = Vec::new();
    let mut push = |style, text: String| {
        if !text.trim().is_empty() {
            out.push(Paragraph { style, text });
        }
    };
    for b in &folio.blocks {
        match b {
            Block::Prompt(t) => push(Style::Prompt, t.trim().to_string()),
            Block::Images(images) => push(Style::Notice, count(images.len(), "image")),
            Block::Reply(t) => push(Style::Reply, t.trim().to_string()),
            Block::Thinking(t) => push(Style::Thinking, cut(t.trim(), THINKING_SHOWN)),
            Block::Tools(steps) => {
                for s in steps {
                    step(s, 0, &mut push);
                }
            }
            Block::Ask {
                tool, shown, state, ..
            } => push(
                Style::Ask,
                format!("Claude asks to use {tool}{}\n{}", said(*state), shown.trim()),
            ),
            Block::Question {
                questions,
                answers,
                state,
                ..
            } => {
                let mut text = format!("Claude asks{}", said(*state));
                for (i, q) in questions.iter().enumerate() {
                    text.push_str(&format!("\n{}", q.question));
                    for (n, o) in q.options.iter().enumerate() {
                        text.push_str(&format!("\n  {}. {}", n + 1, o.label));
                        if !o.description.is_empty() {
                            text.push_str(&format!(": {}", o.description));
                        }
                    }
                    if let Some(a) = answers.get(i).filter(|a| !a.is_empty()) {
                        text.push_str(&format!("\n  Answered: {a}"));
                    }
                }
                push(Style::Ask, text)
            }
            Block::Plan {
                plan,
                state,
                feedback,
                ..
            } => {
                let mut text = format!("Plan{}\n{}", said(*state), plan.trim());
                if !feedback.is_empty() {
                    text.push_str(&format!("\nSent back: {feedback}"));
                }
                push(Style::Ask, text)
            }
            Block::Footer {
                duration_ms,
                ending,
                ..
            } => {
                let how = match ending {
                    Ending::Done => "Done",
                    Ending::Stopped => "Stopped",
                    Ending::Failed => "Failed",
                };
                push(Style::Footer, format!("{how} in {}", duration(*duration_ms)))
            }
            Block::Aside {
                question,
                answer,
                failed,
                ..
            } => {
                let answer = match (answer, failed) {
                    (_, true) => "It could not be answered.".to_string(),
                    (Some(a), false) => a.trim().to_string(),
                    (None, false) => "\u{2026}".to_string(),
                };
                push(Style::Notice, format!("By the way: {question}\n{answer}"))
            }
            Block::Shell(run) => {
                let mut text = format!("! {}", run.command);
                if !run.output.trim().is_empty() {
                    text.push('\n');
                    text.push_str(&tail(run.output.trim_end(), OUTPUT_SHOWN));
                }
                match run.code {
                    Some(0) | None => {}
                    Some(c) => text.push_str(&format!("\nexit {c}")),
                }
                push(Style::Shell, text)
            }
            Block::Notice(n) => match n {
                Notice::Compacted { .. } => {
                    push(Style::Notice, "The conversation was compacted.".to_string())
                }
                Notice::Retrying {
                    attempt,
                    max_retries,
                    error,
                    ..
                } => push(
                    Style::Notice,
                    format!("Trying again ({attempt} of {max_retries}): {error}"),
                ),
                Notice::CommandOutput(s) | Notice::Info(s) => push(Style::Notice, s.clone()),
                Notice::Refused(s) => push(Style::Error, s.clone()),
                Notice::ApiError { kind, text } => push(
                    Style::Error,
                    format!("{}: {}", api_error_title(kind), text.trim()),
                ),
            },
            Block::SignIn(s) => push(
                Style::Error,
                format!(
                    "Claude needs you to sign in{}. Run `claude` in a terminal and type /login, then start this session again.",
                    if s.why.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", s.why.trim())
                    }
                ),
            ),
            Block::Mcp(card) => {
                let mut text = "MCP servers".to_string();
                for s in &card.servers {
                    text.push_str(&format!("\n  {}: {}", s.name, s.status));
                    if !s.error.is_empty() {
                        text.push_str(&format!(" ({})", s.error));
                    }
                }
                push(Style::Notice, text)
            }
            Block::Elicitation(e) => push(
                Style::Ask,
                format!("{} asks{}\n{}", e.server, said(e.state), e.message.trim()),
            ),
            Block::Raw(s) => push(Style::Notice, s.clone()),
        }
    }
    if let Some(error) = folio.failed.as_deref() {
        if !folio.busy {
            push(Style::Error, format!("The last turn failed: {error}"));
        }
    }
    out
}

/// One step and, under it, what its subagent did, indented by `depth`.
fn step(s: &Step, depth: usize, push: &mut impl FnMut(Style, String)) {
    let mark = match s.state {
        StepState::Running => "\u{2026}",
        StepState::Done => "\u{2713}",
        StepState::Failed => "\u{2717}",
        StepState::Stopped => "\u{25a0}",
    };
    let indent = "    ".repeat(depth);
    let mut text = match s.summary.as_str() {
        "" => format!("{indent}{mark} {}", s.tool),
        sum => format!("{indent}{mark} {} {}", s.tool, sum),
    };
    if let Some(e) = &s.edit {
        text.push_str(&format!(" (+{} \u{2212}{})", e.added, e.removed));
    }
    let failed = s.state == StepState::Failed;
    if failed && !s.output.trim().is_empty() {
        text.push('\n');
        text.push_str(&tail(s.output.trim_end(), OUTPUT_SHOWN));
    }
    push(
        if failed {
            Style::StepFailed
        } else {
            Style::Step
        },
        text,
    );
    if let Some(lane) = &s.lane {
        for inner in &lane.steps {
            step(inner, depth + 1, push);
        }
        if lane.working && !lane.progress.is_empty() {
            push(
                Style::Step,
                format!("{}    {}", indent, lane.progress.trim()),
            );
        }
    }
}

/// What became of an ask, after its title.
fn said(state: AskState) -> &'static str {
    match state {
        AskState::Waiting => ", and waits for you",
        AskState::Allowed => " (allowed)",
        AskState::Denied => " (denied)",
        AskState::NotNeeded => " (no longer needed)",
    }
}

fn count(n: usize, what: &str) -> String {
    match n {
        0 => String::new(),
        1 => format!("1 {what}"),
        n => format!("{n} {what}s"),
    }
}

/// `850 ms`, `12 s`, `3 min 5 s`.
pub fn duration(ms: u64) -> String {
    match ms {
        0..=999 => format!("{ms} ms"),
        1000..=59_999 => format!("{} s", ms / 1000),
        _ => {
            let s = ms / 1000;
            match s % 60 {
                0 => format!("{} min", s / 60),
                r => format!("{} min {r} s", s / 60),
            }
        }
    }
}

/// The first `max` characters, with an ellipsis when there was more.
fn cut(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((at, _)) => format!("{}\u{2026}", &s[..at]),
        None => s.to_string(),
    }
}

/// The last `max` characters, with an ellipsis when there was more.
fn tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let at = s.char_indices().nth(n - max).map_or(0, |(i, _)| i);
    format!("\u{2026}{}", &s[at..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::folio::TurnUsage;

    fn folio(blocks: Vec<Block>) -> Folio {
        let mut f = Folio::new();
        f.blocks = blocks;
        f
    }

    #[test]
    fn a_turn_reads_as_prompt_reply_and_footer() {
        let f = folio(vec![
            Block::Prompt(" hello ".into()),
            Block::Reply("Hi.".into()),
            Block::Footer {
                duration_ms: 12_400,
                ending: Ending::Done,
                usage: TurnUsage::default(),
                at: None,
            },
        ]);
        let p = paragraphs(&f);
        assert_eq!(
            p.iter().map(|p| p.style).collect::<Vec<_>>(),
            [Style::Prompt, Style::Reply, Style::Footer]
        );
        assert_eq!(p[0].text, "hello");
        assert_eq!(p[2].text, "Done in 12 s");
    }

    #[test]
    fn an_empty_reply_says_nothing() {
        assert!(paragraphs(&folio(vec![Block::Reply("  ".into())])).is_empty());
    }

    #[test]
    fn a_waiting_ask_says_so_and_names_its_tool() {
        let f = folio(vec![Block::Ask {
            request_id: "r".into(),
            tool: "Bash".into(),
            summary: "ls".into(),
            shown: "ls -la".into(),
            input: serde_json::Value::Null,
            suggestions: serde_json::Value::Null,
            state: AskState::Waiting,
        }]);
        let p = paragraphs(&f);
        assert_eq!(p[0].style, Style::Ask);
        assert_eq!(p[0].text, "Claude asks to use Bash, and waits for you\nls -la");
    }

    #[test]
    fn a_failed_step_shows_the_end_of_its_output() {
        let s = Step {
            id: "1".into(),
            tool: "Bash".into(),
            summary: "cargo test".into(),
            state: StepState::Failed,
            input: serde_json::Value::Null,
            edit: None,
            lane: None,
            output: format!("{}last line", "x".repeat(2000)),
            images: Vec::new(),
            began: None,
            ended: None,
        };
        let p = paragraphs(&folio(vec![Block::Tools(vec![s])]));
        assert_eq!(p[0].style, Style::StepFailed);
        assert!(p[0].text.starts_with("\u{2717} Bash cargo test\n\u{2026}"));
        assert!(p[0].text.ends_with("last line"));
    }

    #[test]
    fn durations_read_like_a_person_says_them() {
        assert_eq!(duration(850), "850 ms");
        assert_eq!(duration(59_999), "59 s");
        assert_eq!(duration(120_000), "2 min");
        assert_eq!(duration(185_000), "3 min 5 s");
    }

    #[test]
    fn cutting_keeps_whole_characters() {
        assert_eq!(cut("\u{e9}\u{e9}\u{e9}", 2), "\u{e9}\u{e9}\u{2026}");
        assert_eq!(tail("\u{e9}\u{e9}\u{e9}", 2), "\u{2026}\u{e9}\u{e9}");
        assert_eq!(cut("ab", 5), "ab");
    }
}
