//! What the Folio's view says and where things go, without a window: a run
//! of tool steps as one line, a turn's footer, the column the conversation
//! is read in, how far it scrolls, and the runes drifting up behind it.
//! The view itself is `folio`, Windows only.

use std::f32::consts::{PI, TAU};
use std::path::Path;
use std::time::Duration;

use horadric_core::folio::{AskState, Ending, Step, StepState};

use crate::layout::Rect;

/// The widest the conversation reads: about 70 characters at the reading
/// size. Past 75 the eye loses its place on the way back to the next
/// line's start.
pub const COLUMN_MAX: f32 = 600.0;
/// The least room either side of the column.
pub const COLUMN_MARGIN: f32 = 22.0;
/// How much of its strength a rune keeps behind the words. Moving shapes
/// under text slow reading, so the runes stay lively in the margins and
/// quiet where the eye reads.
pub const CALM: f32 = 0.3;
/// How far either side of the column the runes take to calm, so no edge
/// shows where the column is.
const CALM_EDGE: f32 = 60.0;
/// How long the background takes to warm when a turn starts.
pub const WARM: Duration = Duration::from_millis(1200);
/// And to cool after it ends, after which nothing moves and nothing draws.
pub const COOL: Duration = Duration::from_millis(2400);
/// The heat while only background agents work and Claude itself waits for
/// them: the runes keep moving, but calmer than a turn's full burn, so
/// work is seen to go on without reading as Claude at it.
pub const AGENTS_HEAT: f32 = 0.35;
/// One depth of runes behind the conversation. Nearer is bigger, faster,
/// and less sunk into the background, which is what makes the eye read
/// the three as depth and not as runes of three sizes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layer {
    /// How many drift at rest.
    pub count: usize,
    /// How many more join them while Claude works, so a turn running
    /// reads from across the room.
    pub busy: usize,
    /// Smallest and largest height, in DIPs.
    pub size: (f32, f32),
    /// Slowest and fastest rise, in DIPs a second.
    pub speed: (f32, f32),
    /// Faintest and strongest at rest.
    pub alpha: (f32, f32),
    /// How much stronger at full heat.
    pub burn: f32,
    /// How many times its rest speed it gains at full heat. Near layers
    /// gain far more than far ones, so a turn feels like moving forward.
    pub warp: f32,
    /// How far it swings side to side.
    pub sway: f32,
    /// How far its colour is sunk toward the background, 0 to 1.
    pub sunk: f32,
    /// Its stroke as a share of its height.
    pub stroke: f32,
}

/// The depths, farthest first, the order they are drawn in.
pub const LAYERS: [Layer; 3] = [
    // A field of specks sunk into the background. At rest they still
    // have to read on a bright screen in daylight, so heat adds less on
    // top: more of them and a faster drift tell a turn apart.
    Layer {
        count: 44,
        busy: 16,
        size: (4.0, 7.0),
        speed: (2.0, 4.5),
        alpha: (0.16, 0.26),
        burn: 0.7,
        warp: 0.6,
        sway: 4.0,
        sunk: 0.3,
        stroke: 0.18,
    },
    Layer {
        count: 16,
        busy: 12,
        size: (9.0, 18.0),
        speed: (7.0, 16.0),
        alpha: (0.18, 0.3),
        burn: 0.9,
        warp: 1.5,
        sway: 14.0,
        sunk: 0.1,
        stroke: 0.12,
    },
    // A few drifting past close by, big and faint so they never fight
    // the words.
    Layer {
        count: 4,
        busy: 3,
        size: (30.0, 44.0),
        speed: (24.0, 36.0),
        alpha: (0.08, 0.13),
        burn: 0.8,
        warp: 3.5,
        sway: 26.0,
        sunk: 0.0,
        stroke: 0.07,
    },
];

/// The share of the screen a pane takes that still shows every rune: one
/// of two side by side. Smaller panes show fewer, so a grid of four or six
/// fields of runes reads as calm as one or two do, not as a swarm.
const ROOMY: f32 = 0.4;

/// Which embers of `l` a pane taking `share` of its screen shows, the
/// first ones of the rest and the busy alike so a pane growing or
/// shrinking only adds or drops a few.
pub fn shown(l: &Layer, share: f32) -> impl Iterator<Item = usize> {
    let k = (share / ROOMY).clamp(0.0, 1.0);
    let n = |c: usize| (c as f32 * k).ceil() as usize;
    (0..n(l.count)).chain(l.count..l.count + n(l.busy))
}

/// Where the column starts and how wide it is, in a view `width` wide.
pub fn column(width: f32) -> (f32, f32) {
    let w = (width - 2.0 * COLUMN_MARGIN).clamp(0.0, COLUMN_MAX);
    ((width - w) / 2.0, w)
}

/// How much of its strength a rune centred at `x` keeps, with the column
/// at `cx` and `cw` wide: [`CALM`] over it, all of it well clear of it.
pub fn calm(x: f32, (cx, cw): (f32, f32)) -> f32 {
    let out = (cx - x).max(x - (cx + cw)).max(0.0);
    let k = (out / CALM_EDGE).min(1.0);
    let smooth = k * k * (3.0 - 2.0 * k);
    CALM + (1.0 - CALM) * smooth
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

/// A turn's footer: how it ended and how long it took. No dollars: on a
/// subscription the API price is not what the human pays.
pub fn footer_line(duration_ms: u64, ending: Ending) -> String {
    let secs = duration_ms as f64 / 1000.0;
    let time = if secs < 60.0 {
        format!("{secs:.1} s")
    } else {
        let whole = secs.round() as u64;
        format!("{} min {} s", whole / 60, whole % 60)
    };
    match ending {
        Ending::Done => time,
        Ending::Stopped => format!("Stopped \u{b7} {time}"),
        Ending::Failed => format!("Ended with an error \u{b7} {time}"),
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
        Kind::Ran => "Run a command?".to_string(),
        Kind::Edited => "Change a file?".to_string(),
        Kind::Read => "Read a file?".to_string(),
        Kind::Fetched => "Go online?".to_string(),
        Kind::Other(name) => format!("Use {name}?"),
        _ => format!("Use {tool}?"),
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

/// The shortest the scroll bar's thumb gets, so a long session's is still
/// there to grab.
const THUMB_MIN: f32 = 28.0;

fn thumb_len(content: f32, view: f32, track: f32) -> f32 {
    (track * view / content).clamp(THUMB_MIN.min(track), track)
}

/// The scroll bar's thumb along a track `track` long: where it starts and
/// how long it is. None when everything fits and there is no bar.
pub fn thumb(scroll: f32, content: f32, view: f32, track: f32) -> Option<(f32, f32)> {
    let most = content - view;
    if most <= 1.0 || track <= 0.0 {
        return None;
    }
    let len = thumb_len(content, view, track);
    Some(((track - len) * scroll.clamp(0.0, most) / most, len))
}

/// The scroll that puts the thumb's start `at` along the track.
pub fn scroll_at(at: f32, content: f32, view: f32, track: f32) -> f32 {
    let room = track - thumb_len(content, view, track);
    if room <= 0.0 {
        return 0.0;
    }
    clamp_scroll(at / room * (content - view), content, view)
}

/// How warm the background is, from 0 at rest to 1 while a turn runs,
/// on its way from `from` to `to` for `since`: warming takes [`WARM`],
/// cooling [`COOL`].
pub fn heat(from: f32, to: f32, since: Duration) -> f32 {
    let span = if to > from { WARM } else { COOL };
    let k = (since.as_secs_f32() / span.as_secs_f32()).min(1.0);
    from + (to - from) * k
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
    /// How far its colour is sunk toward the background.
    pub sunk: f32,
    /// Its stroke width, in DIPs.
    pub stroke: f32,
}

/// FNV-1a over the words, stable across runs and builds.
fn hash(words: &[u64]) -> u64 {
    words.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &w| {
        w.to_le_bytes()
            .iter()
            .fold(h, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
    })
}

/// Ember `i` of `layer` at `t` seconds of the background's own clock,
/// with `warp` the seconds of it spent warm (each weighed by the heat), in
/// a view `w` by `h`, with `runes` to pick from. A function of the time
/// alone, so the view keeps no particles. Its speed never changes: the
/// heat moves it along through `warp`, which only ever grows smoothly, so
/// no rune jumps when a turn starts or ends. Those past the layer's count
/// show only while warm.
pub fn ember(
    layer: usize,
    i: usize,
    (t, warp): (f32, f32),
    (w, h): (f32, f32),
    heat: f32,
    runes: usize,
) -> Ember {
    let l = &LAYERS[layer];
    let u = |salt: u64| (hash(&[i as u64, salt, layer as u64]) % 10_000) as f32 / 10_000.0;
    let share = if i < l.count { 1.0 } else { heat };
    let span = |(lo, hi): (f32, f32), k: f32| lo + (hi - lo) * k;
    let size = span(l.size, u(6));
    let speed = span(l.speed, u(1));
    let margin = margin(l);
    let travel = h + 2.0 * margin;
    let p = (u(2) * travel + (t + l.warp * warp) * speed).rem_euclid(travel);
    let sway = (t * (0.2 + 0.2 * u(3)) + u(4) * TAU).sin() * l.sway;
    let life = (PI * p / travel).sin();
    Ember {
        x: u(5) * w + sway - size / 2.0,
        y: h + margin - p,
        size,
        alpha: life * span(l.alpha, u(7)) * (1.0 + l.burn * heat) * share,
        rune: (hash(&[i as u64, 8, layer as u64]) % runes.max(1) as u64) as usize,
        sunk: l.sunk,
        stroke: (size * l.stroke).max(0.8),
    }
}

/// Room above and below the glass an ember of `l` is born and dies in, so
/// it never pops in or out where it can be seen.
fn margin(l: &Layer) -> f32 {
    l.size.1 + 12.0
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
            input: serde_json::Value::Null,
            edit: None,
            lane: None,
            output: String::new(),
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
            "Use lore_note?"
        );
        assert_eq!(ask_title("Bash"), "Run a command?");
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
    fn the_footer_says_how_it_ended_and_the_time() {
        assert_eq!(footer_line(4321, Ending::Done), "4.3 s");
        assert_eq!(footer_line(125_000, Ending::Done), "2 min 5 s");
        assert_eq!(
            footer_line(900, Ending::Failed),
            "Ended with an error \u{b7} 0.9 s"
        );
        assert_eq!(footer_line(5180, Ending::Stopped), "Stopped \u{b7} 5.2 s");
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
    fn runes_are_quiet_behind_the_words_and_whole_in_the_margins() {
        let col = (200.0, 600.0);
        assert_eq!(calm(500.0, col), CALM);
        assert_eq!(calm(200.0, col), CALM);
        assert_eq!(calm(800.0, col), CALM);
        assert_eq!(calm(100.0, col), 1.0);
        assert_eq!(calm(900.0, col), 1.0);
        let near = calm(170.0, col);
        assert!(near > CALM && near < 1.0);
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
    fn the_thumb_shows_the_view_and_dragging_it_scrolls_back() {
        assert_eq!(thumb(0.0, 300.0, 400.0, 400.0), None);
        assert_eq!(thumb(0.0, 1000.0, 400.0, 400.0), Some((0.0, 160.0)));
        assert_eq!(thumb(600.0, 1000.0, 400.0, 400.0), Some((240.0, 160.0)));
        // A long session's thumb stays big enough to grab.
        assert_eq!(thumb(0.0, 100_000.0, 400.0, 400.0).unwrap().1, THUMB_MIN);
        assert_eq!(scroll_at(240.0, 1000.0, 400.0, 400.0), 600.0);
        assert_eq!(scroll_at(120.0, 1000.0, 400.0, 400.0), 300.0);
        assert_eq!(scroll_at(-50.0, 1000.0, 400.0, 400.0), 0.0);
        assert_eq!(scroll_at(999.0, 1000.0, 400.0, 400.0), 600.0);
    }

    #[test]
    fn the_background_warms_with_a_turn_and_cools_to_rest() {
        assert_eq!(heat(0.0, 1.0, Duration::ZERO), 0.0);
        assert_eq!(heat(0.0, 1.0, WARM * 2), 1.0);
        assert_eq!(heat(1.0, 0.0, Duration::ZERO), 1.0);
        assert_eq!(heat(1.0, 0.0, COOL), 0.0);
        assert!(heat(1.0, 0.0, COOL / 2) > 0.0);
        // A turn's end with agents still at work settles calmer, not cold.
        assert!((heat(1.0, AGENTS_HEAT, COOL * 2) - AGENTS_HEAT).abs() < 1e-6);
    }

    #[test]
    fn embers_rise_within_the_glass_and_burn_brighter_while_warm() {
        let view = (600.0, 400.0);
        for (k, l) in LAYERS.iter().enumerate() {
            let m = margin(l);
            for i in 0..l.count {
                for t in [0.0, 3.3, 47.0, 1234.5] {
                    let e = ember(k, i, (t, 0.0), view, 0.0, 12);
                    assert!(e.y >= -m && e.y <= view.1 + m);
                    assert!(e.x >= -m && e.x <= view.0 + m);
                    assert!(e.alpha >= 0.0 && e.alpha <= l.alpha.1);
                    assert!(e.rune < 12);
                    let warm = ember(k, i, (t, 0.0), view, 1.0, 12);
                    assert!(warm.alpha >= e.alpha);
                    assert_eq!((warm.x, warm.y), (e.x, e.y));
                }
                // Later, it is higher up, unless it went round.
                let a = ember(k, i, (10.0, 0.0), view, 0.0, 12);
                let b = ember(k, i, (10.5, 0.0), view, 0.0, 12);
                assert!(b.y < a.y || b.y > view.1);
            }
        }
    }

    #[test]
    fn small_panes_show_fewer_runes() {
        let l = &LAYERS[0];
        assert_eq!(shown(l, 0.9).count(), l.count + l.busy);
        assert_eq!(shown(l, ROOMY).count(), l.count + l.busy);
        let quarter: Vec<_> = shown(l, 0.2).collect();
        assert_eq!(quarter.len(), l.count / 2 + l.busy / 2);
        assert!(quarter.contains(&l.count), "the busy ones still join");
    }

    #[test]
    fn more_runes_join_only_while_warm() {
        let view = (600.0, 400.0);
        for (k, l) in LAYERS.iter().enumerate() {
            for i in l.count..l.count + l.busy {
                for t in [0.0, 3.3, 47.0, 1234.5] {
                    assert_eq!(ember(k, i, (t, 0.0), view, 0.0, 12).alpha, 0.0);
                }
                let lit = (0..80).any(|t| ember(k, i, (t as f32, 0.0), view, 1.0, 12).alpha > 0.0);
                assert!(lit);
            }
        }
    }

    #[test]
    fn nearer_is_bigger_faster_and_less_sunk() {
        for pair in LAYERS.windows(2) {
            let (far, near) = (pair[0], pair[1]);
            assert!(near.size.0 > far.size.1);
            assert!(near.speed.0 > far.speed.1);
            assert!(near.warp > far.warp);
            assert!(near.sunk < far.sunk);
        }
    }

    #[test]
    fn warmth_carries_near_runes_further_than_far_ones() {
        let view = (600.0, 400.0);
        let moved = |k: usize| {
            let a = ember(k, 0, (10.0, 0.0), view, 1.0, 12).y;
            let b = ember(k, 0, (10.0, 0.1), view, 1.0, 12).y;
            (a - b).rem_euclid(view.1 + 2.0 * margin(&LAYERS[k]))
        };
        assert!(moved(2) > moved(1) && moved(1) > moved(0));
    }
}
