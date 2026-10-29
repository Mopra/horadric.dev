//! What the Folio's view says and where things go, without a window: a run
//! of tool steps as one line, a turn's footer, the column the conversation
//! is read in, how far it scrolls, and the runes drifting up behind it.
//! The view itself is `folio`, Windows only.

use std::f32::consts::{PI, TAU};
use std::path::Path;
use std::time::Duration;

use horadric_core::folio::{AskState, Ending, Step, StepState};

use crate::layout::Rect;

/// The widest the conversation reads. Wider lines are hard to follow.
pub const COLUMN_MAX: f32 = 760.0;
/// The least room either side of the column.
pub const COLUMN_MARGIN: f32 = 22.0;
/// How long the background takes to warm when a turn starts.
pub const WARM: Duration = Duration::from_millis(1200);
/// And to cool after it ends, after which nothing moves and nothing draws.
pub const COOL: Duration = Duration::from_millis(2400);
/// How many runes drift behind the conversation.
pub const EMBERS: usize = 16;
/// Room above and below the glass an ember is born and dies in, so it
/// never pops in or out where it can be seen.
const EMBER_MARGIN: f32 = 30.0;

/// Where the column starts and how wide it is, in a view `width` wide.
pub fn column(width: f32) -> (f32, f32) {
    let w = (width - 2.0 * COLUMN_MARGIN).clamp(0.0, COLUMN_MAX);
    ((width - w) / 2.0, w)
}

/// How a run of tool steps stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Running,
    /// Its turn ended with a step still running.
    Stopped,
    /// Every step done, this many of them failed.
    Done(usize),
}

pub fn run_state(steps: &[Step]) -> RunState {
    let any = |state| steps.iter().any(|s| s.state == state);
    if any(StepState::Running) {
        return RunState::Running;
    }
    if any(StepState::Stopped) {
        return RunState::Stopped;
    }
    let failed = steps.iter().filter(|s| s.state == StepState::Failed);
    RunState::Done(failed.count())
}

/// What kind of work a tool does, for counting a run's steps together.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Read,
    Ran,
    Edited,
    Searched,
    Fetched,
    Agent,
    Plan,
    Other(String),
}

fn kind(tool: &str) -> Kind {
    match tool {
        "Read" | "NotebookRead" => Kind::Read,
        "Bash" | "PowerShell" | "BashOutput" | "KillShell" => Kind::Ran,
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => Kind::Edited,
        "Grep" | "Glob" | "LS" | "ToolSearch" => Kind::Searched,
        "WebFetch" | "WebSearch" => Kind::Fetched,
        "Task" | "Agent" => Kind::Agent,
        "TodoWrite" => Kind::Plan,
        // An MCP tool reads better by its own last part.
        t => Kind::Other(t.rsplit("__").next().unwrap_or(t).to_string()),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

fn times(n: usize) -> &'static str {
    match n {
        1 => "once",
        2 => "twice",
        _ => "",
    }
}

/// The file a step was about, by its name alone, when its summary is a path.
fn file_name(summary: &str) -> Option<String> {
    let s = summary.trim();
    if s.is_empty() || s.contains(' ') {
        return None;
    }
    Some(Path::new(s).file_name()?.to_string_lossy().into_owned())
}

/// A run of tool steps as one line, kinds in the order they first came:
/// "Read 3 files · Ran 1 command · Edited pane.rs".
pub fn run_summary(steps: &[Step]) -> String {
    let mut kinds: Vec<(Kind, Vec<&Step>)> = Vec::new();
    for s in steps {
        let k = kind(&s.tool);
        match kinds.iter_mut().find(|(seen, _)| *seen == k) {
            Some((_, list)) => list.push(s),
            None => kinds.push((k, vec![s])),
        }
    }
    let parts: Vec<String> = kinds
        .iter()
        .map(|(k, list)| {
            let n = list.len();
            let one = (n == 1).then(|| file_name(&list[0].summary)).flatten();
            match k {
                Kind::Read => match one {
                    Some(f) => format!("Read {f}"),
                    None => format!("Read {}", plural(n, "file", "files")),
                },
                Kind::Ran => format!("Ran {}", plural(n, "command", "commands")),
                Kind::Edited => match one {
                    Some(f) => format!("Edited {f}"),
                    None => format!("Edited {}", plural(n, "file", "files")),
                },
                Kind::Searched => match times(n) {
                    "" => format!("Searched {n} times"),
                    t => format!("Searched {t}"),
                },
                Kind::Fetched => format!("Fetched {}", plural(n, "page", "pages")),
                Kind::Agent => format!("Ran {}", plural(n, "agent", "agents")),
                Kind::Plan => "Updated the plan".to_string(),
                Kind::Other(name) => match times(n) {
                    "" => format!("Used {name} {n} times"),
                    _ if n == 1 => format!("Used {name}"),
                    t => format!("Used {name} {t}"),
                },
            }
        })
        .collect();
    parts.join(" \u{b7} ")
}

/// A turn's footer: how it ended, how long it took and what it cost.
pub fn footer_line(duration_ms: u64, cost_usd: f64, ending: Ending) -> String {
    let secs = duration_ms as f64 / 1000.0;
    let time = if secs < 60.0 {
        format!("{secs:.1} s")
    } else {
        let whole = secs.round() as u64;
        format!("{} min {} s", whole / 60, whole % 60)
    };
    let cost = if cost_usd > 0.0 {
        format!(" \u{b7} ${cost_usd:.4}")
    } else {
        String::new()
    };
    match ending {
        Ending::Done => format!("{time}{cost}"),
        Ending::Stopped => format!("Stopped \u{b7} {time}{cost}"),
        Ending::Failed => format!("Ended with an error \u{b7} {time}{cost}"),
    }
}

/// What an answered or lapsed ask's card says where its buttons were.
pub fn ask_said(state: AskState) -> &'static str {
    match state {
        AskState::Waiting => "",
        AskState::Allowed => "Allowed",
        AskState::Denied => "Denied",
        AskState::NotNeeded => "Not needed",
    }
}

/// The part of `r` inside `to`, or None when none of it is.
pub fn clip(r: &Rect, to: &Rect) -> Option<Rect> {
    let (x, y) = (r.x.max(to.x), r.y.max(to.y));
    let (right, bottom) = (r.right().min(to.right()), r.bottom().min(to.bottom()));
    (right > x && bottom > y).then(|| Rect::new(x, y, right - x, bottom - y))
}

/// Thinking folded to one line: its first words that say anything.
pub fn thinking_line(text: &str) -> String {
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if first.is_empty() {
        "Thinking".to_string()
    } else {
        format!("Thinking \u{b7} {first}")
    }
}

/// What an ask's card says Claude wants.
pub fn ask_title(tool: &str) -> String {
    match kind(tool) {
        Kind::Ran => "Claude wants to run a command".to_string(),
        Kind::Edited => "Claude wants to change a file".to_string(),
        Kind::Read => "Claude wants to read a file".to_string(),
        Kind::Fetched => "Claude wants to go online".to_string(),
        Kind::Other(name) => format!("Claude wants to use {name}"),
        _ => format!("Claude wants to use {tool}"),
    }
}

/// What the composer holds, as a prompt to send, or None when it says
/// nothing.
pub fn prompt_of(text: &str) -> Option<String> {
    let t = text.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// The scroll kept inside what there is: from the top to where the last
/// of the content meets the bottom of the view.
pub fn clamp_scroll(scroll: f32, content: f32, view: f32) -> f32 {
    scroll.clamp(0.0, (content - view).max(0.0))
}

/// Whether the view shows the end, so it keeps following what streams in.
pub fn at_bottom(scroll: f32, content: f32, view: f32) -> bool {
    scroll >= (content - view).max(0.0) - 2.0
}

/// How warm the background is, from 0 at rest to 1 while a turn runs,
/// `since` the turn started or ended.
pub fn heat(busy: bool, since: Duration) -> f32 {
    if busy {
        (since.as_secs_f32() / WARM.as_secs_f32()).min(1.0)
    } else {
        (1.0 - since.as_secs_f32() / COOL.as_secs_f32()).max(0.0)
    }
}

/// One rune drifting up behind the conversation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ember {
    pub x: f32,
    pub y: f32,
    /// Its height, in DIPs.
    pub size: f32,
    pub alpha: f32,
    /// Which of the `runes` it is.
    pub rune: usize,
}

/// FNV-1a over the words, stable across runs and builds.
fn hash(words: &[u64]) -> u64 {
    words.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &w| {
        w.to_le_bytes()
            .iter()
            .fold(h, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
    })
}

fn unit(i: usize, salt: u64) -> f32 {
    (hash(&[i as u64, salt]) % 10_000) as f32 / 10_000.0
}

/// Ember `i` at `t` seconds of the background's own clock, in a view `w`
/// by `h`, with `runes` to pick from. A function of the time alone, so the
/// view keeps no particles. Warmth makes it brighter, not faster: a speed
/// that changed would jump every ember along its path.
pub fn ember(i: usize, t: f32, w: f32, h: f32, heat: f32, runes: usize) -> Ember {
    let speed = 7.0 + 9.0 * unit(i, 1);
    let travel = h + 2.0 * EMBER_MARGIN;
    let p = (unit(i, 2) * travel + t * speed).rem_euclid(travel);
    let sway = (t * (0.2 + 0.2 * unit(i, 3)) + unit(i, 4) * TAU).sin() * 14.0;
    let life = (PI * p / travel).sin();
    Ember {
        x: unit(i, 5) * w + sway,
        y: h + EMBER_MARGIN - p,
        size: 9.0 + 9.0 * unit(i, 6),
        alpha: life * (0.07 + 0.06 * unit(i, 7)) * (1.0 + 1.6 * heat),
        rune: (hash(&[i as u64, 8]) % runes.max(1) as u64) as usize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(tool: &str, summary: &str, ok: Option<bool>) -> Step {
        Step {
            id: format!("{tool}{summary}"),
            tool: tool.to_string(),
            summary: summary.to_string(),
            state: match ok {
                None => StepState::Running,
                Some(true) => StepState::Done,
                Some(false) => StepState::Failed,
            },
        }
    }

    #[test]
    fn a_run_counts_each_kind_in_the_order_it_came() {
        let steps = [
            step("Read", "src/a.rs", Some(true)),
            step("Bash", "cargo test", Some(true)),
            step("Read", "src/b.rs", Some(true)),
            step("Read", "src/c.rs", Some(true)),
        ];
        assert_eq!(run_summary(&steps), "Read 3 files \u{b7} Ran 1 command");
    }

    #[test]
    fn one_file_is_named_and_searches_are_counted_in_words() {
        let steps = [
            step("Edit", "crates/ui/src/pane.rs", Some(true)),
            step("Grep", "fn paint", Some(true)),
            step("Glob", "**/*.rs", Some(true)),
        ];
        assert_eq!(run_summary(&steps), "Edited pane.rs \u{b7} Searched twice");
        let three = [
            step("Grep", "a", None),
            step("Grep", "b", None),
            step("Grep", "c", None),
        ];
        assert_eq!(run_summary(&three), "Searched 3 times");
    }

    #[test]
    fn an_mcp_tool_goes_by_its_own_name() {
        let steps = [step("mcp__horadric__lore_recall", "", Some(true))];
        assert_eq!(run_summary(&steps), "Used lore_recall");
        assert_eq!(
            ask_title("mcp__horadric__lore_note"),
            "Claude wants to use lore_note"
        );
        assert_eq!(ask_title("Bash"), "Claude wants to run a command");
    }

    #[test]
    fn a_run_is_running_until_every_step_is_done_and_counts_failures() {
        let mut steps = vec![step("Bash", "x", Some(false)), step("Read", "y", None)];
        assert_eq!(run_state(&steps), RunState::Running);
        steps[1].state = StepState::Stopped;
        assert_eq!(run_state(&steps), RunState::Stopped);
        steps[1].state = StepState::Done;
        assert_eq!(run_state(&steps), RunState::Done(1));
    }

    #[test]
    fn a_hit_is_cut_to_what_shows_and_gone_when_hidden() {
        let area = Rect::new(0.0, 0.0, 400.0, 300.0);
        let half = clip(&Rect::new(10.0, 280.0, 80.0, 30.0), &area).unwrap();
        assert_eq!((half.y, half.h), (280.0, 20.0));
        assert!(clip(&Rect::new(10.0, 300.0, 80.0, 30.0), &area).is_none());
        assert!(clip(&Rect::new(10.0, -40.0, 80.0, 30.0), &area).is_none());
        let inside = Rect::new(10.0, 10.0, 80.0, 30.0);
        assert_eq!(clip(&inside, &area), Some(inside));
    }

    #[test]
    fn a_lapsed_ask_says_it_is_not_needed() {
        assert_eq!(ask_said(AskState::NotNeeded), "Not needed");
        assert_eq!(ask_said(AskState::Allowed), "Allowed");
    }

    #[test]
    fn the_footer_says_time_and_cost() {
        assert_eq!(
            footer_line(4321, 0.01234, Ending::Done),
            "4.3 s \u{b7} $0.0123"
        );
        assert_eq!(footer_line(125_000, 0.0, Ending::Done), "2 min 5 s");
        assert_eq!(
            footer_line(900, 0.5, Ending::Failed),
            "Ended with an error \u{b7} 0.9 s \u{b7} $0.5000"
        );
        assert_eq!(
            footer_line(5180, 0.0, Ending::Stopped),
            "Stopped \u{b7} 5.2 s"
        );
    }

    #[test]
    fn thinking_folds_to_its_first_line_that_says_something() {
        assert_eq!(
            thinking_line("\n  The user wants a test.\nThen more."),
            "Thinking \u{b7} The user wants a test."
        );
        assert_eq!(thinking_line("  \n"), "Thinking");
    }

    #[test]
    fn only_a_prompt_with_words_is_sent() {
        assert_eq!(prompt_of("  hi there\n"), Some("hi there".to_string()));
        assert_eq!(prompt_of(" \n\t"), None);
    }

    #[test]
    fn the_column_is_centred_and_never_wider_than_reads_well() {
        assert_eq!(column(400.0), (22.0, 356.0));
        let (x, w) = column(2000.0);
        assert_eq!(w, COLUMN_MAX);
        assert_eq!(x * 2.0 + w, 2000.0);
        assert_eq!(column(10.0).1, 0.0);
    }

    #[test]
    fn the_scroll_stays_inside_the_content_and_knows_the_bottom() {
        assert_eq!(clamp_scroll(-5.0, 1000.0, 400.0), 0.0);
        assert_eq!(clamp_scroll(900.0, 1000.0, 400.0), 600.0);
        assert_eq!(clamp_scroll(50.0, 300.0, 400.0), 0.0);
        assert!(at_bottom(600.0, 1000.0, 400.0));
        assert!(!at_bottom(500.0, 1000.0, 400.0));
        assert!(at_bottom(0.0, 300.0, 400.0));
    }

    #[test]
    fn the_background_warms_with_a_turn_and_cools_to_rest() {
        assert_eq!(heat(true, Duration::ZERO), 0.0);
        assert_eq!(heat(true, WARM * 2), 1.0);
        assert_eq!(heat(false, Duration::ZERO), 1.0);
        assert_eq!(heat(false, COOL), 0.0);
        assert!(heat(false, COOL / 2) > 0.0);
    }

    #[test]
    fn embers_rise_within_the_glass_and_burn_brighter_while_warm() {
        let (w, h) = (600.0, 400.0);
        for i in 0..EMBERS {
            for t in [0.0, 3.3, 47.0, 1234.5] {
                let e = ember(i, t, w, h, 0.0, 12);
                assert!(e.y >= -EMBER_MARGIN && e.y <= h + EMBER_MARGIN);
                assert!(e.x >= -15.0 && e.x <= w + 15.0);
                assert!(e.alpha >= 0.0 && e.alpha < 0.14);
                assert!(e.rune < 12);
                let warm = ember(i, t, w, h, 1.0, 12);
                assert!(warm.alpha >= e.alpha);
                assert_eq!((warm.x, warm.y), (e.x, e.y));
            }
            // Later, it is higher up, unless it went round.
            let a = ember(i, 10.0, w, h, 0.0, 12);
            let b = ember(i, 10.5, w, h, 0.0, 12);
            assert!(b.y < a.y || b.y > h);
        }
    }
}
