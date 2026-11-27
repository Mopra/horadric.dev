//! What the overlay decides, apart from how it is drawn: what a line of
//! the history says, when bypass ends, how a glide moves, where the
//! plate's controls are. Pure, so it is tested without a window.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use horadric_core::session::{Phase, WaitReason};

use super::shells::{FOLD_H, SHELLS_GAP};
use crate::app::guard;
use crate::motion;
use crate::theme::{self, Color};

/// Control counts as running this long after the last desktop call.
pub(crate) const ACTIVE_FOR: Duration = Duration::from_secs(20);
/// How long the mark takes to reach a click until the plate's slider says
/// otherwise. Slow, so the human can see where it goes and stop it before
/// it acts.
pub(crate) const GLIDE: Duration = Duration::from_secs(2);
/// The slider's two ends.
pub(crate) const GLIDE_FAST: Duration = Duration::from_millis(300);
pub(crate) const GLIDE_SLOW: Duration = Duration::from_secs(4);
/// The mark stays this long after its last action, then fades.
pub(crate) const GHOST_STAYS: Duration = Duration::from_secs(3);
pub(crate) const GHOST_FADE: Duration = Duration::from_millis(400);
pub(crate) const FADE_IN: Duration = Duration::from_millis(250);
pub(crate) const FADE_OUT: Duration = Duration::from_millis(700);
/// Lines of history the plate keeps.
pub(crate) const HISTORY: usize = 10;

/// How long Esc is held before it stops what the session does. A tap is
/// the program's own Esc and stops nothing here.
pub(crate) const ESC_HOLD: Duration = Duration::from_millis(600);
/// How long the mouse is held on Bypass before it turns on, so a click
/// that strays onto it turns nothing on.
pub(crate) const BYPASS_HOLD: Duration = Duration::from_millis(700);
/// Bypass never lasts longer than this, whatever the sessions do.
pub(crate) const BYPASS_MAX: Duration = Duration::from_secs(30 * 60);

/// The plate's width, its padding, and its distance from the work area's
/// corner, clear of the runes round the screen, in DIPs.
pub(crate) const BOX_W: f32 = 380.0;
pub(crate) const PAD: f32 = 14.0;
pub(crate) const BOX_MARGIN: f32 = 22.0;
pub(crate) const ROW: f32 = 18.0;
/// The control row under the title: the speed slider on the left, Bypass
/// on the right, in DIPs from the plate's corner.
pub(crate) const CONTROLS_Y: f32 = 40.0;
pub(crate) const CONTROLS_H: f32 = 24.0;
pub(crate) const CONTROLS_STEP: f32 = 30.0;
pub(crate) const SLIDER_X: f32 = PAD + 48.0;
pub(crate) const SLIDER_W: f32 = 140.0;
/// Room beside the slider's ends that still takes the knob.
pub(crate) const SLIDER_SLACK: f32 = 8.0;
pub(crate) const BYPASS_W: f32 = 100.0;
pub(crate) const BYPASS_X: f32 = BOX_W - PAD - BYPASS_W;
/// The warning band under the controls while bypass is on.
pub(crate) const BANNER: f32 = 26.0;
/// How long bypass waits for a session that was resting when it was turned
/// on to start working, and for one that waits for the human to go on,
/// before it counts that session's work as done.
pub(crate) const BYPASS_IDLE: Duration = Duration::from_secs(60);
/// The reason gets at most this many lines.
pub(crate) const REASON_LINES: f32 = 3.0;
pub(crate) const REASON_LINE: f32 = 17.0;

// Glyphs of the system's icon font.
pub(crate) const CHECK: char = '\u{E73E}';
pub(crate) const ERROR: char = '\u{E783}';
pub(crate) const CROSS: char = '\u{E711}';
pub(crate) const STOP: char = '\u{E71A}';

/// How an action in the history went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    Ok,
    Failed,
    /// The human said no in a question.
    Refused,
    /// The human pressed Esc while the ghost was on its way.
    Stopped,
}

impl Mark {
    pub(crate) fn glyph(self) -> char {
        match self {
            Mark::Ok => CHECK,
            Mark::Failed => ERROR,
            Mark::Refused => CROSS,
            Mark::Stopped => STOP,
        }
    }

    pub(crate) fn colour(self) -> Color {
        match self {
            Mark::Ok => theme::done(),
            Mark::Failed | Mark::Stopped => theme::error(),
            Mark::Refused => theme::waiting(),
        }
    }
}

/// One line of the history.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    pub at: String,
    pub mark: Mark,
    pub what: String,
    pub outcome: String,
    /// Bypass let it through without asking.
    pub unasked: bool,
}

/// The plate's line for an entry, after its time and mark. What bypass let
/// through says so first, where a long line is not cut, so the human sees
/// afterwards what went unasked.
pub(crate) fn entry_line(e: &Entry) -> String {
    let line = match (e.what.trim(), e.outcome.trim()) {
        ("", "") => String::new(),
        (what, "") => what.to_string(),
        ("", outcome) => outcome.to_string(),
        (what, outcome) => format!("{what}: {outcome}"),
    };
    match (e.unasked, line.is_empty()) {
        (false, _) => line,
        (true, true) => "unasked".into(),
        (true, false) => format!("unasked: {line}"),
    }
}

/// Notes an action bypass let through, until its `desktop.done` comes.
pub(crate) fn note_unasked(unasked: &mut VecDeque<String>, what: &str) {
    unasked.push_back(what.to_string());
    while unasked.len() > HISTORY {
        unasked.pop_front();
    }
}

/// Whether a finished action was one bypass let through, forgetting it.
pub(crate) fn take_unasked(unasked: &mut VecDeque<String>, what: &str) -> bool {
    match unasked.iter().position(|w| w == what) {
        Some(i) => {
            unasked.remove(i);
            true
        }
        None => false,
    }
}

/// Whether a glide that has arrived is answered now. While the human has
/// Esc down the answer waits: a hold not yet long enough to count must
/// not lose to a mark that lands meanwhile, and the fastest glide is
/// shorter than the hold.
pub(crate) fn may_land(arrived: bool, esc_down: bool) -> bool {
    arrived && !esc_down
}

/// What Esc held does: whether it halts the glide on its way, and whether
/// it ends bypass. With nothing on its way it still ends bypass.
pub(crate) fn esc_does(gliding: bool, bypass: bool) -> (bool, bool) {
    (gliding, bypass)
}

/// Whether a queued call may start its glide. One only bypass let through
/// goes only where Esc can stop it; one the human allowed goes anyway.
pub(crate) fn may_glide(unasked: bool, have_esc: bool) -> bool {
    !unasked || have_esc
}

/// Whether a trip the guard noted stops the glide now on its way. One
/// noted for an earlier glide came too late and stops nothing.
pub(crate) fn trip_stops(tripped: u64, gliding: Option<u64>) -> bool {
    gliding == Some(tripped)
}

/// Where a session's turn stands, for when bypass ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Turn {
    /// At work: a tool, a prompt it is on, a permission or dialog mid turn.
    Working,
    /// Its turn is over: done, idle, failed, waiting for the human's words.
    Resting,
    /// Its process ended, or Horadric no longer has it.
    Gone,
}

/// A session's phase as a [`Turn`]. None is a session Horadric no longer has.
pub(crate) fn turn_of(phase: Option<&Phase>) -> Turn {
    match phase {
        None | Some(Phase::Ended | Phase::Paused) => Turn::Gone,
        Some(Phase::Working | Phase::Waiting(WaitReason::Permission | WaitReason::Dialog)) => {
            Turn::Working
        }
        Some(_) => Turn::Resting,
    }
}

/// Whether a session waits for the human mid turn: a permission prompt or
/// a dialog of its agent's. Its turn is not over, but it does nothing
/// until the human comes, so it keeps bypass no longer than a rest does.
pub(crate) fn asks_human(phase: Option<&Phase>) -> bool {
    matches!(
        phase,
        Some(Phase::Waiting(WaitReason::Permission | WaitReason::Dialog))
    )
}

/// A session bypass is on for: the one the plate showed when it was turned
/// on. Another session operating meanwhile never joins, so it can neither
/// use bypass nor keep it on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Holder {
    /// Seen at work since bypass was on for it.
    pub worked: bool,
    /// Its turn has ended since.
    pub ended: bool,
    /// Since when it has waited for the human without a break.
    pub asking: Option<Instant>,
}

impl Holder {
    /// It operated a program while bypass was on: it is at work again.
    /// Looking does not count, or a session that only watches would keep
    /// bypass on for ever.
    pub(crate) fn called(&mut self) {
        *self = Holder {
            worked: true,
            ended: false,
            asking: None,
        };
    }

    /// What its tile says at `now`. Resting ends it once it has worked, or
    /// once it rested the whole of [`BYPASS_IDLE`] after bypass was turned
    /// on (`waited`). Waiting for the human (`asking`) is not work: it
    /// ends it after [`BYPASS_IDLE`] of nothing but waiting.
    pub(crate) fn saw(&mut self, turn: Turn, asking: bool, waited: bool, now: Instant) {
        if asking {
            let since = *self.asking.get_or_insert(now);
            if now.duration_since(since) >= BYPASS_IDLE {
                self.ended = true;
            }
            return;
        }
        self.asking = None;
        match turn {
            Turn::Working => self.worked = true,
            Turn::Gone => self.ended = true,
            Turn::Resting if self.worked || waited => self.ended = true,
            Turn::Resting => {}
        }
    }
}

/// Whether a tool is one of the takeover tools, under any MCP prefix and
/// by either family of names: the `takeover_` ones, and the `desktop_`
/// ones they were first shipped as and are still answered by.
pub(crate) fn is_takeover_tool(tool: &str) -> bool {
    let bare = tool.rsplit("__").next().unwrap_or(tool);
    ["takeover_", "desktop_"]
        .iter()
        .any(|family| bare.starts_with(family))
}

/// Where a tool works, in a few words for the plate.
pub(crate) fn tool_place(tool: &str) -> String {
    let bare = tool.rsplit("__").next().unwrap_or(tool);
    match bare {
        "Bash" | "PowerShell" | "BashOutput" | "KillShell" => "in the shell".into(),
        "Read" | "Edit" | "Write" | "MultiEdit" | "Grep" | "Glob" | "NotebookEdit" => {
            "on files".into()
        }
        "WebFetch" | "WebSearch" => "on the web".into(),
        "Agent" | "Task" => "with a subagent".into(),
        b if b.starts_with("browser_") => "in the browser".into(),
        b => format!("with {b}"),
    }
}

/// What NOW says instead of the last desktop action, when the session no
/// longer works in the program: None while that action is still its latest
/// doing.
pub(crate) fn away_of(
    tool: Option<&str>,
    turn: Turn,
    asking: bool,
    program: &str,
) -> Option<String> {
    let program = match program.trim() {
        "" => "the program".to_string(),
        p => p.to_string(),
    };
    if asking && turn == Turn::Working {
        return Some(format!(
            "Waiting for you in its terminal, not working in {program}."
        ));
    }
    match (turn, tool) {
        (Turn::Gone, _) => Some("The session has ended.".into()),
        (Turn::Resting, _) => Some(format!("Finished its turn, not working in {program}.")),
        (Turn::Working, Some(t)) if !is_takeover_tool(t) => Some(format!(
            "Working {} ({}), not in {program}.",
            tool_place(t),
            t.rsplit("__").next().unwrap_or(t)
        )),
        _ => None,
    }
}

/// How bypass ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BypassEnd {
    /// The human held Esc.
    Esc,
    /// The human clicked the switch on the plate.
    Switched,
    /// The work it was on for is done.
    Done,
    /// It was on for [`BYPASS_MAX`].
    Ceiling,
    /// The human turned takeover off in the tray menu.
    Off,
}

/// The history's words for bypass turned on (`None`) or ended, what and
/// outcome, so the history shows the whole period it was on and who
/// ended it.
pub(crate) fn bypass_words(end: Option<BypassEnd>) -> (&'static str, &'static str) {
    match end {
        None => ("Bypass on", ""),
        Some(BypassEnd::Esc) => ("Bypass off", "Esc held"),
        Some(BypassEnd::Switched) => ("Bypass off", "switched off"),
        Some(BypassEnd::Done) => ("Bypass off", "the work it was on for is done"),
        Some(BypassEnd::Ceiling) => ("Bypass off", "on for 30 minutes, its limit"),
        Some(BypassEnd::Off) => ("Bypass off", "takeover turned off"),
    }
}

/// Whether bypass has outlived the work it was turned on for: every
/// session it is on for has ended its turn and no action is on its way.
/// On for nobody, it is over at once.
pub(crate) fn bypass_over(holders: &HashMap<String, Holder>, in_flight: bool) -> bool {
    !in_flight && holders.values().all(|h| h.ended)
}

/// Whether bypass has been on as long as it ever may.
pub(crate) fn bypass_spent(on_for: Duration) -> bool {
    on_for >= BYPASS_MAX
}

/// Whether a call keeps bypass on for its session: one that operates a
/// program. The app does not say which a call is, so it is read off the
/// call: a click has a point, and what only bypass let through was held.
pub(crate) fn keeps_bypass(point: bool, unasked: bool) -> bool {
    point || unasked
}

/// Bypass is on, for the hook thread, which notes turns only then.
pub(crate) static BYPASS_LIVE: AtomicBool = AtomicBool::new(false);
/// Sessions whose turn ended while bypass was on, from the hook thread,
/// until the overlay's next look.
static TURNS_ENDED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// A session's turn ended, its `Stop`, `StopFailure` or `SessionEnd`. Called
/// from the thread that reads hooks, so the overlay takes it on its frame.
pub(crate) fn turn_ended(session: &str) {
    if !BYPASS_LIVE.load(Ordering::Acquire) {
        return;
    }
    if let Ok(mut ended) = TURNS_ENDED.lock() {
        if !ended.iter().any(|s| s == session) {
            ended.push(session.to_string());
        }
    }
}

pub(crate) fn take_turns_ended() -> Vec<String> {
    TURNS_ENDED
        .lock()
        .map(|mut e| std::mem::take(&mut *e))
        .unwrap_or_default()
}

/// A wall clock time as the history shows it.
pub(crate) fn clock(h: u16, m: u16, s: u16) -> String {
    format!("{h:02}:{m:02}:{s:02}")
}

/// `e` on top of `history`, the oldest dropped past [`HISTORY`].
pub(crate) fn remember(history: &mut VecDeque<Entry>, e: Entry) {
    history.push_front(e);
    history.truncate(HISTORY);
}

/// Until when control counts as running after a call at `now`. An allowed
/// call starts it; any other call only keeps it going.
pub(crate) fn extend(until: Option<Instant>, now: Instant, allowed: bool) -> Option<Instant> {
    match until {
        Some(u) if u > now => Some(now + ACTIVE_FOR),
        _ if allowed => Some(now + ACTIVE_FOR),
        _ => None,
    }
}

/// Where the ghost is `elapsed` into a glide from `from` to `to` that
/// `takes` that long, soft at both ends.
pub(crate) fn glide_at(
    from: (f32, f32),
    to: (f32, f32),
    elapsed: Duration,
    takes: Duration,
) -> (f32, f32) {
    let t = motion::ease_in_out(motion::progress(elapsed, takes));
    (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t)
}

/// How much of a glide is left, 1 to 0, for the bar that counts it down.
pub(crate) fn left_of_glide(elapsed: Duration, takes: Duration) -> f32 {
    1.0 - motion::progress(elapsed, takes)
}

/// A glide kept between the slider's ends.
pub(crate) fn clamp_glide(d: Duration) -> Duration {
    d.clamp(GLIDE_FAST, GLIDE_SLOW)
}

/// The glide a place on the slider sets, 0 its left end and 1 its right.
/// Right is fast, as a speed reads, and it moves in whole tenths of a
/// second, as its label shows them.
pub(crate) fn glide_of_slider(f: f32) -> Duration {
    let f = if f.is_nan() { 0.0 } else { f.clamp(0.0, 1.0) };
    let (fast, slow) = (GLIDE_FAST.as_secs_f32(), GLIDE_SLOW.as_secs_f32());
    let tenths = ((slow - f * (slow - fast)) * 10.0).round() as u64;
    clamp_glide(Duration::from_millis(tenths * 100))
}

/// Where on the slider a glide sits, 0 to 1.
pub(crate) fn slider_of_glide(d: Duration) -> f32 {
    let (fast, slow) = (GLIDE_FAST.as_secs_f32(), GLIDE_SLOW.as_secs_f32());
    (slow - clamp_glide(d).as_secs_f32()) / (slow - fast)
}

/// Where along the slider a point `x` DIPs across the plate falls.
pub(crate) fn slider_at(x: f32) -> f32 {
    ((x - SLIDER_X) / SLIDER_W).clamp(0.0, 1.0)
}

/// The glide's seconds beside the slider.
pub(crate) fn speed_label(d: Duration) -> String {
    format!("{:.1} s", d.as_secs_f32())
}

/// The glide as kept in the state file, none for the default.
pub(crate) fn glide_from_saved(ms: Option<u32>) -> Duration {
    ms.map_or(GLIDE, |ms| clamp_glide(Duration::from_millis(ms.into())))
}

pub(crate) fn glide_to_saved(d: Duration) -> Option<u32> {
    (d != GLIDE).then(|| u32::try_from(d.as_millis()).unwrap_or(u32::MAX))
}

/// A control on the plate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Control {
    Speed,
    Bypass,
    /// The shell section's header, which folds it.
    Shells,
}

/// The control under a point `x`, `y` DIPs from the plate's corner, with
/// the shell section beginning at `shells` when the plate has one.
pub(crate) fn control_at(x: f32, y: f32, shells: Option<f32>) -> Option<Control> {
    if let Some(top) = shells {
        let header = top + SHELLS_GAP;
        if (header..header + FOLD_H).contains(&y) && (PAD..BOX_W - PAD).contains(&x) {
            return Some(Control::Shells);
        }
    }
    if !(CONTROLS_Y..CONTROLS_Y + CONTROLS_H).contains(&y) {
        return None;
    }
    if (SLIDER_X - SLIDER_SLACK..SLIDER_X + SLIDER_W + SLIDER_SLACK).contains(&x) {
        Some(Control::Speed)
    } else if (BYPASS_X..BYPASS_X + BYPASS_W).contains(&x) {
        Some(Control::Bypass)
    } else {
        None
    }
}

/// Whether the slider follows the mouse: only while the human's own
/// press, as the mouse hook saw it, went down on the slider and the
/// hardware says the button is still down. `pressed_on` is the control
/// under that press, none when the hook saw none. A posted message says
/// nothing of who sent it, and the speed is kept across restarts.
pub(crate) fn slider_follows(pressed_on: Option<Control>, button_down: bool) -> bool {
    button_down && pressed_on == Some(Control::Speed)
}

fn window_named(title: &str) -> String {
    match title.trim() {
        "" => "a window with no title".into(),
        t => format!("the window \"{t}\""),
    }
}

fn guard_did(kind: guard::Kind) -> &'static str {
    match kind {
        guard::Kind::Front => "came to the front",
        guard::Kind::Shown | guard::Kind::Dialog => "opened",
    }
}

/// What the agent is told when the guard stopped its action: what changed,
/// and that its point was chosen on a screen that is gone.
pub(crate) fn guard_error(title: &str, kind: guard::Kind, held: bool) -> String {
    let (wait, act) = if held {
        ("while Horadric held the action", "acting")
    } else {
        ("while the ghost was on its way", "clicking")
    };
    format!(
        "stopped: {} {} {wait}; look again before {act}",
        window_named(title),
        guard_did(kind)
    )
}

/// The history's word for the same.
pub(crate) fn guard_outcome(title: &str, kind: guard::Kind) -> String {
    format!("stopped: {} {}", window_named(title), guard_did(kind))
}

/// The mark's alpha `since` its last action.
pub(crate) fn ghost_alpha(since: Duration) -> f32 {
    match since.checked_sub(GHOST_STAYS) {
        None => 1.0,
        Some(fading) => 1.0 - motion::progress(fading, GHOST_FADE),
    }
}

/// The plate's place: the top right corner of the work area, `margin`
/// in from it.
pub(crate) fn box_place(work: [i32; 4], size: (i32, i32), margin: i32) -> (i32, i32) {
    let [l, t, r, _] = work;
    ((r - size.0 - margin).max(l), t + margin)
}

/// The plate's height in DIPs for a reason `reason_h` tall and `rows`
/// lines of history, with the bypass band when it is on, and a shell
/// section `shells` tall under it all.
pub(crate) fn box_height(reason_h: f32, rows: usize, bypass: bool, shells: f32) -> f32 {
    (body_height(reason_h, rows, bypass) + shells + PAD).ceil()
}

/// Where the shell section begins: under the controls, NOW and history.
pub(crate) fn body_height(reason_h: f32, rows: usize, bypass: bool) -> f32 {
    let reason = reason_h.min(REASON_LINES * REASON_LINE);
    let controls = CONTROLS_STEP + if bypass { BANNER } else { 0.0 };
    let now = CONTROLS_Y + controls + 22.0 + if reason > 0.0 { reason + 4.0 } else { 0.0 } + ROW;
    let history = if rows == 0 {
        0.0
    } else {
        14.0 + 22.0 + rows as f32 * ROW
    };
    now + history
}
