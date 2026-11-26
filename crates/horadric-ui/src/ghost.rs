//! What the human sees while a session drives a desktop program: every
//! screen framed by a pulsing glow, a ghost cursor that glides to where the
//! next click goes before it is made, and a Thinking plate in the top right
//! corner saying what the session is about to do and what it did.
//!
//! The ghost is slow on purpose. A click waits for it to arrive, two
//! seconds, and Esc while it glides stops that click: the human sees the
//! aim before the act and has time to say no.
//!
//! Every window here is layered with its own alpha, topmost, never
//! activated, and the mouse goes through it. Each is left out of screen
//! captures, so the session's own screenshots show the program and not the
//! frame around it.
//!
//! Its state lives here, off the app, since the frames come to its own
//! window and must not wait on the app's borrow.
//!
//! The plate is the one window here the mouse can reach, and only over its
//! controls: a slider for how long the ghost takes, kept across restarts,
//! and Bypass. Clicking them never activates it, so the program being
//! driven keeps the focus. Bypass lets what would be asked go unasked, for
//! a human who trusts the work and watches it: the frame turns to the
//! warning colour, each call it lets through is held for Esc as a click
//! is, and Esc then stops the call and turns bypass off. It starts off at
//! every start of the app and is never kept.
//!
//! Bypass is trust in the work at hand, so it ends with that work: once
//! every session it was on for has ended its turn (its `Stop`,
//! `StopFailure` or `SessionEnd`, or seen working and then resting) and no
//! action is on its way, it turns itself off and the plate says so.
//!
//! NOW says what the session last did in the program. When the session
//! goes on with other tools (a shell, files, the browser) or ends its turn,
//! the plate says that instead, so an old NOW is never taken for the
//! present. It names the session by its id and start too, since two
//! sessions can have the same name.
//!
//! While the ghost is on its way, [`super::guard`] watches for a window
//! opening or another program taking the front, and stops the click as
//! Esc would: the point was chosen on a screen that is no longer there.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::f32::consts::PI;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use horadric_core::desktop_programs::Source;
use horadric_core::event::HookEvent;
use horadric_core::folio::{read_result, ResultKind};
use horadric_core::session::{Phase, WaitReason};

use horadric_hooks::listener::Reply;
use serde_json::json;
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED,
    D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DCRenderTarget, ID2D1RenderTarget, ID2D1SolidColorBrush,
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_GAMMA_2_2,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteTextFormat, IDWriteTextLayout, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL, DWRITE_TEXT_METRICS, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EnumDisplayMonitors,
    GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
    HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, ReleaseCapture, SetCapture, UnregisterHotKey, MOD_NOREPEAT, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetWindowLongPtrW,
    RegisterClassW, SetCursorPos, SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, UpdateLayeredWindow, GWL_EXSTYLE, HTCLIENT, HTTRANSPARENT, MA_NOACTIVATE,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE,
    SW_SHOWNOACTIVATE, ULW_ALPHA, WDA_EXCLUDEFROMCAPTURE, WM_APP, WM_CAPTURECHANGED, WM_HOTKEY,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCHITTEST, WM_TIMER,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    WS_POPUP,
};

use windows_numerics::Vector2;

use super::guard;
use crate::motion;
use crate::render::{self, color};
use crate::theme::{self, Color};
use crate::window::Shared;

const CLASS: PCWSTR = w!("HoradricGhost");
const FRAME: usize = 1;
/// The id of the Esc that stops a glide, on the clock window, so no other
/// hotkey of the app's can share it.
const STOP_KEY: i32 = 0x6057;

/// What the guard posts to the clock window when it saw something.
const GUARD_MSG: u32 = WM_APP + 1;

/// Control counts as running this long after the last desktop call.
pub(super) const ACTIVE_FOR: Duration = Duration::from_secs(20);
/// How long the ghost takes to reach a click until the plate's slider says
/// otherwise. Slow, so the human can see where it goes and stop it before
/// it acts.
pub(super) const GLIDE: Duration = Duration::from_secs(2);
/// The slider's two ends.
pub(super) const GLIDE_FAST: Duration = Duration::from_millis(300);
pub(super) const GLIDE_SLOW: Duration = Duration::from_secs(4);
/// The ring the ghost gives when it arrives.
const RING: Duration = Duration::from_millis(450);
/// The ghost stays this long after its last action, then fades.
const GHOST_STAYS: Duration = Duration::from_secs(3);
const GHOST_FADE: Duration = Duration::from_millis(400);
/// One breath of the frame around the screens.
const PULSE: Duration = Duration::from_secs(2);
const FADE_IN: Duration = Duration::from_millis(250);
const FADE_OUT: Duration = Duration::from_millis(700);
/// Lines of history the plate keeps.
pub(super) const HISTORY: usize = 10;

/// The glow's depth into the screen, in DIPs.
const EDGE: f32 = 8.0;
/// The plate's width, its padding, and its distance from the work area's
/// corner, clear of the frame, in DIPs.
const BOX_W: f32 = 380.0;
const PAD: f32 = 14.0;
const BOX_MARGIN: f32 = 18.0;
const ROW: f32 = 18.0;
/// The control row under the title: the speed slider on the left, Bypass
/// on the right, in DIPs from the plate's corner.
const CONTROLS_Y: f32 = 40.0;
const CONTROLS_H: f32 = 24.0;
const CONTROLS_STEP: f32 = 30.0;
const SLIDER_X: f32 = PAD + 48.0;
const SLIDER_W: f32 = 140.0;
/// Room beside the slider's ends that still takes the knob.
const SLIDER_SLACK: f32 = 8.0;
const BYPASS_W: f32 = 100.0;
const BYPASS_X: f32 = BOX_W - PAD - BYPASS_W;
/// The warning band under the controls while bypass is on.
const BANNER: f32 = 26.0;
/// How long bypass waits for a session that was resting when it was turned
/// on to start working, before it counts that session's work as done.
pub(super) const BYPASS_IDLE: Duration = Duration::from_secs(60);
/// How often, while bypass is on, the sessions it is on for are looked at,
/// and how often the plate looks at what its session does now.
const TURNS_EVERY: Duration = Duration::from_millis(250);
/// What a call bypass let through is told when bypass ends before it went.
const BYPASS_ENDED: &str =
    "the human stopped bypass, so this action was not made; ask again and they will be asked";
/// What a call bypass let through is told when Esc cannot be taken, since
/// then nothing would let the human stop it.
const NO_ESC: &str = "Horadric could not take the Esc key, so the human could not stop this \
    action and bypass did not let it through; bypass can not be used now, ask again";
/// The reason gets at most this many lines.
const REASON_LINES: f32 = 3.0;
const REASON_LINE: f32 = 17.0;
/// The ghost's window, with the tip of its arrow at `GHOST_TIP` from its
/// corner, room around it for the ring and beside it for the name.
const GHOST_SIZE: (f32, f32) = (230.0, 86.0);
const GHOST_TIP: f32 = 38.0;
const TAG_MAX: f32 = 150.0;

// Glyphs of the system's icon font.
const CHECK: char = '\u{E73E}';
const ERROR: char = '\u{E783}';
const CROSS: char = '\u{E711}';
const STOP: char = '\u{E71A}';

/// How an action in the history went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mark {
    Ok,
    Failed,
    /// The human said no in a question.
    Refused,
    /// The human pressed Esc while the ghost was on its way.
    Stopped,
}

impl Mark {
    pub(super) fn glyph(self) -> char {
        match self {
            Mark::Ok => CHECK,
            Mark::Failed => ERROR,
            Mark::Refused => CROSS,
            Mark::Stopped => STOP,
        }
    }

    fn colour(self) -> Color {
        match self {
            Mark::Ok => theme::done(),
            Mark::Failed | Mark::Stopped => theme::error(),
            Mark::Refused => theme::waiting(),
        }
    }
}

/// One line of the history.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Entry {
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
pub(super) fn entry_line(e: &Entry) -> String {
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
pub(super) fn note_unasked(unasked: &mut VecDeque<String>, what: &str) {
    unasked.push_back(what.to_string());
    while unasked.len() > HISTORY {
        unasked.pop_front();
    }
}

/// Whether a finished action was one bypass let through, forgetting it.
pub(super) fn take_unasked(unasked: &mut VecDeque<String>, what: &str) -> bool {
    match unasked.iter().position(|w| w == what) {
        Some(i) => {
            unasked.remove(i);
            true
        }
        None => false,
    }
}

/// Whether Esc is held for the human: while a glide runs, so it can be
/// stopped, and the whole time bypass is on, as its banner promises.
pub(super) fn wants_esc(gliding: bool, bypass: bool) -> bool {
    gliding || bypass
}

/// What Esc does: whether it halts the glide on its way, and whether it
/// ends bypass. With nothing on its way it still ends bypass.
pub(super) fn esc_does(gliding: bool, bypass: bool) -> (bool, bool) {
    (gliding, bypass)
}

/// Whether a queued call may start its glide. One only bypass let through
/// goes only where Esc can stop it; one the human allowed goes anyway.
pub(super) fn may_glide(unasked: bool, have_esc: bool) -> bool {
    !unasked || have_esc
}

/// Whether a trip the guard noted stops the glide now on its way. One
/// noted for an earlier glide came too late and stops nothing.
pub(super) fn trip_stops(tripped: u64, gliding: Option<u64>) -> bool {
    gliding == Some(tripped)
}

/// Where a session's turn stands, for when bypass ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Turn {
    /// At work: a tool, a prompt it is on, a permission or dialog mid turn.
    Working,
    /// Its turn is over: done, idle, failed, waiting for the human's words.
    Resting,
    /// Its process ended, or Horadric no longer has it.
    Gone,
}

/// A session's phase as a [`Turn`]. None is a session Horadric no longer has.
pub(super) fn turn_of(phase: Option<&Phase>) -> Turn {
    match phase {
        None | Some(Phase::Ended | Phase::Paused) => Turn::Gone,
        Some(Phase::Working | Phase::Waiting(WaitReason::Permission | WaitReason::Dialog)) => {
            Turn::Working
        }
        Some(_) => Turn::Resting,
    }
}

/// A session bypass is on for: the one driving when it was turned on, and
/// each whose call came while it was on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Holder {
    /// Seen at work since bypass was on for it.
    pub worked: bool,
    /// Its turn has ended since.
    pub ended: bool,
}

impl Holder {
    /// A call of its came while bypass was on: it is at work again.
    pub(super) fn called(&mut self) {
        *self = Holder {
            worked: true,
            ended: false,
        };
    }

    /// What its tile says now. Resting ends it once it has worked, or once
    /// it rested the whole of [`BYPASS_IDLE`] after bypass was turned on.
    pub(super) fn saw(&mut self, turn: Turn, waited: bool) {
        match turn {
            Turn::Working => self.worked = true,
            Turn::Gone => self.ended = true,
            Turn::Resting if self.worked || waited => self.ended = true,
            Turn::Resting => {}
        }
    }
}

/// Whether a tool is one of the desktop tools, under any MCP prefix.
pub(super) fn is_desktop_tool(tool: &str) -> bool {
    tool.rsplit("__")
        .next()
        .unwrap_or(tool)
        .starts_with("desktop_")
}

/// Where a tool works, in a few words for the plate.
pub(super) fn tool_place(tool: &str) -> String {
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
pub(super) fn away_of(tool: Option<&str>, turn: Turn, program: &str) -> Option<String> {
    let program = match program.trim() {
        "" => "the program".to_string(),
        p => p.to_string(),
    };
    match (turn, tool) {
        (Turn::Gone, _) => Some("The session has ended.".into()),
        (Turn::Resting, _) => Some(format!("Finished its turn, not working in {program}.")),
        (Turn::Working, Some(t)) if !is_desktop_tool(t) => Some(format!(
            "Working {} ({}), not in {program}.",
            tool_place(t),
            t.rsplit("__").next().unwrap_or(t)
        )),
        _ => None,
    }
}

/// How bypass ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BypassEnd {
    /// The human pressed Esc.
    Esc,
    /// The human clicked the switch on the plate.
    Switched,
    /// The work it was on for is done.
    Done,
}

/// The history's words for bypass turned on (`None`) or ended, what and
/// outcome, so the history shows the whole period it was on and who
/// ended it.
pub(super) fn bypass_words(end: Option<BypassEnd>) -> (&'static str, &'static str) {
    match end {
        None => ("Bypass on", ""),
        Some(BypassEnd::Esc) => ("Bypass off", "Esc"),
        Some(BypassEnd::Switched) => ("Bypass off", "switched off"),
        Some(BypassEnd::Done) => ("Bypass off", "the work it was on for is done"),
    }
}

/// Whether bypass has outlived the work it was turned on for: every
/// session it is on for has ended its turn and no action is on its way.
pub(super) fn bypass_over(holders: &HashMap<String, Holder>, in_flight: bool) -> bool {
    !in_flight && !holders.is_empty() && holders.values().all(|h| h.ended)
}

/// Bypass is on, for the hook thread, which notes turns only then.
static BYPASS_LIVE: AtomicBool = AtomicBool::new(false);
/// Sessions whose turn ended while bypass was on, from the hook thread,
/// until the overlay's next look.
static TURNS_ENDED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// A session's turn ended, its `Stop`, `StopFailure` or `SessionEnd`. Called
/// from the thread that reads hooks, so the overlay takes it on its frame.
pub(super) fn turn_ended(session: &str) {
    if !BYPASS_LIVE.load(Ordering::Acquire) {
        return;
    }
    if let Ok(mut ended) = TURNS_ENDED.lock() {
        if !ended.iter().any(|s| s == session) {
            ended.push(session.to_string());
        }
    }
}

fn take_turns_ended() -> Vec<String> {
    TURNS_ENDED
        .lock()
        .map(|mut e| std::mem::take(&mut *e))
        .unwrap_or_default()
}

/// Shell commands the plate lists for one session, newest first.
pub(super) const SHELLS: usize = 8;
/// How many lines of a command show: the running one is what the human
/// must be able to read, the finished ones only to recognise.
const RUNNING_LINES: usize = 8;
const DONE_LINES: usize = 2;
/// The shell section's room above its header, the header, the padding
/// round a command's well and the room between commands, in DIPs.
const SHELLS_GAP: f32 = 14.0;
const FOLD_H: f32 = 22.0;
const WELL_PAD: f32 = 3.0;
const SHELL_GAP: f32 = 6.0;
/// A command's width in its well, and how much of it is laid out at all:
/// a script written inline can be thousands of lines, of which the plate
/// shows eight.
const COMMAND_W: f32 = BOX_W - 2.0 * PAD - 12.0;
const COMMAND_CHARS: usize = 4000;

// More glyphs of the system's icon font.
const RUNNING: char = '\u{E768}';
const CLOCK: char = '\u{E823}';
const OPEN: char = '\u{E70D}';
const FOLDED: char = '\u{E76C}';

/// Where a shell command the plate lists stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ShellState {
    Running,
    Done,
    /// It failed, with its exit code when it gave one.
    Failed(Option<i32>),
    /// The human stopped it.
    Interrupted,
    /// It ran past its timeout and goes on in the background.
    TimedOut,
    /// It was sent to the background from the start.
    Background,
}

impl ShellState {
    /// The state in a word or two, beside the command.
    pub(super) fn word(self) -> String {
        match self {
            ShellState::Running => "running".into(),
            ShellState::Done => "done".into(),
            ShellState::Failed(Some(code)) => format!("exit {code}"),
            ShellState::Failed(None) => "failed".into(),
            ShellState::Interrupted => "interrupted".into(),
            ShellState::TimedOut => "timed out, in background".into(),
            ShellState::Background => "in background".into(),
        }
    }

    fn glyph(self) -> char {
        match self {
            ShellState::Running => RUNNING,
            ShellState::Done => CHECK,
            ShellState::Failed(_) => ERROR,
            ShellState::Interrupted => STOP,
            ShellState::TimedOut | ShellState::Background => CLOCK,
        }
    }

    fn colour(self) -> Color {
        match self {
            ShellState::Running => theme::working(),
            ShellState::Done => theme::done(),
            ShellState::Failed(_) | ShellState::Interrupted => theme::error(),
            ShellState::TimedOut | ShellState::Background => theme::waiting(),
        }
    }

    /// How many lines of its command the plate shows.
    pub(super) fn lines(self) -> usize {
        match self {
            ShellState::Running => RUNNING_LINES,
            _ => DONE_LINES,
        }
    }
}

/// One shell command a session ran while control was on.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Shell {
    /// Claude Code's id for the call, which its end quotes back.
    pub id: String,
    /// `Bash` or `PowerShell`.
    pub tool: String,
    pub command: String,
    pub description: String,
    /// A subagent of the session ran it.
    pub agent: bool,
    pub at: String,
    pub state: ShellState,
}

impl Shell {
    /// What the row says the command is for: its description, else which
    /// shell it ran in.
    pub(super) fn title(&self) -> String {
        match self.description.trim() {
            "" => format!("{} command", self.tool),
            d => d.to_string(),
        }
    }
}

/// What the hook thread heard of a shell command, for the overlay.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum ShellNote {
    Began {
        session: String,
        shell: Shell,
    },
    Ended {
        session: String,
        id: String,
        state: ShellState,
    },
}

impl ShellNote {
    pub(super) fn session(&self) -> &str {
        match self {
            ShellNote::Began { session, .. } | ShellNote::Ended { session, .. } => session,
        }
    }
}

/// Whether a tool runs commands in a shell, under any MCP prefix.
pub(super) fn is_shell_tool(tool: &str) -> bool {
    matches!(
        tool.rsplit("__").next().unwrap_or(tool),
        "Bash" | "PowerShell"
    )
}

/// How a shell command ended, from its `PostToolUse` (`error` none) or
/// its `PostToolUseFailure`.
pub(super) fn shell_ended(
    tool: &str,
    input: &serde_json::Value,
    response: &serde_json::Value,
    error: Option<&str>,
    interrupt: bool,
) -> ShellState {
    if interrupt {
        return ShellState::Interrupted;
    }
    let bare = tool.rsplit("__").next().unwrap_or(tool);
    let read = match error {
        Some(e) => read_result(bare, input, e, true, &serde_json::Value::Null),
        None => {
            let text = response.as_str().unwrap_or_default();
            let detail = if response.is_object() {
                response
            } else {
                &serde_json::Value::Null
            };
            read_result(bare, input, text, false, detail)
        }
    };
    let ResultKind::Shell(sh) = read.kind else {
        return if error.is_some() {
            ShellState::Failed(None)
        } else {
            ShellState::Done
        };
    };
    if sh.interrupted {
        ShellState::Interrupted
    } else if sh.timed_out_ms.is_some() {
        ShellState::TimedOut
    } else if sh.background.is_some() {
        ShellState::Background
    } else {
        match (sh.exit_code, error.is_some()) {
            (Some(0) | None, false) => ShellState::Done,
            (code, _) => ShellState::Failed(code.filter(|c| *c != 0)),
        }
    }
}

/// What a hook event says of a shell command of session `target`, at
/// wall clock `at`. None for any other event or tool.
pub(super) fn shell_note(target: &str, e: &HookEvent, at: &str) -> Option<ShellNote> {
    let tool = e.tool_name.as_deref().filter(|t| is_shell_tool(t))?;
    let id = e.tool_use_id.clone().unwrap_or_default();
    let input = e.tool_input.clone().unwrap_or_default();
    let text = |key: &str| input[key].as_str().unwrap_or_default().to_string();
    let session = target.to_string();
    match e.hook_event_name.as_str() {
        "PreToolUse" => Some(ShellNote::Began {
            session,
            shell: Shell {
                id,
                tool: tool.rsplit("__").next().unwrap_or(tool).to_string(),
                command: text("command"),
                description: text("description"),
                agent: e.is_subagent(),
                at: at.to_string(),
                state: ShellState::Running,
            },
        }),
        "PostToolUse" | "PostToolUseFailure" => Some(ShellNote::Ended {
            session,
            id,
            state: shell_ended(
                tool,
                &input,
                e.tool_response.as_ref().unwrap_or(&serde_json::Value::Null),
                (e.hook_event_name == "PostToolUseFailure")
                    .then(|| e.error_text().unwrap_or_default()),
                e.interrupted(),
            ),
        }),
        _ => None,
    }
}

/// Takes a note into the sessions' lists, and gives back the command it
/// began or ended, for the log. An end whose start was never heard, one
/// begun before control, is dropped.
pub(super) fn hear_shell(
    lists: &mut HashMap<String, VecDeque<Shell>>,
    note: ShellNote,
) -> Option<Shell> {
    match note {
        ShellNote::Began { session, shell } => {
            let list = lists.entry(session).or_default();
            list.push_front(shell.clone());
            list.truncate(SHELLS);
            Some(shell)
        }
        ShellNote::Ended { session, id, state } => {
            let shell = lists
                .get_mut(&session)?
                .iter_mut()
                .find(|s| !id.is_empty() && s.id == id && s.state == ShellState::Running)?;
            shell.state = state;
            Some(shell.clone())
        }
    }
}

/// The fold's header: how many commands, and how many still run.
pub(super) fn shells_count(list: &VecDeque<Shell>) -> String {
    let running = list
        .iter()
        .filter(|s| s.state == ShellState::Running)
        .count();
    match (list.len(), running) {
        (0, _) => "none yet".into(),
        (n, 0) => format!("{n}"),
        (n, r) => format!("{n}, {r} running"),
    }
}

/// A command's height on the plate: its lines, at most as many as its
/// state shows.
pub(super) fn command_height(lines: usize, line_h: f32, state: ShellState) -> f32 {
    lines.clamp(1, state.lines()) as f32 * line_h
}

/// How many of a command's `lines` the plate leaves out in `state`.
pub(super) fn hidden_lines(lines: usize, state: ShellState) -> usize {
    lines.saturating_sub(state.lines())
}

/// The tag on a command cut short, none when it shows whole.
pub(super) fn more_lines(hidden: usize) -> Option<String> {
    match hidden {
        0 => None,
        1 => Some("+1 line".into()),
        n => Some(format!("+{n} lines")),
    }
}

/// The start of a command, as much as is laid out.
pub(super) fn command_shown(command: &str) -> &str {
    match command.char_indices().nth(COMMAND_CHARS) {
        Some((at, _)) => &command[..at],
        None => command,
    }
}

/// One listed command's whole row, `command_h` its command's height.
fn shell_row(command_h: f32) -> f32 {
    ROW + command_h + 2.0 * WELL_PAD + SHELL_GAP
}

/// The shell section's height: its header, and while open each command
/// in `commands` (their heights), or a line saying there are none.
pub(super) fn shells_height(open: bool, commands: &[f32]) -> f32 {
    let rows = match (open, commands.is_empty()) {
        (false, _) => 0.0,
        (true, true) => ROW,
        (true, false) => commands.iter().map(|&c| shell_row(c)).sum(),
    };
    SHELLS_GAP + FOLD_H + rows
}

/// How many commands, newest first, fit in `room` DIPs of plate, so the
/// plate never runs off the bottom of the screen.
pub(super) fn shells_fit(commands: &[f32], room: f32) -> usize {
    let mut left = room - SHELLS_GAP - FOLD_H;
    commands
        .iter()
        .take_while(|&&c| {
            left -= shell_row(c);
            left >= 0.0
        })
        .count()
}

/// One line of the desktop log for a shell command: when, which session,
/// what happened, in which shell, the command and what it is for.
pub(super) fn shell_log_line(utc: &str, session: &str, shell: &Shell) -> String {
    let flat = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let from = if shell.agent { " by a subagent" } else { "" };
    let how = match shell.state {
        ShellState::Running => format!("shell command started{from}"),
        state => format!("shell command {}{from}", state.word()),
    };
    let mut line = format!(
        "{utc}  {}  {how}  {}  ({})",
        flat(session),
        shell.tool,
        flat(&shell.command)
    );
    if !shell.description.trim().is_empty() {
        line.push_str(&format!("  [{}]", flat(&shell.description)));
    }
    line.push('\n');
    line
}

/// Desktop control is on, for the hook thread, which notes shell
/// commands only then: one begun before it is not the control's.
static SHELLS_LIVE: AtomicBool = AtomicBool::new(false);
/// Shell commands heard while control was on, until the overlay's next
/// frame.
static SHELL_NOTES: Mutex<Vec<ShellNote>> = Mutex::new(Vec::new());

/// A hook event for session `target`, from the thread that reads hooks:
/// a shell command's start or end is kept for the plate.
pub(super) fn shell_heard(target: &str, e: &HookEvent) {
    if !SHELLS_LIVE.load(Ordering::Acquire) {
        return;
    }
    let Some(note) = shell_note(target, e, &now_clock()) else {
        return;
    };
    if let Ok(mut notes) = SHELL_NOTES.lock() {
        notes.push(note);
    }
}

fn take_shell_notes() -> Vec<ShellNote> {
    SHELL_NOTES
        .lock()
        .map(|mut n| std::mem::take(&mut *n))
        .unwrap_or_default()
}

/// A wall clock time as the history shows it.
pub(super) fn clock(h: u16, m: u16, s: u16) -> String {
    format!("{h:02}:{m:02}:{s:02}")
}

/// `e` on top of `history`, the oldest dropped past [`HISTORY`].
pub(super) fn remember(history: &mut VecDeque<Entry>, e: Entry) {
    history.push_front(e);
    history.truncate(HISTORY);
}

/// Until when control counts as running after a call at `now`. An allowed
/// call starts it; any other call only keeps it going.
pub(super) fn extend(until: Option<Instant>, now: Instant, allowed: bool) -> Option<Instant> {
    match until {
        Some(u) if u > now => Some(now + ACTIVE_FOR),
        _ if allowed => Some(now + ACTIVE_FOR),
        _ => None,
    }
}

/// Where the ghost is `elapsed` into a glide from `from` to `to` that
/// `takes` that long, soft at both ends.
pub(super) fn glide_at(
    from: (f32, f32),
    to: (f32, f32),
    elapsed: Duration,
    takes: Duration,
) -> (f32, f32) {
    let t = motion::ease_in_out(motion::progress(elapsed, takes));
    (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t)
}

/// How much of a glide is left, 1 to 0, for the bar that counts it down.
pub(super) fn left_of_glide(elapsed: Duration, takes: Duration) -> f32 {
    1.0 - motion::progress(elapsed, takes)
}

/// A glide kept between the slider's ends.
pub(super) fn clamp_glide(d: Duration) -> Duration {
    d.clamp(GLIDE_FAST, GLIDE_SLOW)
}

/// The glide a place on the slider sets, 0 its left end and 1 its right.
/// Right is fast, as a speed reads, and it moves in whole tenths of a
/// second, as its label shows them.
pub(super) fn glide_of_slider(f: f32) -> Duration {
    let f = if f.is_nan() { 0.0 } else { f.clamp(0.0, 1.0) };
    let (fast, slow) = (GLIDE_FAST.as_secs_f32(), GLIDE_SLOW.as_secs_f32());
    let tenths = ((slow - f * (slow - fast)) * 10.0).round() as u64;
    clamp_glide(Duration::from_millis(tenths * 100))
}

/// Where on the slider a glide sits, 0 to 1.
pub(super) fn slider_of_glide(d: Duration) -> f32 {
    let (fast, slow) = (GLIDE_FAST.as_secs_f32(), GLIDE_SLOW.as_secs_f32());
    (slow - clamp_glide(d).as_secs_f32()) / (slow - fast)
}

/// Where along the slider a point `x` DIPs across the plate falls.
pub(super) fn slider_at(x: f32) -> f32 {
    ((x - SLIDER_X) / SLIDER_W).clamp(0.0, 1.0)
}

/// The glide's seconds beside the slider.
pub(super) fn speed_label(d: Duration) -> String {
    format!("{:.1} s", d.as_secs_f32())
}

/// The glide as kept in the state file, none for the default.
pub(super) fn glide_from_saved(ms: Option<u32>) -> Duration {
    ms.map_or(GLIDE, |ms| clamp_glide(Duration::from_millis(ms.into())))
}

pub(super) fn glide_to_saved(d: Duration) -> Option<u32> {
    (d != GLIDE).then(|| u32::try_from(d.as_millis()).unwrap_or(u32::MAX))
}

/// A control on the plate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Control {
    Speed,
    Bypass,
    /// The shell section's header, which folds it.
    Shells,
}

/// The control under a point `x`, `y` DIPs from the plate's corner, with
/// the shell section beginning at `shells` when the plate has one.
pub(super) fn control_at(x: f32, y: f32, shells: Option<f32>) -> Option<Control> {
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
pub(super) fn guard_error(title: &str, kind: guard::Kind, held: bool) -> String {
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
pub(super) fn guard_outcome(title: &str, kind: guard::Kind) -> String {
    format!("stopped: {} {}", window_named(title), guard_did(kind))
}

/// The frame's alpha `t` into control, breathing between faint and full.
pub(super) fn pulse(t: Duration) -> f32 {
    let phase = t.as_secs_f32() / PULSE.as_secs_f32();
    0.45 + 0.55 * (0.5 - 0.5 * (2.0 * PI * phase).cos())
}

/// The ghost's alpha `since` its last action.
pub(super) fn ghost_alpha(since: Duration) -> f32 {
    match since.checked_sub(GHOST_STAYS) {
        None => 1.0,
        Some(fading) => 1.0 - motion::progress(fading, GHOST_FADE),
    }
}

/// The four strips that frame a monitor, as x, y, width, height, `thick`
/// pixels deep: top and bottom the whole width, the sides between them.
pub(super) fn edges(monitor: [i32; 4], thick: i32) -> [[i32; 4]; 4] {
    let [l, t, r, b] = monitor;
    let (w, h) = (r - l, b - t);
    let thick = thick.min(w / 2).min(h / 2).max(1);
    let side = (h - 2 * thick).max(0);
    [
        [l, t, w, thick],
        [l, b - thick, w, thick],
        [l, t + thick, thick, side],
        [r - thick, t + thick, thick, side],
    ]
}

/// The plate's place: the top right corner of the work area, `margin`
/// in from it.
pub(super) fn box_place(work: [i32; 4], size: (i32, i32), margin: i32) -> (i32, i32) {
    let [l, t, r, _] = work;
    ((r - size.0 - margin).max(l), t + margin)
}

/// The plate's height in DIPs for a reason `reason_h` tall and `rows`
/// lines of history, with the bypass band when it is on, and a shell
/// section `shells` tall under it all.
pub(super) fn box_height(reason_h: f32, rows: usize, bypass: bool, shells: f32) -> f32 {
    (body_height(reason_h, rows, bypass) + shells + PAD).ceil()
}

/// Where the shell section begins: under the controls, NOW and history.
pub(super) fn body_height(reason_h: f32, rows: usize, bypass: bool) -> f32 {
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

/// What is shown under NOW.
#[derive(Clone, Default)]
struct Now {
    /// The session's id, which bypass is then on for.
    session: String,
    /// Its id and when it started, as the human reads them.
    ident: String,
    /// What it does instead, when it no longer works in the program.
    away: Option<String>,
    name: String,
    program: String,
    reason: String,
    what: String,
}

/// An allowed call, as the plate and the ghost show it.
pub(super) struct Allowed {
    pub session: String,
    /// The session's id and start, for the plate.
    pub ident: String,
    pub name: String,
    pub program: String,
    pub reason: String,
    pub what: String,
    /// Where the click goes, in physical screen pixels.
    pub point: Option<(i32, i32)>,
    /// The program's window, for the screen the plate goes on.
    pub hwnd: isize,
    /// Only bypass let it through: held for Esc even without a point.
    pub unasked: bool,
}

/// A click on its way, its answer held until the ghost arrives. A hold is
/// one without a point, the ghost waiting where it is.
struct Glide {
    from: (f32, f32),
    to: (f32, f32),
    began: Instant,
    takes: Duration,
    held: bool,
    unasked: bool,
    /// Its number, so what the guard saw is matched to it.
    serial: u64,
    reply: Reply,
    what: String,
}

/// A click waiting for the one before it to land.
struct Queued {
    to: Option<(i32, i32)>,
    hwnd: isize,
    unasked: bool,
    reply: Reply,
    name: String,
    what: String,
}

thread_local! {
    /// How long a glide takes, from the plate's slider.
    static SPEED: Cell<Duration> = const { Cell::new(GLIDE) };
    /// Bypass is on. Here and not on the overlay, since deciding a call
    /// reads it too, and never saved.
    static BYPASS: Cell<bool> = const { Cell::new(false) };
}

/// Whether bypass is on.
pub(super) fn bypass() -> bool {
    BYPASS.get()
}

/// The glide speed from the state file, at the app's start.
pub(super) fn set_speed_saved(ms: Option<u32>) {
    SPEED.set(glide_from_saved(ms));
}

/// The glide speed for the state file.
pub(super) fn speed_saved() -> Option<u32> {
    glide_to_saved(SPEED.get())
}

/// A window drawn with its own alpha through a DIB, the way layered windows
/// take per pixel alpha.
struct Layer {
    hwnd: HWND,
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    rt: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    pos: (i32, i32),
    size: (i32, i32),
    scale: f32,
    shown: bool,
}

impl Layer {
    fn new(gpu: &render::Gpu, pos: (i32, i32), size: (i32, i32), scale: f32) -> Result<Layer> {
        unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW
                    | WS_EX_TOPMOST
                    | WS_EX_NOACTIVATE
                    | WS_EX_TRANSPARENT
                    | WS_EX_LAYERED,
                CLASS,
                w!(""),
                WS_POPUP,
                pos.0,
                pos.1,
                size.0.max(1),
                size.1.max(1),
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                None,
            )?;
            // A debug run can show them to a screen copy, to check them.
            if std::env::var_os("HORADRIC_DEBUG_OVERLAY").is_none() {
                let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
            }
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 0.0,
                dpiY: 0.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = gpu.d2d.CreateDCRenderTarget(&props)?;
            // ClearType needs to know what is behind the text, and here
            // nothing is.
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let brush = rt.CreateSolidColorBrush(&color(theme::text()), None)?;
            let dc = CreateCompatibleDC(None);
            let mut layer = Layer {
                hwnd,
                dc,
                bitmap: HBITMAP::default(),
                old: HGDIOBJ::default(),
                rt,
                brush,
                pos,
                size: (0, 0),
                scale,
                shown: false,
            };
            layer.resize(size)?;
            Ok(layer)
        }
    }

    fn resize(&mut self, size: (i32, i32)) -> Result<()> {
        let size = (size.0.max(1), size.1.max(1));
        if size == self.size {
            return Ok(());
        }
        unsafe {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: size.0,
                    // Top down, as Direct2D draws.
                    biHeight: -size.1,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap =
                CreateDIBSection(Some(self.dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let was = SelectObject(self.dc, bitmap.into());
            if self.bitmap.is_invalid() {
                self.old = was;
            } else {
                let _ = DeleteObject(self.bitmap.into());
            }
            self.bitmap = bitmap;
        }
        self.size = size;
        Ok(())
    }

    /// Draws the whole window afresh, in DIPs.
    fn paint(&self, draw: impl FnOnce(&Pen)) -> Result<()> {
        unsafe {
            let all = RECT {
                left: 0,
                top: 0,
                right: self.size.0,
                bottom: self.size.1,
            };
            self.rt.BindDC(self.dc, &all)?;
            let dpi = 96.0 * self.scale;
            self.rt.SetDpi(dpi, dpi);
            self.rt.BeginDraw();
            self.rt.Clear(Some(&D2D1_COLOR_F::default()));
            draw(&Pen {
                rt: &self.rt,
                brush: &self.brush,
            });
            self.rt.EndDraw(None, None)
        }
    }

    /// Puts what was painted on screen at `pos`, `alpha` over all of it.
    fn present(&mut self, pos: (i32, i32), alpha: f32) {
        self.pos = pos;
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let at = POINT { x: pos.0, y: pos.1 };
        let size = SIZE {
            cx: self.size.0,
            cy: self.size.1,
        };
        let from = POINT { x: 0, y: 0 };
        unsafe {
            let _ = UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&at),
                Some(&size),
                Some(self.dc),
                Some(&from),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            if !self.shown {
                self.shown = true;
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }

    /// Lets the mouse reach the window, or go through it. Toggled as the
    /// cursor comes over a control, since a click on another process's
    /// window behind does not come through `HTTRANSPARENT`.
    ///
    /// Windows sends a click to the window it found under the pointer at
    /// the pointer's last move, so a style changed under a pointer at rest
    /// would not count until it moved: a click without a move went through
    /// the plate. `SWP_FRAMECHANGED` makes Windows take the new style in
    /// at once, and putting the pointer back where it is makes it look
    /// again what is under it, without the pointer moving.
    fn set_clickable(&self, on: bool, at: POINT) {
        unsafe {
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            let through = WS_EX_TRANSPARENT.0 as isize;
            let want = if on { ex & !through } else { ex | through };
            if want == ex {
                return;
            }
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, want);
            let _ = SetWindowPos(
                self.hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            // Only where the human left it: a pointer that moved since has
            // been looked at by Windows already, and is not pulled back.
            let mut now = POINT::default();
            if GetCursorPos(&mut now).is_ok() && now.x == at.x && now.y == at.y {
                let _ = SetCursorPos(at.x, at.y);
            }
        }
    }

    fn hide(&mut self) {
        if self.shown {
            self.shown = false;
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }
}

impl Drop for Layer {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Drawing on a layer.
struct Pen<'a> {
    rt: &'a ID2D1RenderTarget,
    brush: &'a ID2D1SolidColorBrush,
}

fn d2rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}

impl Pen<'_> {
    unsafe fn fill_rounded(&self, r: D2D_RECT_F, radius: f32, c: Color) {
        self.brush.SetColor(&color(c));
        self.rt.FillRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: r,
                radiusX: radius,
                radiusY: radius,
            },
            self.brush,
        );
    }

    unsafe fn stroke_rounded(&self, r: D2D_RECT_F, radius: f32, c: Color, width: f32) {
        self.brush.SetColor(&color(c));
        self.rt.DrawRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: r,
                radiusX: radius,
                radiusY: radius,
            },
            self.brush,
            width,
            None,
        );
    }

    unsafe fn circle(&self, at: (f32, f32), r: f32, c: Color, stroke: Option<f32>) {
        self.brush.SetColor(&color(c));
        let e = D2D1_ELLIPSE {
            point: Vector2 { X: at.0, Y: at.1 },
            radiusX: r,
            radiusY: r,
        };
        match stroke {
            Some(w) => self.rt.DrawEllipse(&e, self.brush, w, None),
            None => self.rt.FillEllipse(&e, self.brush),
        }
    }

    unsafe fn text(&self, fmt: &IDWriteTextFormat, c: Color, s: &str, r: D2D_RECT_F) {
        if r.right <= r.left {
            return;
        }
        let wide: Vec<u16> = s.encode_utf16().collect();
        self.brush.SetColor(&color(c));
        self.rt.DrawText(
            &wide,
            fmt,
            &r,
            self.brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }

    /// A gradient from `stops[0]` at `from` to the last at `to`.
    unsafe fn fill_gradient(
        &self,
        r: D2D_RECT_F,
        from: (f32, f32),
        to: (f32, f32),
        stops: &[(f32, Color)],
    ) {
        let stops: Vec<D2D1_GRADIENT_STOP> = stops
            .iter()
            .map(|&(position, c)| D2D1_GRADIENT_STOP {
                position,
                color: color(c),
            })
            .collect();
        let Ok(collection) =
            self.rt
                .CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)
        else {
            return;
        };
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
            startPoint: Vector2 {
                X: from.0,
                Y: from.1,
            },
            endPoint: Vector2 { X: to.0, Y: to.1 },
        };
        if let Ok(brush) = self.rt.CreateLinearGradientBrush(&props, None, &collection) {
            self.rt.FillRectangle(&r, &brush);
        }
    }
}

/// Everything shown while control runs.
struct Overlay {
    shared: Rc<Shared>,
    /// A window never shown, where the frames and the Esc come.
    clock: HWND,
    framing: bool,
    edges: Vec<Layer>,
    plate: Option<Layer>,
    /// The plate's work area, and its monitor's scale.
    work: [i32; 4],
    scale: f32,
    ghost: Option<Layer>,
    /// The tip of the arrow, in physical pixels.
    ghost_at: Option<(f32, f32)>,
    ghost_name: String,
    /// The last time the ghost acted, for when it fades.
    ghost_acted: Instant,
    /// Stopped with Esc: it shows red and stays while control runs.
    ghost_stopped: bool,
    ring: Option<(Instant, Color)>,
    glide: Option<Glide>,
    glides: u64,
    queue: VecDeque<Queued>,
    stop_key: bool,
    /// Watching for windows while a glide runs.
    watch: Option<guard::Watch>,
    /// The slider's knob is held.
    dragging: bool,
    hover: Option<Control>,
    /// The frame was painted for bypass.
    edges_bypass: bool,
    /// What bypass let through and has not been told done yet.
    unasked: VecDeque<String>,
    /// The sessions bypass is on for, when it was turned on, and when they
    /// were last looked at.
    holders: HashMap<String, Holder>,
    bypass_at: Instant,
    turns_checked: Instant,
    /// When the plate last looked at what its session does now.
    now_checked: Instant,
    until: Option<Instant>,
    /// When control began, for the pulse, and how far it has faded in.
    began: Instant,
    shown: f32,
    ticked: Instant,
    now: Now,
    history: VecDeque<Entry>,
    /// Each session's shell commands since control began, newest first.
    shells: HashMap<String, VecDeque<Shell>>,
    /// The shell section is open: the human folds it with its header.
    shells_open: bool,
    /// Where the shell section begins on the plate, in DIPs, for the mouse.
    shells_top: Option<f32>,
    /// The commands' monospaced face, made on the first paint.
    mono: Option<IDWriteTextFormat>,
    /// The plate's content changed since it was painted.
    stale: bool,
}

thread_local! {
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

fn registered() -> bool {
    thread_local! {
        static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if DONE.get() {
        return true;
    }
    let ok = unsafe {
        GetModuleHandleW(None).is_ok_and(|module| {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: module.into(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassW(&wc) != 0
        })
    };
    DONE.set(ok);
    ok
}

/// Runs `f` on the overlay, made the first time. Nothing when its windows
/// cannot be made, since control goes on without its show.
fn with(shared: &Rc<Shared>, f: impl FnOnce(&mut Overlay)) {
    OVERLAY.with(|o| {
        let Ok(mut slot) = o.try_borrow_mut() else {
            return;
        };
        if slot.is_none() && registered() {
            let clock = unsafe {
                CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    CLASS,
                    w!(""),
                    WS_POPUP,
                    0,
                    0,
                    0,
                    0,
                    None,
                    None,
                    GetModuleHandleW(None).ok().map(Into::into),
                    None,
                )
            };
            match clock {
                Ok(clock) => {
                    let now = Instant::now();
                    *slot = Some(Overlay {
                        shared: Rc::clone(shared),
                        clock,
                        framing: false,
                        edges: Vec::new(),
                        plate: None,
                        work: [0, 0, 1280, 720],
                        scale: 1.0,
                        ghost: None,
                        ghost_at: None,
                        ghost_name: String::new(),
                        ghost_acted: now,
                        ghost_stopped: false,
                        ring: None,
                        glide: None,
                        glides: 0,
                        queue: VecDeque::new(),
                        stop_key: false,
                        watch: None,
                        dragging: false,
                        hover: None,
                        edges_bypass: false,
                        unasked: VecDeque::new(),
                        holders: HashMap::new(),
                        bypass_at: now,
                        turns_checked: now,
                        now_checked: now,
                        until: None,
                        began: now,
                        shown: 0.0,
                        ticked: now,
                        now: Now::default(),
                        history: VecDeque::new(),
                        shells: HashMap::new(),
                        shells_open: true,
                        shells_top: None,
                        mono: None,
                        stale: true,
                    })
                }
                Err(e) => eprintln!("horadric: cannot show desktop control: {e}"),
            }
        }
        if let Some(o) = slot.as_mut() {
            f(o);
        }
    });
}

/// An allowed call. With a point the ghost glides there and the call is
/// answered when it arrives, or refused if the human presses Esc; without
/// one it is answered at once, unless only bypass let it through, when it
/// is held as long as a glide. True when this call started control, so
/// the app can say so once.
pub(super) fn allowed(shared: &Rc<Shared>, call: Allowed, reply: Reply) -> bool {
    // Without an overlay the app's own word is all the human gets.
    let mut started = true;
    let mut reply = Some(reply);
    let unasked = call.unasked;
    with(shared, |o| {
        let now = Instant::now();
        started = !o.active(now);
        if started {
            o.began = now;
        }
        SHELLS_LIVE.store(true, Ordering::Release);
        o.until = extend(o.until, now, true);
        o.place_plate(call.hwnd, call.point);
        if bypass() {
            o.holders.entry(call.session.clone()).or_default().called();
        }
        o.now = Now {
            session: call.session,
            ident: call.ident,
            away: None,
            name: call.name.clone(),
            program: call.program,
            reason: call.reason,
            what: call.what.clone(),
        };
        o.stale = true;
        // Taken only when held: any other is answered below.
        let held = call.point.is_some() || call.unasked;
        if let Some(r) = reply.take_if(|_| held) {
            o.queue.push_back(Queued {
                to: call.point,
                hwnd: call.hwnd,
                unasked: call.unasked,
                reply: r,
                name: call.name,
                what: call.what,
            });
            if o.glide.is_none() {
                o.next_glide(now);
            }
        }
        o.run();
    });
    // Without a point, or without an overlay to show the way. What only
    // bypass allowed goes nowhere the human could not stop it.
    if let Some(r) = reply {
        if unasked {
            r.send(
                json!({ "error": "Horadric could not show this action to the human, so \
                bypass did not let it through; ask again" }),
            );
        } else {
            r.send(json!({ "allowed": true }));
        }
    }
    started
}

/// A finished action, from `desktop.done`.
pub(super) fn done(shared: &Rc<Shared>, what: &str, ok: bool, outcome: &str) {
    with(shared, |o| {
        let unasked = take_unasked(&mut o.unasked, what);
        o.note(Entry {
            at: now_clock(),
            mark: if ok { Mark::Ok } else { Mark::Failed },
            what: what.to_string(),
            outcome: outcome.to_string(),
            unasked,
        });
    });
}

/// A call the human refused.
pub(super) fn refused(shared: &Rc<Shared>, what: &str, why: &str) {
    record(
        shared,
        Entry {
            at: now_clock(),
            mark: Mark::Refused,
            what: what.to_string(),
            outcome: why.to_string(),
            unasked: false,
        },
    );
}

fn record(shared: &Rc<Shared>, e: Entry) {
    with(shared, |o| o.note(e));
}

fn now_clock() -> String {
    let t = unsafe { GetLocalTime() };
    clock(t.wHour, t.wMinute, t.wSecond)
}

fn monitor_scale(monitor: HMONITOR) -> f32 {
    let (mut dx, mut dy) = (96u32, 96u32);
    unsafe {
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    }
    dx.max(96) as f32 / 96.0
}

fn monitor_rects(monitor: HMONITOR) -> Option<([i32; 4], [i32; 4])> {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetMonitorInfoW(monitor, &mut info) }
        .as_bool()
        .then(|| {
            let (m, w) = (info.rcMonitor, info.rcWork);
            (
                [m.left, m.top, m.right, m.bottom],
                [w.left, w.top, w.right, w.bottom],
            )
        })
}

fn monitors() -> Vec<HMONITOR> {
    unsafe extern "system" fn each(
        m: HMONITOR,
        _: HDC,
        _: *mut RECT,
        data: LPARAM,
    ) -> windows::core::BOOL {
        let list = &mut *(data.0 as *mut Vec<HMONITOR>);
        list.push(m);
        true.into()
    }
    let mut list: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(each),
            LPARAM(&mut list as *mut Vec<HMONITOR> as isize),
        );
    }
    list
}

impl Overlay {
    /// Bypass keeps it all up, so it is never on unseen.
    fn active(&self, now: Instant) -> bool {
        self.until.is_some_and(|u| u > now) || bypass()
    }

    fn note(&mut self, e: Entry) {
        let now = Instant::now();
        self.until = extend(self.until, now, false);
        remember(&mut self.history, e);
        self.stale = true;
        self.run();
    }

    /// Starts the frames, if they are not running.
    fn run(&mut self) {
        if !self.framing {
            self.framing = true;
            self.ticked = Instant::now();
            crate::vsync::start(self.clock, FRAME);
        }
    }

    fn place_plate(&mut self, hwnd: isize, point: Option<(i32, i32)>) {
        let monitor = unsafe {
            if hwnd != 0 {
                MonitorFromWindow(HWND(hwnd as *mut _), MONITOR_DEFAULTTONEAREST)
            } else {
                let (x, y) = point.unwrap_or_default();
                MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST)
            }
        };
        if let Some((_, work)) = monitor_rects(monitor) {
            let scale = monitor_scale(monitor);
            if (scale - self.scale).abs() > f32::EPSILON {
                self.plate = None;
            }
            self.work = work;
            self.scale = scale;
        }
    }

    /// Makes the frame around every monitor, once a run of control begins,
    /// so a screen added since is framed too.
    fn frame_screens(&mut self) {
        if !self.edges.is_empty() {
            return;
        }
        for monitor in monitors() {
            let Some((rect, _)) = monitor_rects(monitor) else {
                continue;
            };
            let scale = monitor_scale(monitor);
            let thick = (EDGE * scale).round() as i32;
            for (side, [x, y, w, h]) in edges(rect, thick).into_iter().enumerate() {
                if w <= 0 || h <= 0 {
                    continue;
                }
                let Ok(mut layer) = Layer::new(&self.shared.gpu, (x, y), (w, h), scale) else {
                    continue;
                };
                let (dw, dh) = (w as f32 / scale, h as f32 / scale);
                // From the screen's edge inward: bright at the edge, gone
                // by the glow's depth.
                let (from, to) = match side {
                    0 => ((0.0, 0.0), (0.0, dh)),
                    1 => ((0.0, dh), (0.0, 0.0)),
                    2 => ((0.0, 0.0), (dw, 0.0)),
                    _ => ((dw, 0.0), (0.0, 0.0)),
                };
                let glow = if bypass() {
                    theme::waiting()
                } else {
                    theme::working()
                };
                let painted = layer.paint(|p| unsafe {
                    p.fill_gradient(
                        d2rect(0.0, 0.0, dw, dh),
                        from,
                        to,
                        &[
                            (0.0, glow.with_alpha(0.95)),
                            (0.35, glow.with_alpha(0.5)),
                            (1.0, glow.with_alpha(0.0)),
                        ],
                    );
                });
                if painted.is_ok() {
                    layer.present((x, y), 0.0);
                    self.edges.push(layer);
                }
            }
        }
        self.edges_bypass = bypass();
    }

    /// Starts the next queued call's glide, unless one is on its way:
    /// starting another would drop that one's answer and its guard.
    fn next_glide(&mut self, now: Instant) {
        if self.glide.is_some() {
            return;
        }
        while let Some(q) = self.queue.pop_front() {
            // Its agent gave up waiting: acting now would act twice.
            if !q.reply.wanted() {
                continue;
            }
            // Taken before the glide starts, so the human can stop it from
            // its first frame.
            self.listen_for_stop(true);
            if !may_glide(q.unasked, self.stop_key) {
                q.reply.send(json!({ "error": NO_ESC }));
                remember(
                    &mut self.history,
                    Entry {
                        at: now_clock(),
                        mark: Mark::Stopped,
                        what: q.what,
                        outcome: "not made: Esc could not be taken".into(),
                        unasked: true,
                    },
                );
                self.stale = true;
                continue;
            }
            let from = self.ghost_at.unwrap_or_else(|| {
                let mut p = POINT::default();
                let _ = unsafe { GetCursorPos(&mut p) };
                (p.x as f32, p.y as f32)
            });
            self.ghost_at = Some(from);
            self.ghost_name = q.name;
            self.ghost_stopped = false;
            self.ghost_acted = now;
            self.glides += 1;
            if self.watch.is_none() {
                self.watch = Some(guard::watch(self.clock, GUARD_MSG));
            }
            guard::aim(self.glides, q.hwnd, q.to);
            self.glide = Some(Glide {
                from,
                to: q.to.map_or(from, |(x, y)| (x as f32, y as f32)),
                began: now,
                takes: SPEED.get(),
                held: q.to.is_none(),
                unasked: q.unasked,
                serial: self.glides,
                reply: q.reply,
                what: q.what,
            });
            self.paint_ghost(now);
            return;
        }
        self.watch = None;
        self.keep_esc();
    }

    /// Holds Esc while it has something to stop, and gives it back to the
    /// human's programs the moment it has not.
    fn keep_esc(&mut self) {
        self.listen_for_stop(wants_esc(self.glide.is_some(), bypass()));
    }

    /// Takes or gives back Esc. A failed take leaves it given back, and the
    /// next take tries again.
    fn listen_for_stop(&mut self, on: bool) {
        if on == self.stop_key {
            return;
        }
        unsafe {
            if on {
                self.stop_key =
                    RegisterHotKey(Some(self.clock), STOP_KEY, MOD_NOREPEAT, VK_ESCAPE.0 as u32)
                        .is_ok();
            } else {
                let _ = UnregisterHotKey(Some(self.clock), STOP_KEY);
                self.stop_key = false;
            }
        }
    }

    /// The glide on its way is not made: `error` to its agent, `outcome`
    /// in the history.
    fn halt(&mut self, error: &str, outcome: String) {
        let now = Instant::now();
        let Some(g) = self.glide.take() else {
            return;
        };
        g.reply.send(json!({ "error": error }));
        self.ghost_stopped = true;
        self.ghost_acted = now;
        self.ring = Some((now, theme::error()));
        self.until = extend(self.until, now, false);
        remember(
            &mut self.history,
            Entry {
                at: now_clock(),
                mark: Mark::Stopped,
                what: g.what,
                outcome,
                unasked: g.unasked,
            },
        );
        self.stale = true;
    }

    /// Esc: the click on its way is not made. With bypass on it ends
    /// bypass too, the human's word that the trust is over, even with
    /// nothing on its way.
    fn stop(&mut self) {
        let (halts, ends) = esc_does(self.glide.is_some(), bypass());
        if halts {
            self.halt(
                "the human stopped this action",
                "stopped by the human".into(),
            );
        }
        if ends {
            // It starts the next glide itself.
            self.bypass_off(BypassEnd::Esc);
        } else {
            // The rest waiting were asked before the stop and still go,
            // each with its own glide to stop.
            self.next_glide(Instant::now());
        }
        self.run();
    }

    /// The guard saw a window open or the front move while the glide it
    /// tripped on was on its way. Bypass stays: nobody said no.
    fn tripped(&mut self) {
        let Some(t) = guard::take() else {
            return;
        };
        let Some(g) = &self.glide else {
            return;
        };
        if !trip_stops(t.glide, Some(g.serial)) {
            return;
        }
        let error = guard_error(&t.title, t.kind, g.held);
        self.halt(&error, guard_outcome(&t.title, t.kind));
        self.next_glide(Instant::now());
        self.run();
    }

    fn bypass_on(&mut self) {
        BYPASS.set(true);
        BYPASS_LIVE.store(true, Ordering::Release);
        take_turns_ended();
        let now = Instant::now();
        self.bypass_at = now;
        self.turns_checked = now;
        self.holders.clear();
        // It is on for the session the plate shows. One that only rests
        // has BYPASS_IDLE to start working.
        if !self.now.session.is_empty() {
            self.holders
                .insert(self.now.session.clone(), Holder::default());
        }
        // Its banner says Esc ends it, so Esc is held from now.
        self.keep_esc();
        self.note_bypass(None);
        self.stale = true;
        self.run();
    }

    /// Writes bypass turned on or ended into the history and into
    /// `desktop.log`, which is where the human reads afterwards what went
    /// unasked and between which lines.
    fn note_bypass(&mut self, end: Option<BypassEnd>) {
        let (what, outcome) = bypass_words(end);
        remember(
            &mut self.history,
            Entry {
                at: now_clock(),
                mark: Mark::Ok,
                what: what.into(),
                outcome: outcome.into(),
                unasked: false,
            },
        );
        let source = match end {
            None => Source::BypassOn,
            Some(_) => Source::BypassOff(outcome),
        };
        super::desktop::log(&self.now.name, source, &self.now.program, "");
    }

    /// Ends bypass, and with it every call it let through that has not
    /// gone yet.
    fn bypass_off(&mut self, how: BypassEnd) {
        BYPASS.set(false);
        BYPASS_LIVE.store(false, Ordering::Release);
        take_turns_ended();
        self.holders.clear();
        if self.glide.as_ref().is_some_and(|g| g.unasked) {
            self.halt(BYPASS_ENDED, "stopped: bypass turned off".into());
        }
        let (gone, kept): (VecDeque<Queued>, VecDeque<Queued>) =
            self.queue.drain(..).partition(|q| q.unasked);
        self.queue = kept;
        for q in gone {
            q.reply.send(json!({ "error": BYPASS_ENDED }));
            remember(
                &mut self.history,
                Entry {
                    at: now_clock(),
                    mark: Mark::Stopped,
                    what: q.what,
                    outcome: "not made: bypass turned off".into(),
                    unasked: true,
                },
            );
        }
        // Newest on top, above what it stopped.
        self.note_bypass(Some(how));
        // Control runs on a while, so the plate does not vanish the
        // moment bypass ends.
        self.until = extend(self.until, Instant::now(), true);
        self.stale = true;
        // Starts what the human asked for, or gives Esc back when nothing
        // is left to stop.
        self.next_glide(Instant::now());
        self.run();
    }

    /// Bypass on: looks at the sessions it is on for, and turns it off once
    /// their work is done and nothing is on its way.
    fn watch_turns(&mut self, now: Instant) {
        if !bypass() || now.duration_since(self.turns_checked) < TURNS_EVERY {
            return;
        }
        self.turns_checked = now;
        for session in take_turns_ended() {
            if let Some(h) = self.holders.get_mut(&session) {
                h.ended = true;
            }
        }
        let waited = now.duration_since(self.bypass_at) >= BYPASS_IDLE;
        if let Ok(reg) = self.shared.registry.lock() {
            for (id, h) in &mut self.holders {
                h.saw(turn_of(reg.get(id).map(|s| &s.phase)), waited);
            }
        }
        let in_flight = self.glide.is_some() || !self.queue.is_empty();
        if !bypass_over(&self.holders, in_flight) {
            return;
        }
        self.bypass_off(BypassEnd::Done);
    }

    /// Looks at what the plate's session does now, so NOW does not stand
    /// on an action long done while it works elsewhere.
    fn watch_now(&mut self, now: Instant) {
        if self.now.session.is_empty()
            || self.glide.is_some()
            || now.duration_since(self.now_checked) < TURNS_EVERY
        {
            return;
        }
        self.now_checked = now;
        let Ok(reg) = self.shared.registry.lock() else {
            return;
        };
        let s = reg.get(&self.now.session);
        let away = away_of(
            s.and_then(|s| s.tool.as_deref()),
            turn_of(s.map(|s| &s.phase)),
            &self.now.program,
        );
        drop(reg);
        if away != self.now.away {
            self.now.away = away;
            self.stale = true;
        }
    }

    /// Takes in the shell commands the hook thread heard, each into its
    /// session's list and into `desktop.log`, the audit trail of what ran
    /// beside the control.
    fn hear_shells(&mut self) {
        let notes = take_shell_notes();
        if notes.is_empty() {
            return;
        }
        let mut lines = String::new();
        for note in notes {
            let session = note.session().to_string();
            let Some(shell) = hear_shell(&mut self.shells, note) else {
                continue;
            };
            if session == self.now.session {
                self.stale = true;
            }
            let name = self
                .shared
                .registry
                .lock()
                .ok()
                .and_then(|r| r.get(&session).map(|s| s.name.clone()))
                .filter(|n| !n.trim().is_empty())
                .unwrap_or(session);
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            lines.push_str(&shell_log_line(
                &horadric_core::tasks::utc(secs),
                &name,
                &shell,
            ));
        }
        let file = horadric_hooks::tasks::desktop_log_file();
        let Some(file) = file.filter(|_| !lines.is_empty()) else {
            return;
        };
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)
            .and_then(|mut f| std::io::Write::write_all(&mut f, lines.as_bytes()));
    }

    fn set_speed(&mut self, f: f32) {
        let d = glide_of_slider(f);
        if d != SPEED.get() {
            SPEED.set(d);
            self.stale = true;
        }
    }

    /// Whether a point on screen, in physical pixels, is over one of the
    /// plate's controls, or the knob is held.
    fn over_control(&self, hwnd: HWND, at: (i32, i32)) -> bool {
        let Some(p) = self.plate.as_ref().filter(|p| p.hwnd == hwnd && p.shown) else {
            return false;
        };
        let x = (at.0 - p.pos.0) as f32 / p.scale;
        let y = (at.1 - p.pos.1) as f32 / p.scale;
        self.dragging || control_at(x, y, self.shells_top).is_some()
    }

    /// The mouse on the plate. True when it turned bypass on, for the app
    /// to answer the calls waiting for a question.
    fn mouse(&mut self, hwnd: HWND, msg: u32, lparam: LPARAM) -> bool {
        let Some(p) = self.plate.as_ref().filter(|p| p.hwnd == hwnd) else {
            return false;
        };
        let scale = p.scale;
        let x = (lparam.0 & 0xFFFF) as i16 as f32 / scale;
        let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / scale;
        match msg {
            WM_LBUTTONDOWN => match control_at(x, y, self.shells_top) {
                Some(Control::Speed) => {
                    self.dragging = true;
                    unsafe {
                        SetCapture(hwnd);
                    }
                    self.set_speed(slider_at(x));
                }
                Some(Control::Bypass) if bypass() => self.bypass_off(BypassEnd::Switched),
                Some(Control::Bypass) => {
                    self.bypass_on();
                    return true;
                }
                Some(Control::Shells) => {
                    self.shells_open = !self.shells_open;
                    self.stale = true;
                }
                None => {}
            },
            WM_MOUSEMOVE if self.dragging => self.set_speed(slider_at(x)),
            WM_LBUTTONUP if self.dragging => {
                self.dragging = false;
                unsafe {
                    let _ = ReleaseCapture();
                }
            }
            WM_CAPTURECHANGED => self.dragging = false,
            _ => {}
        }
        false
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let dt = now
            .duration_since(self.ticked)
            .min(Duration::from_millis(50));
        self.ticked = now;
        let active = self.active(now) || self.glide.is_some();
        if self.edges_bypass != bypass() {
            self.edges.clear();
        }
        if active {
            self.frame_screens();
            self.shown = (self.shown + dt.as_secs_f32() / FADE_IN.as_secs_f32()).min(1.0);
        } else {
            self.shown = (self.shown - dt.as_secs_f32() / FADE_OUT.as_secs_f32()).max(0.0);
        }

        // A trip whose message came while the overlay was busy, or after
        // this frame, is taken before the glide is answered: the window
        // it saw opened before the click.
        self.tripped();
        self.watch_turns(now);
        self.watch_now(now);
        SHELLS_LIVE.store(active, Ordering::Release);
        self.hear_shells();
        if let Some(g) = &self.glide {
            let elapsed = now.duration_since(g.began);
            self.ghost_at = Some(glide_at(g.from, g.to, elapsed, g.takes));
            self.stale = true;
            if elapsed >= g.takes {
                if let Some(g) = self.glide.take() {
                    g.reply.send(json!({ "allowed": true }));
                    if g.unasked {
                        note_unasked(&mut self.unasked, &g.what);
                    }
                }
                self.ghost_acted = now;
                self.ring = Some((now, theme::working()));
                self.next_glide(now);
            }
        }
        if self.ring.is_some_and(|(t, _)| now.duration_since(t) > RING) {
            self.ring = None;
        }
        self.paint_ghost(now);

        let alpha = self.shown * pulse(now.duration_since(self.began));
        for e in &mut self.edges {
            e.present(e.pos, alpha);
        }
        self.paint_plate(now);

        let ghost_seen = self.ghost.as_ref().is_some_and(|g| g.shown);
        if self.shown <= 0.0 && !active && !ghost_seen && self.ring.is_none() {
            self.rest();
        }
    }

    /// Nothing moves: the windows hide and the frames stop.
    fn rest(&mut self) {
        // The next run of control lists only what it saw begin.
        SHELLS_LIVE.store(false, Ordering::Release);
        take_shell_notes();
        self.shells.clear();
        self.edges.clear();
        if let Some(p) = self.plate.as_mut() {
            p.hide();
        }
        self.ghost_stopped = false;
        self.framing = false;
        crate::vsync::stop(self.clock, FRAME);
    }

    fn paint_ghost(&mut self, now: Instant) {
        let Some(at) = self.ghost_at else {
            return;
        };
        // A stopped ghost stays where it was stopped while control runs.
        let alpha = if self.glide.is_some() || (self.ghost_stopped && self.active(now)) {
            1.0
        } else if self.ghost_stopped {
            self.shown
        } else {
            ghost_alpha(now.duration_since(self.ghost_acted))
        };
        if alpha <= 0.0 {
            if let Some(g) = self.ghost.as_mut() {
                g.hide();
            }
            // Its next glide starts from the real cursor again.
            self.ghost_at = None;
            return;
        }
        let monitor = unsafe {
            MonitorFromPoint(
                POINT {
                    x: at.0 as i32,
                    y: at.1 as i32,
                },
                MONITOR_DEFAULTTONEAREST,
            )
        };
        let scale = monitor_scale(monitor);
        let size = (
            (GHOST_SIZE.0 * scale).round() as i32,
            (GHOST_SIZE.1 * scale).round() as i32,
        );
        if self.ghost.as_ref().is_some_and(|g| g.scale != scale) {
            self.ghost = None;
        }
        if self.ghost.is_none() {
            match Layer::new(&self.shared.gpu, (at.0 as i32, at.1 as i32), size, scale) {
                Ok(l) => self.ghost = Some(l),
                Err(_) => return,
            }
        }
        let gpu = &self.shared.gpu;
        let ring = self
            .ring
            .map(|(t, c)| (motion::progress(now.duration_since(t), RING), c));
        let tint = if self.ghost_stopped {
            theme::error()
        } else if self.glide.as_ref().is_some_and(|g| g.unasked) {
            theme::waiting()
        } else {
            theme::working()
        };
        let name = self.ghost_name.as_str();
        let Some(layer) = self.ghost.as_mut() else {
            return;
        };
        let tip = GHOST_TIP;
        let _ = layer.paint(|p| unsafe {
            p.circle((tip, tip), 13.0, tint.with_alpha(0.22), None);
            if let Some((t, c)) = ring {
                let r = 6.0 + 26.0 * motion::ease_out(t);
                p.circle((tip, tip), r, c.with_alpha(1.0 - t), Some(2.5));
            }
            arrow(gpu, p, tip, tint);
            tag(gpu, p, (tip + 14.0, tip + 20.0), name, tint);
        });
        let off = (tip * scale).round() as i32;
        layer.present(
            (at.0.round() as i32 - off, at.1.round() as i32 - off),
            alpha,
        );
    }

    fn paint_plate(&mut self, now: Instant) {
        if self.shown <= 0.0 {
            if let Some(p) = self.plate.as_mut() {
                p.hide();
            }
            return;
        }
        let gliding = self
            .glide
            .as_ref()
            .map(|g| left_of_glide(now.duration_since(g.began), g.takes));
        let bypass = bypass();
        let gpu = &self.shared.gpu;
        let content_w = BOX_W - 2.0 * PAD;
        // Away, what it does now takes the place of why it acted then.
        let said = self.now.away.as_deref().unwrap_or(&self.now.reason).trim();
        let reason = (!said.is_empty())
            .then(|| render::wrapped(gpu, &gpu.small, said, content_w).ok())
            .flatten();
        let reason_h = reason
            .as_ref()
            .map_or(0.0, |l| render::text_size(l).1.ceil());
        let body = body_height(reason_h, self.history.len(), bypass);
        if self.mono.is_none() {
            self.mono = unsafe { mono_format(gpu) };
        }
        // The commands of the session the plate shows, as many as the
        // screen has room for under the rest.
        let room =
            (self.work[3] - self.work[1]) as f32 / self.scale - 2.0 * BOX_MARGIN - body - PAD;
        let mut rows: Vec<ShellRow> = Vec::new();
        let list = self.shells.get(&self.now.session);
        if let (Some(list), Some(mono), true) = (list, self.mono.as_ref(), self.shells_open) {
            for shell in list {
                let shown = command_shown(&shell.command);
                let Ok(layout) = render::wrapped(gpu, mono, shown, COMMAND_W) else {
                    continue;
                };
                let (lines, line_h) = line_metrics(&layout);
                let h = command_height(lines, line_h, shell.state);
                // Past its lines the layout trims the last one with an
                // ellipsis.
                unsafe {
                    let _ = layout.SetMaxHeight(h + 0.5);
                }
                rows.push(ShellRow {
                    shell,
                    command: layout,
                    h,
                    hidden: hidden_lines(lines, shell.state),
                });
            }
            let heights: Vec<f32> = rows.iter().map(|r| r.h).collect();
            rows.truncate(shells_fit(&heights, room));
        }
        let heights: Vec<f32> = rows.iter().map(|r| r.h).collect();
        let shells_h = shells_height(self.shells_open, &heights);
        let h = box_height(reason_h, self.history.len(), bypass, shells_h);
        self.shells_top = Some(body);
        let count = list.map_or_else(|| shells_count(&VecDeque::new()), shells_count);
        let s = self.scale;
        let size = ((BOX_W * s).round() as i32, (h * s).round() as i32);
        let margin = (BOX_MARGIN * s).round() as i32;
        let pos = box_place(self.work, size, margin);
        if self.plate.is_none() {
            match Layer::new(gpu, pos, size, s) {
                Ok(l) => self.plate = Some(l),
                Err(_) => return,
            }
            self.stale = true;
        }
        let Some(plate) = self.plate.as_mut() else {
            return;
        };
        if self.stale || plate.size != size {
            self.stale = false;
            if plate.resize(size).is_err() {
                return;
            }
            let now_line = &self.now;
            let history = &self.history;
            let metrics_radius = self.shared.metrics.window_radius;
            let (speed, hover, dragging) = (SPEED.get(), self.hover, self.dragging);
            let shells = ShellsScene {
                top: body,
                open: self.shells_open,
                count,
                rows,
                hover: hover == Some(Control::Shells),
            };
            let _ = plate.paint(|p| unsafe {
                draw_plate(
                    gpu,
                    p,
                    PlateScene {
                        h,
                        radius: metrics_radius,
                        now: now_line,
                        reason: reason.as_ref(),
                        reason_h,
                        gliding,
                        history,
                        speed,
                        bypass,
                        hover,
                        dragging,
                    },
                );
                draw_shells(gpu, p, &shells);
            });
        }
        plate.present(pos, self.shown);
        // The mouse reaches the plate only over a control, so the rest of
        // it does not stand between the human and their screen. Looked at
        // every frame, not only on a move, so a pointer that rests on a
        // control when the plate comes up, or that the plate grew under,
        // clicks it.
        let mut cursor = POINT::default();
        let hover = unsafe { GetCursorPos(&mut cursor) }.ok().and_then(|_| {
            let x = (cursor.x - pos.0) as f32 / s;
            let y = (cursor.y - pos.1) as f32 / s;
            control_at(x, y, self.shells_top)
        });
        plate.set_clickable(hover.is_some() || self.dragging, cursor);
        if hover != self.hover {
            self.hover = hover;
            self.stale = true;
        }
    }
}

/// The ghost's arrow, its tip at (`tip`, `tip`).
unsafe fn arrow(gpu: &render::Gpu, p: &Pen, tip: f32, tint: Color) {
    let Ok(path) = gpu.d2d.CreatePathGeometry() else {
        return;
    };
    let Ok(sink) = path.Open() else {
        return;
    };
    let k = 1.25;
    let pts = [
        (0.0, 17.0),
        (4.5, 13.0),
        (7.5, 20.0),
        (10.0, 19.0),
        (7.0, 12.5),
        (12.5, 12.5),
    ];
    let at = |(x, y): (f32, f32)| Vector2 {
        X: tip + x * k,
        Y: tip + y * k,
    };
    sink.BeginFigure(at((0.0, 0.0)), D2D1_FIGURE_BEGIN_FILLED);
    for pt in pts {
        sink.AddLine(at(pt));
    }
    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
    if sink.Close().is_err() {
        return;
    }
    p.brush.SetColor(&color(tint.with_alpha(0.75)));
    p.rt.FillGeometry(&path, p.brush, None);
    p.brush
        .SetColor(&color(Color::rgb(0xFFFFFF).with_alpha(0.95)));
    p.rt.DrawGeometry(&path, p.brush, 1.3, None);
}

/// The session's name on a pill by the arrow.
unsafe fn tag(gpu: &render::Gpu, p: &Pen, at: (f32, f32), name: &str, tint: Color) {
    if name.is_empty() {
        return;
    }
    let wide: Vec<u16> = name.encode_utf16().collect();
    let Ok(layout) = gpu.dw.CreateTextLayout(&wide, &gpu.chip, TAG_MAX, 18.0) else {
        return;
    };
    let w = render::text_size(&layout).0.min(TAG_MAX).ceil() + 14.0;
    let r = d2rect(at.0, at.1, w, 19.0);
    p.fill_rounded(r, 9.5, tint.with_alpha(0.92));
    p.text(
        &gpu.chip,
        Color::rgb(0x0B0C0E),
        name,
        d2rect(at.0 + 7.0, at.1, w - 14.0, 19.0),
    );
}

struct PlateScene<'a> {
    h: f32,
    radius: f32,
    now: &'a Now,
    reason: Option<&'a windows::Win32::Graphics::DirectWrite::IDWriteTextLayout>,
    reason_h: f32,
    /// How much of the glide is left, while one runs.
    gliding: Option<f32>,
    history: &'a VecDeque<Entry>,
    speed: Duration,
    bypass: bool,
    hover: Option<Control>,
    dragging: bool,
}

/// The Thinking plate, drawn like the app's other plates: dark metal lit
/// from above, a seam cut round it, the lamp of what it is doing.
unsafe fn draw_plate(gpu: &render::Gpu, p: &Pen, s: PlateScene) {
    let (w, h) = (BOX_W, s.h);
    let radius = s.radius.max(6.0);
    let all = d2rect(0.0, 0.0, w, h);
    // Bypass tints the plate as it tints the frame.
    let lamp = if s.bypass {
        theme::waiting()
    } else {
        theme::working()
    };
    p.fill_rounded(all, radius, theme::plate_bottom());
    p.rt.PushAxisAlignedClip(&d2rect(0.0, 0.0, w, h), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    p.fill_gradient(
        d2rect(0.0, 0.0, w, h.min(120.0)),
        (0.0, 0.0),
        (0.0, h.min(120.0)),
        &[
            (0.0, theme::plate_top()),
            (1.0, theme::plate_bottom().with_alpha(0.0)),
        ],
    );
    p.rt.PopAxisAlignedClip();
    p.stroke_rounded(
        d2rect(0.5, 0.5, w - 1.0, h - 1.0),
        radius,
        lamp.with_alpha(if s.bypass { 0.9 } else { 0.55 }),
        if s.bypass { 2.0 } else { 1.0 },
    );
    let seam = d2rect(5.5, 5.5, w - 11.0, h - 11.0);
    let seam_r = (radius - 3.0).max(2.0);
    p.stroke_rounded(
        d2rect(5.5, 6.5, w - 11.0, h - 11.0),
        seam_r,
        theme::engrave_light(),
        1.0,
    );
    p.stroke_rounded(seam, seam_r, theme::engrave_dark(), 1.0);

    let x = PAD;
    let cw = w - 2.0 * PAD;
    // Title: the lamp, the name of the plate, and who is at work where.
    p.circle((x + 4.0, 24.0), 4.0, lamp, None);
    p.circle((x + 4.0, 24.0), 7.0, lamp.with_alpha(0.25), None);
    p.text(
        &gpu.title,
        theme::text(),
        "Thinking",
        d2rect(x + 16.0, 14.0, 80.0, 20.0),
    );
    let who = match (s.now.name.trim(), s.now.program.trim()) {
        ("", program) => program.to_string(),
        (name, "") => name.to_string(),
        (name, program) => format!("{name} in {program}"),
    };
    p.text(
        &gpu.small_right,
        theme::text_dim(),
        &who,
        d2rect(x + 100.0, 14.0, cw - 100.0, 20.0),
    );

    draw_controls(gpu, p, &s);
    let mut y = CONTROLS_Y + CONTROLS_STEP;
    if s.bypass {
        p.fill_rounded(
            d2rect(x, y - 2.0, cw, BANNER - 6.0),
            4.0,
            theme::waiting().with_alpha(0.18),
        );
        p.text(
            &gpu.small_bold,
            theme::waiting(),
            "BYPASS ON, Esc stops and turns it off",
            d2rect(x + 8.0, y, cw - 16.0, ROW),
        );
        y += BANNER;
    }
    p.text(&gpu.chip, theme::legend(), "NOW", d2rect(x, y, 60.0, 16.0));
    // The session's id and start, since two sessions can share a name.
    let ident_w = if s.gliding.is_some() {
        cw - 140.0
    } else {
        cw - 40.0
    };
    p.text(
        &gpu.small,
        theme::legend(),
        s.now.ident.trim(),
        d2rect(x + 40.0, y - 1.0, ident_w, 16.0),
    );
    if let Some(left) = s.gliding {
        p.text(
            &gpu.small_right,
            theme::waiting(),
            "Esc to stop",
            d2rect(x + 60.0, y, cw - 60.0, 16.0),
        );
        p.fill_rounded(d2rect(x, y + 18.0, cw, 2.0), 1.0, theme::well());
        p.fill_rounded(d2rect(x, y + 18.0, cw * left, 2.0), 1.0, theme::waiting());
    }
    y += 22.0;
    if let Some(reason) = s.reason {
        let shown = s.reason_h.min(REASON_LINES * REASON_LINE);
        p.rt.PushAxisAlignedClip(&d2rect(x, y, cw, shown), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        p.brush.SetColor(&color(if s.now.away.is_some() {
            theme::waiting()
        } else {
            theme::text()
        }));
        p.rt.DrawTextLayout(
            Vector2 { X: x, Y: y },
            reason,
            p.brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
        );
        p.rt.PopAxisAlignedClip();
        y += shown + 4.0;
    }
    let what = match (s.now.what.trim(), s.now.away.is_some()) {
        ("", false) => "looking".to_string(),
        (what, false) => what.to_string(),
        ("", true) => "Last in the program: looking".to_string(),
        (what, true) => format!("Last in the program: {what}"),
    };
    let ink = if s.now.away.is_some() {
        theme::legend()
    } else {
        theme::text_dim()
    };
    p.text(&gpu.small, ink, &what, d2rect(x, y, cw, ROW));
    y += ROW;

    if s.history.is_empty() {
        return;
    }
    y += 6.0;
    p.fill_rounded(d2rect(x, y + 1.0, cw, 1.0), 0.0, theme::engrave_light());
    p.fill_rounded(d2rect(x, y, cw, 1.0), 0.0, theme::engrave_dark());
    y += 8.0;
    p.text(
        &gpu.chip,
        theme::legend(),
        "HISTORY",
        d2rect(x, y, 80.0, 16.0),
    );
    y += 22.0;
    for (i, e) in s.history.iter().enumerate() {
        // The newest reads plainly, the older ones step back.
        let ink = if i == 0 {
            theme::text()
        } else {
            theme::text_dim()
        };
        p.text(&gpu.small, theme::legend(), &e.at, d2rect(x, y, 60.0, ROW));
        let mut buf = [0u8; 4];
        let mark = if e.unasked {
            theme::waiting()
        } else {
            e.mark.colour()
        };
        p.text(
            &gpu.icon_small,
            mark,
            e.mark.glyph().encode_utf8(&mut buf),
            d2rect(x + 58.0, y, 16.0, ROW),
        );
        p.text(
            &gpu.small,
            ink,
            &entry_line(e),
            d2rect(x + 80.0, y, cw - 80.0, ROW),
        );
        y += ROW;
    }
}

/// One command on the plate, laid out.
struct ShellRow<'a> {
    shell: &'a Shell,
    command: IDWriteTextLayout,
    /// How tall its command shows.
    h: f32,
    /// Lines of it the plate does not show.
    hidden: usize,
}

/// The shell section: what the session ran in a shell while control was
/// on, under the rest of the plate.
struct ShellsScene<'a> {
    top: f32,
    open: bool,
    count: String,
    rows: Vec<ShellRow<'a>>,
    hover: bool,
}

/// How many lines laid out text has, and how tall one is.
fn line_metrics(l: &IDWriteTextLayout) -> (usize, f32) {
    let mut m = DWRITE_TEXT_METRICS::default();
    match unsafe { l.GetMetrics(&mut m) } {
        Ok(()) if m.lineCount > 0 => (m.lineCount as usize, m.height / m.lineCount as f32),
        _ => (1, REASON_LINE),
    }
}

/// The commands' face: monospaced, as a terminal shows them, trimmed with
/// an ellipsis where the plate stops showing them.
unsafe fn mono_format(gpu: &render::Gpu) -> Option<IDWriteTextFormat> {
    let f = gpu
        .dw
        .CreateTextFormat(
            w!("Consolas"),
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            12.0,
            w!("en-us"),
        )
        .ok()?;
    let trimming = DWRITE_TRIMMING {
        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
        delimiter: 0,
        delimiterCount: 0,
    };
    if let Ok(sign) = gpu.dw.CreateEllipsisTrimmingSign(&f) {
        let _ = f.SetTrimming(&trimming, &sign);
    }
    Some(f)
}

/// The shell section: a header that folds it, and each command with when
/// it began, how it stands, what it is for, and the command itself.
unsafe fn draw_shells(gpu: &render::Gpu, p: &Pen, s: &ShellsScene) {
    let x = PAD;
    let cw = BOX_W - 2.0 * PAD;
    let mut y = s.top + 6.0;
    p.fill_rounded(d2rect(x, y + 1.0, cw, 1.0), 0.0, theme::engrave_light());
    p.fill_rounded(d2rect(x, y, cw, 1.0), 0.0, theme::engrave_dark());
    y = s.top + SHELLS_GAP;
    let ink = if s.hover {
        theme::text()
    } else {
        theme::legend()
    };
    p.text(
        &gpu.chip,
        ink,
        "SHELL COMMANDS",
        d2rect(x, y, 140.0, FOLD_H),
    );
    p.text(
        &gpu.small_right,
        if s.hover {
            theme::text()
        } else {
            theme::text_dim()
        },
        &s.count,
        d2rect(x + 140.0, y, cw - 140.0 - 24.0, FOLD_H),
    );
    let mut buf = [0u8; 4];
    let chevron = if s.open { OPEN } else { FOLDED };
    p.text(
        &gpu.icon_small,
        ink,
        chevron.encode_utf8(&mut buf),
        d2rect(x + cw - 16.0, y, 16.0, FOLD_H),
    );
    y += FOLD_H;
    if !s.open {
        return;
    }
    if s.rows.is_empty() {
        p.text(
            &gpu.small,
            theme::legend(),
            "No shell command since control began",
            d2rect(x, y, cw, ROW),
        );
        return;
    }
    for row in &s.rows {
        let shell = row.shell;
        let running = shell.state == ShellState::Running;
        p.text(
            &gpu.small,
            theme::legend(),
            &shell.at,
            d2rect(x, y, 60.0, ROW),
        );
        let mut buf = [0u8; 4];
        p.text(
            &gpu.icon_small,
            shell.state.colour(),
            shell.state.glyph().encode_utf8(&mut buf),
            d2rect(x + 58.0, y, 16.0, ROW),
        );
        let mut left = x + 80.0;
        if shell.agent {
            p.fill_rounded(d2rect(left, y + 2.0, 42.0, ROW - 4.0), 4.0, theme::well());
            p.text(
                &gpu.chip,
                theme::legend(),
                "agent",
                d2rect(left + 6.0, y, 36.0, ROW),
            );
            left += 48.0;
        }
        let word = shell.state.word();
        let word_w = {
            let wide: Vec<u16> = word.encode_utf16().collect();
            gpu.dw
                .CreateTextLayout(&wide, &gpu.small, cw, ROW)
                .map_or(80.0, |l| render::text_size(&l).0.ceil())
        };
        p.text(
            &gpu.small_right,
            shell.state.colour(),
            &word,
            d2rect(x, y, cw, ROW),
        );
        p.text(
            &gpu.small,
            if running {
                theme::text()
            } else {
                theme::text_dim()
            },
            &shell.title(),
            d2rect(left, y, (x + cw - word_w - 10.0 - left).max(0.0), ROW),
        );
        y += ROW;
        let well = d2rect(x, y, cw, row.h + 2.0 * WELL_PAD);
        p.fill_rounded(well, 4.0, theme::well());
        if running {
            p.stroke_rounded(
                d2rect(x + 0.5, y + 0.5, cw - 1.0, row.h + 2.0 * WELL_PAD - 1.0),
                4.0,
                theme::working().with_alpha(0.6),
                1.0,
            );
        }
        p.rt.PushAxisAlignedClip(
            &d2rect(x, y + WELL_PAD, cw, row.h),
            D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
        );
        p.brush.SetColor(&color(if running {
            theme::text()
        } else {
            theme::text_dim()
        }));
        p.rt.DrawTextLayout(
            Vector2 {
                X: x + 6.0,
                Y: y + WELL_PAD,
            },
            &row.command,
            p.brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
        );
        p.rt.PopAxisAlignedClip();
        // A cut at a line's end shows no ellipsis, so the rest is counted.
        if let Some(more) = more_lines(row.hidden) {
            let (tx, ty) = (x + cw - 74.0, y + WELL_PAD + row.h - 15.0);
            p.fill_rounded(d2rect(tx, ty, 70.0, 15.0), 4.0, theme::plate_bottom());
            p.text(
                &gpu.chip,
                theme::waiting(),
                &more,
                d2rect(tx + 6.0, ty, 62.0, 15.0),
            );
        }
        y += row.h + 2.0 * WELL_PAD + SHELL_GAP;
    }
}

/// The control row: the glide's speed on a slider drawn like the glide's
/// own bar, and the Bypass switch.
unsafe fn draw_controls(gpu: &render::Gpu, p: &Pen, s: &PlateScene) {
    let y = CONTROLS_Y;
    let mid = y + CONTROLS_H / 2.0;
    p.text(
        &gpu.chip,
        theme::legend(),
        "SPEED",
        d2rect(PAD, mid - 8.0, 48.0, 16.0),
    );
    let at = slider_of_glide(s.speed);
    let live = s.dragging || s.hover == Some(Control::Speed);
    p.fill_rounded(
        d2rect(SLIDER_X, mid - 1.5, SLIDER_W, 3.0),
        1.5,
        theme::well(),
    );
    p.fill_rounded(
        d2rect(SLIDER_X, mid - 1.5, SLIDER_W * at, 3.0),
        1.5,
        theme::working(),
    );
    let knob = (SLIDER_X + SLIDER_W * at, mid);
    if live {
        p.circle(knob, 9.0, theme::working().with_alpha(0.25), None);
    }
    p.circle(knob, 5.5, theme::text(), None);
    p.circle(knob, 5.5, theme::working(), Some(1.5));
    p.text(
        &gpu.small,
        if live {
            theme::text()
        } else {
            theme::text_dim()
        },
        &speed_label(s.speed),
        d2rect(SLIDER_X + SLIDER_W + 12.0, mid - 9.0, 50.0, ROW),
    );

    let button = d2rect(BYPASS_X, y, BYPASS_W, CONTROLS_H);
    let hover = s.hover == Some(Control::Bypass);
    if s.bypass {
        p.fill_rounded(button, 5.0, theme::waiting());
        p.text(&gpu.small_centre, Color::rgb(0x0B0C0E), "Bypass on", button);
    } else {
        p.fill_rounded(button, 5.0, theme::well());
        let edge = if hover {
            theme::waiting()
        } else {
            theme::engrave_light()
        };
        p.stroke_rounded(
            d2rect(BYPASS_X + 0.5, y + 0.5, BYPASS_W - 1.0, CONTROLS_H - 1.0),
            5.0,
            edge,
            1.0,
        );
        let ink = if hover {
            theme::text()
        } else {
            theme::text_dim()
        };
        p.text(&gpu.small_centre, ink, "Bypass", button);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => {
            let at = (
                (lparam.0 & 0xFFFF) as i16 as i32,
                ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
            );
            let over = OVERLAY.with(|o| {
                o.try_borrow()
                    .ok()
                    .and_then(|o| o.as_ref().map(|o| o.over_control(hwnd, at)))
                    .unwrap_or(false)
            });
            if over {
                LRESULT(HTCLIENT as isize)
            } else {
                LRESULT(HTTRANSPARENT as isize)
            }
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_MOUSEMOVE | WM_CAPTURECHANGED => {
            let turned_on = OVERLAY.with(|o| {
                o.try_borrow_mut()
                    .ok()
                    .and_then(|mut o| o.as_mut().map(|o| o.mouse(hwnd, msg, lparam)))
                    .unwrap_or(false)
            });
            // Outside the overlay's borrow: the app may hand it calls.
            if turned_on {
                super::with_app(|app| app.desktop_bypass_on());
            }
            LRESULT(0)
        }
        GUARD_MSG => {
            OVERLAY.with(|o| {
                if let Ok(mut o) = o.try_borrow_mut() {
                    if let Some(o) = o.as_mut() {
                        o.tripped();
                    }
                }
            });
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_TIMER if wparam.0 == FRAME => {
            crate::vsync::took(hwnd, FRAME);
            OVERLAY.with(|o| {
                if let Ok(mut o) = o.try_borrow_mut() {
                    if let Some(o) = o.as_mut() {
                        o.tick();
                    }
                }
            });
            LRESULT(0)
        }
        WM_HOTKEY if wparam.0 as i32 == STOP_KEY => {
            OVERLAY.with(|o| {
                if let Ok(mut o) = o.try_borrow_mut() {
                    if let Some(o) = o.as_mut() {
                        o.stop();
                    }
                }
            });
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(n: usize) -> Entry {
        Entry {
            at: clock(9, 5, n as u16),
            mark: Mark::Ok,
            what: format!("click {n}"),
            outcome: String::new(),
            unasked: false,
        }
    }

    #[test]
    fn control_starts_with_an_allowed_call_and_lasts_after_the_last() {
        let t0 = Instant::now();
        assert_eq!(extend(None, t0, false), None);
        let until = extend(None, t0, true);
        assert_eq!(until, Some(t0 + ACTIVE_FOR));
        let later = t0 + Duration::from_secs(15);
        assert_eq!(extend(until, later, false), Some(later + ACTIVE_FOR));
        // Over: a refusal or a done does not bring it back.
        let after = t0 + Duration::from_secs(21);
        assert_eq!(extend(until, after, false), None);
        assert_eq!(extend(until, after, true), Some(after + ACTIVE_FOR));
    }

    #[test]
    fn the_ghost_glides_softly_from_start_to_point() {
        let (a, b) = ((0.0, 100.0), (200.0, -100.0));
        for takes in [GLIDE, GLIDE_FAST, GLIDE_SLOW] {
            assert_eq!(glide_at(a, b, Duration::ZERO, takes), a);
            assert_eq!(glide_at(a, b, takes, takes), b);
            assert_eq!(glide_at(a, b, takes * 3, takes), b);
            let half = glide_at(a, b, takes / 2, takes);
            assert!((half.0 - 100.0).abs() < 0.01 && half.1.abs() < 0.01);
            // Slow at the start: a tenth of the time is far less than a
            // tenth of the way.
            assert!(glide_at(a, b, takes / 10, takes).0 < 10.0);
        }
        // A hold goes nowhere.
        assert_eq!(glide_at(a, a, GLIDE / 2, GLIDE), a);
    }

    #[test]
    fn the_glide_counts_down_its_bar() {
        for takes in [GLIDE, GLIDE_FAST] {
            assert_eq!(left_of_glide(Duration::ZERO, takes), 1.0);
            assert!((left_of_glide(takes / 4, takes) - 0.75).abs() < 1e-4);
            assert_eq!(left_of_glide(takes * 2, takes), 0.0);
        }
    }

    #[test]
    fn the_slider_runs_from_slow_on_the_left_to_fast_on_the_right() {
        assert_eq!(glide_of_slider(0.0), GLIDE_SLOW);
        assert_eq!(glide_of_slider(1.0), GLIDE_FAST);
        assert_eq!(glide_of_slider(-3.0), GLIDE_SLOW);
        assert_eq!(glide_of_slider(7.0), GLIDE_FAST);
        assert_eq!(glide_of_slider(f32::NAN), GLIDE_SLOW);
        // Whole tenths only, as the label shows.
        for i in 0..=100 {
            let d = glide_of_slider(i as f32 / 100.0);
            assert_eq!(d.as_millis() % 100, 0, "{d:?}");
            assert!((GLIDE_FAST..=GLIDE_SLOW).contains(&d));
        }
        // And back: a glide's place gives the same glide.
        for ms in [300, 1000, 2000, 3700, 4000] {
            let d = Duration::from_millis(ms);
            assert_eq!(glide_of_slider(slider_of_glide(d)), d);
        }
        assert_eq!(slider_of_glide(Duration::from_secs(60)), 0.0);
        assert_eq!(slider_of_glide(Duration::ZERO), 1.0);
    }

    #[test]
    fn the_slider_takes_its_place_from_the_mouse() {
        assert_eq!(slider_at(SLIDER_X), 0.0);
        assert_eq!(slider_at(SLIDER_X + SLIDER_W), 1.0);
        assert_eq!(slider_at(SLIDER_X + SLIDER_W / 2.0), 0.5);
        assert_eq!(slider_at(0.0), 0.0);
        assert_eq!(slider_at(BOX_W), 1.0);
        assert_eq!(speed_label(GLIDE), "2.0 s");
        assert_eq!(speed_label(GLIDE_FAST), "0.3 s");
    }

    #[test]
    fn the_speed_is_kept_only_when_moved_from_the_default() {
        assert_eq!(glide_to_saved(GLIDE), None);
        assert_eq!(glide_from_saved(None), GLIDE);
        let d = Duration::from_millis(700);
        assert_eq!(glide_from_saved(glide_to_saved(d)), d);
        // A file edited by hand stays between the ends.
        assert_eq!(glide_from_saved(Some(10)), GLIDE_FAST);
        assert_eq!(glide_from_saved(Some(60_000)), GLIDE_SLOW);
    }

    #[test]
    fn the_mouse_finds_the_controls_and_nothing_else() {
        let mid = CONTROLS_Y + CONTROLS_H / 2.0;
        assert_eq!(control_at(SLIDER_X + 10.0, mid, None), Some(Control::Speed));
        assert_eq!(control_at(SLIDER_X - 4.0, mid, None), Some(Control::Speed));
        assert_eq!(control_at(BYPASS_X + 5.0, mid, None), Some(Control::Bypass));
        assert_eq!(
            control_at(BOX_W - PAD - 1.0, mid, None),
            Some(Control::Bypass)
        );
        // The title, the label, the gap and the history are not controls.
        assert_eq!(control_at(SLIDER_X + 10.0, 20.0, None), None);
        assert_eq!(control_at(PAD + 2.0, mid, None), None);
        assert_eq!(control_at(BYPASS_X - 4.0, mid, None), None);
        assert_eq!(
            control_at(BYPASS_X + 5.0, CONTROLS_Y + CONTROLS_H, None),
            None
        );
        assert_eq!(control_at(BOX_W - 2.0, mid, None), None);
    }

    #[test]
    fn the_guard_tells_the_agent_what_opened() {
        assert_eq!(
            guard_error("Synchronization", guard::Kind::Dialog, false),
            "stopped: the window \"Synchronization\" opened while the ghost was on its way; \
             look again before clicking"
        );
        assert_eq!(
            guard_error(" ", guard::Kind::Front, true),
            "stopped: a window with no title came to the front while Horadric held the \
             action; look again before acting"
        );
        assert_eq!(
            guard_outcome("Save As", guard::Kind::Shown),
            "stopped: the window \"Save As\" opened"
        );
    }

    #[test]
    fn esc_is_held_while_a_glide_runs_or_bypass_is_on() {
        assert!(!wants_esc(false, false));
        assert!(wants_esc(true, false));
        assert!(wants_esc(false, true));
        assert!(wants_esc(true, true));
    }

    #[test]
    fn esc_halts_what_glides_and_ends_bypass_even_with_nothing_on_its_way() {
        assert_eq!(esc_does(true, false), (true, false));
        assert_eq!(esc_does(true, true), (true, true));
        assert_eq!(esc_does(false, true), (false, true));
        assert_eq!(esc_does(false, false), (false, false));
    }

    #[test]
    fn only_what_the_human_allowed_goes_without_esc() {
        assert!(may_glide(false, false));
        assert!(may_glide(false, true));
        assert!(may_glide(true, true));
        assert!(!may_glide(true, false));
    }

    #[test]
    fn a_trip_stops_only_the_glide_it_came_for() {
        assert!(trip_stops(4, Some(4)));
        assert!(!trip_stops(3, Some(4)));
        assert!(!trip_stops(4, None));
    }

    #[test]
    fn now_says_when_the_session_works_elsewhere() {
        let sysmac = "Sysmac Studio";
        let click = Some("mcp__horadric__desktop_click");
        assert_eq!(away_of(click, Turn::Working, sysmac), None);
        assert_eq!(away_of(None, Turn::Working, sysmac), None);
        assert_eq!(
            away_of(Some("Bash"), Turn::Working, sysmac),
            Some("Working in the shell (Bash), not in Sysmac Studio.".into())
        );
        assert_eq!(
            away_of(Some("mcp__horadric__browser_open"), Turn::Working, ""),
            Some("Working in the browser (browser_open), not in the program.".into())
        );
        assert!(away_of(click, Turn::Resting, sysmac)
            .unwrap()
            .starts_with("Finished its turn"));
        assert_eq!(
            away_of(None, Turn::Gone, sysmac),
            Some("The session has ended.".into())
        );
    }

    #[test]
    fn tools_are_placed_in_words() {
        assert!(is_desktop_tool("desktop_type"));
        assert!(is_desktop_tool("mcp__horadric__desktop_text"));
        assert!(!is_desktop_tool("mcp__horadric__browser_click"));
        assert!(!is_desktop_tool("Bash"));
        assert_eq!(tool_place("PowerShell"), "in the shell");
        assert_eq!(tool_place("Edit"), "on files");
        assert_eq!(
            tool_place("mcp__omron__sysmac_project_build"),
            "with sysmac_project_build"
        );
    }

    #[test]
    fn a_session_works_until_its_turn_is_over() {
        let at = |p: Phase| turn_of(Some(&p));
        assert_eq!(at(Phase::Working), Turn::Working);
        assert_eq!(at(Phase::Waiting(WaitReason::Permission)), Turn::Working);
        assert_eq!(at(Phase::Waiting(WaitReason::Dialog)), Turn::Working);
        assert_eq!(at(Phase::Done), Turn::Resting);
        assert_eq!(at(Phase::Idle), Turn::Resting);
        assert_eq!(at(Phase::Waiting(WaitReason::Input)), Turn::Resting);
        assert_eq!(
            at(Phase::Waiting(WaitReason::Error("x".into()))),
            Turn::Resting
        );
        assert_eq!(at(Phase::Ended), Turn::Gone);
        assert_eq!(at(Phase::Paused), Turn::Gone);
        assert_eq!(turn_of(None), Turn::Gone);
    }

    #[test]
    fn the_history_says_when_bypass_began_and_how_it_ended() {
        let line = |end| {
            let (what, outcome) = bypass_words(end);
            entry_line(&Entry {
                at: String::new(),
                mark: Mark::Ok,
                what: what.into(),
                outcome: outcome.into(),
                unasked: false,
            })
        };
        assert_eq!(line(None), "Bypass on");
        assert_eq!(line(Some(BypassEnd::Esc)), "Bypass off: Esc");
        assert_eq!(line(Some(BypassEnd::Switched)), "Bypass off: switched off");
        assert_eq!(
            line(Some(BypassEnd::Done)),
            "Bypass off: the work it was on for is done"
        );
    }

    #[test]
    fn bypass_ends_when_the_work_it_was_on_for_is_done() {
        let mut held = HashMap::new();
        // Nobody to wait for: bypass is left to the human.
        assert!(!bypass_over(&held, false));
        held.insert("a".to_string(), Holder::default());
        held.get_mut("a").unwrap().saw(Turn::Working, false);
        assert!(!bypass_over(&held, false));
        // Done, the Stop hook's phase, after working.
        held.get_mut("a").unwrap().saw(Turn::Resting, false);
        assert!(bypass_over(&held, false));
        // Not while the last action is still on its way.
        assert!(!bypass_over(&held, true));
        // A second session still at work keeps it on.
        held.insert("b".to_string(), Holder::default());
        held.get_mut("b").unwrap().called();
        assert!(!bypass_over(&held, false));
        // Its Stop, StopFailure or SessionEnd ends its share.
        held.get_mut("b").unwrap().ended = true;
        assert!(bypass_over(&held, false));
    }

    #[test]
    fn a_resting_session_gets_time_to_start_working() {
        let mut h = Holder::default();
        h.saw(Turn::Resting, false);
        assert!(!h.ended, "turned on before its next prompt");
        h.saw(Turn::Resting, true);
        assert!(h.ended, "rested the whole wait");
        let mut gone = Holder::default();
        gone.saw(Turn::Gone, false);
        assert!(gone.ended);
        // A new call while bypass is on starts its share over.
        gone.called();
        assert_eq!(
            gone,
            Holder {
                worked: true,
                ended: false
            }
        );
        // Working after its turn ended does not undo the end the hook told.
        gone.ended = true;
        gone.saw(Turn::Working, false);
        assert!(gone.ended);
    }

    #[test]
    fn what_bypass_let_through_is_marked_once() {
        let mut list = VecDeque::new();
        note_unasked(&mut list, "type \"x\"");
        note_unasked(&mut list, "click OK");
        assert!(take_unasked(&mut list, "click OK"));
        assert!(!take_unasked(&mut list, "click OK"));
        assert!(take_unasked(&mut list, "type \"x\""));
        for n in 0..HISTORY + 4 {
            note_unasked(&mut list, &format!("click {n}"));
        }
        assert_eq!(list.len(), HISTORY);
        assert!(!take_unasked(&mut list, "click 0"));
    }

    #[test]
    fn the_frame_breathes_between_faint_and_full() {
        assert!((pulse(Duration::ZERO) - 0.45).abs() < 1e-4);
        assert!((pulse(PULSE / 2) - 1.0).abs() < 1e-4);
        assert!((pulse(PULSE) - 0.45).abs() < 1e-4);
        for ms in (0..4000).step_by(37) {
            let a = pulse(Duration::from_millis(ms));
            assert!((0.45..=1.0001).contains(&a), "{ms}: {a}");
        }
    }

    #[test]
    fn the_ghost_stays_then_fades() {
        assert_eq!(ghost_alpha(Duration::ZERO), 1.0);
        assert_eq!(ghost_alpha(GHOST_STAYS), 1.0);
        assert!((ghost_alpha(GHOST_STAYS + GHOST_FADE / 2) - 0.5).abs() < 1e-4);
        assert_eq!(ghost_alpha(GHOST_STAYS + GHOST_FADE), 0.0);
    }

    #[test]
    fn four_strips_frame_a_monitor_without_overlapping() {
        let m = [-2560, 0, 0, 1440];
        assert_eq!(
            edges(m, 10),
            [
                [-2560, 0, 2560, 10],
                [-2560, 1430, 2560, 10],
                [-2560, 10, 10, 1420],
                [-10, 10, 10, 1420],
            ]
        );
        // A monitor too small for the depth keeps strips that fit.
        let tiny = edges([0, 0, 6, 6], 10);
        assert!(tiny.iter().all(|r| r[2] >= 0 && r[3] >= 0));
    }

    #[test]
    fn the_plate_sits_in_the_top_right_of_the_work_area() {
        assert_eq!(box_place([0, 0, 1920, 1040], (380, 200), 18), (1522, 18));
        assert_eq!(box_place([-2560, 40, 0, 1440], (380, 200), 18), (-398, 58));
        // Narrower than the plate: it keeps its left edge on the screen.
        assert_eq!(box_place([100, 0, 300, 600], (380, 200), 18), (100, 18));
    }

    #[test]
    fn the_plate_grows_with_its_reason_and_history() {
        let bare = box_height(0.0, 0, false, 0.0);
        // Room for the controls under the title.
        assert!(bare >= CONTROLS_Y + CONTROLS_STEP + 22.0 + ROW);
        assert!(box_height(17.0, 0, false, 0.0) > bare);
        assert_eq!(
            box_height(500.0, 0, false, 0.0),
            box_height(REASON_LINES * REASON_LINE, 0, false, 0.0)
        );
        let one = box_height(0.0, 1, false, 0.0);
        assert!(one > bare);
        assert_eq!(box_height(0.0, 3, false, 0.0) - one, 2.0 * ROW);
        // And for the bypass band while it is on.
        assert_eq!(box_height(0.0, 1, true, 0.0) - one, BANNER);
    }

    #[test]
    fn history_keeps_the_newest_on_top() {
        let mut h = VecDeque::new();
        for n in 0..HISTORY + 3 {
            remember(&mut h, entry(n));
        }
        assert_eq!(h.len(), HISTORY);
        assert_eq!(h.front(), Some(&entry(HISTORY + 2)));
        assert_eq!(h.back(), Some(&entry(3)));
    }

    #[test]
    fn history_lines_read_what_then_how_it_went() {
        let mut e = entry(1);
        assert_eq!(entry_line(&e), "click 1");
        e.outcome = "dialog opened".into();
        assert_eq!(entry_line(&e), "click 1: dialog opened");
        e.what = " ".into();
        assert_eq!(entry_line(&e), "dialog opened");
        e.unasked = true;
        assert_eq!(entry_line(&e), "unasked: dialog opened");
        e.outcome.clear();
        assert_eq!(entry_line(&e), "unasked");
        assert_eq!(clock(7, 3, 9), "07:03:09");
    }

    #[test]
    fn each_mark_has_its_own_glyph() {
        let marks = [Mark::Ok, Mark::Failed, Mark::Refused, Mark::Stopped];
        let glyphs: std::collections::HashSet<char> = marks.iter().map(|m| m.glyph()).collect();
        assert_eq!(glyphs.len(), marks.len());
    }

    fn hook(name: &str, tool: &str, id: &str) -> HookEvent {
        HookEvent {
            tool_name: Some(tool.into()),
            tool_use_id: Some(id.into()),
            tool_input: Some(json!({"command": "dir C:\\", "description": "List the drive"})),
            ..HookEvent::synthetic(name)
        }
    }

    fn began(id: &str, cmd: &str) -> ShellNote {
        ShellNote::Began {
            session: "s".into(),
            shell: Shell {
                id: id.into(),
                tool: "PowerShell".into(),
                command: cmd.into(),
                description: String::new(),
                agent: false,
                at: "10:00:00".into(),
                state: ShellState::Running,
            },
        }
    }

    #[test]
    fn only_shell_tools_are_listed() {
        assert!(is_shell_tool("Bash"));
        assert!(is_shell_tool("PowerShell"));
        assert!(is_shell_tool("mcp__x__Bash"));
        assert!(!is_shell_tool("BashOutput"));
        assert!(!is_shell_tool("Read"));
        assert!(shell_note("s", &hook("PreToolUse", "Read", "t"), "10:00:00").is_none());
        assert!(shell_note("s", &hook("Stop", "Bash", "t"), "10:00:00").is_none());
    }

    #[test]
    fn a_shell_command_begins_with_what_it_runs_and_why() {
        let mut e = hook("PreToolUse", "PowerShell", "t1");
        e.agent_id = Some("a1".into());
        let Some(ShellNote::Began { session, shell }) = shell_note("tile", &e, "10:00:00") else {
            panic!("not a start");
        };
        assert_eq!(session, "tile");
        assert_eq!(shell.id, "t1");
        assert_eq!(shell.tool, "PowerShell");
        assert_eq!(shell.command, "dir C:\\");
        assert_eq!(shell.title(), "List the drive");
        assert!(shell.agent);
        assert_eq!(shell.state, ShellState::Running);
        let bare = Shell {
            description: " ".into(),
            ..shell
        };
        assert_eq!(bare.title(), "PowerShell command");
    }

    #[test]
    fn a_shell_command_ends_as_its_result_says() {
        let input = json!({"command": "x"});
        let ok = json!({"stdout": "hi", "stderr": "", "interrupted": false});
        assert_eq!(
            shell_ended("Bash", &input, &ok, None, false),
            ShellState::Done
        );
        assert_eq!(
            shell_ended("Bash", &input, &json!("plain text"), None, false),
            ShellState::Done
        );
        let cut = json!({"stdout": "", "interrupted": true});
        assert_eq!(
            shell_ended("Bash", &input, &cut, None, false),
            ShellState::Interrupted
        );
        let late = json!({"stdout": "", "backgroundTaskId": "b1", "timedOutAfterMs": 120000});
        assert_eq!(
            shell_ended("Bash", &input, &late, None, false),
            ShellState::TimedOut
        );
        let sent = json!({"stdout": "", "backgroundTaskId": "b1"});
        assert_eq!(
            shell_ended("Bash", &input, &sent, None, false),
            ShellState::Background
        );
        let null = serde_json::Value::Null;
        assert_eq!(
            shell_ended(
                "PowerShell",
                &input,
                &null,
                Some("Exit code 3\nnope"),
                false
            ),
            ShellState::Failed(Some(3))
        );
        assert_eq!(
            shell_ended("PowerShell", &input, &null, Some("it broke"), false),
            ShellState::Failed(None)
        );
        assert_eq!(
            shell_ended("PowerShell", &input, &null, Some("Exit code 1"), true),
            ShellState::Interrupted
        );
    }

    #[test]
    fn a_failure_hook_carries_its_exit_code() {
        let mut e = hook("PostToolUseFailure", "Bash", "t1");
        e.error = Some(json!("Exit code 127\ncommand not found"));
        let Some(ShellNote::Ended { id, state, .. }) = shell_note("s", &e, "10:00:01") else {
            panic!("not an end");
        };
        assert_eq!(id, "t1");
        assert_eq!(state, ShellState::Failed(Some(127)));
        let mut stop = hook("PostToolUseFailure", "Bash", "t2");
        stop.is_interrupt = Some(json!(true));
        assert!(matches!(
            shell_note("s", &stop, "10:00:01"),
            Some(ShellNote::Ended {
                state: ShellState::Interrupted,
                ..
            })
        ));
    }

    #[test]
    fn the_list_keeps_the_newest_on_top_and_ends_only_what_it_saw_begin() {
        let mut lists = HashMap::new();
        for i in 0..SHELLS + 2 {
            hear_shell(&mut lists, began(&format!("t{i}"), &format!("cmd {i}")));
        }
        let list = &lists["s"];
        assert_eq!(list.len(), SHELLS);
        assert_eq!(list[0].command, format!("cmd {}", SHELLS + 1));
        let ended = |id: &str, state| ShellNote::Ended {
            session: "s".into(),
            id: id.into(),
            state,
        };
        let done = hear_shell(&mut lists, ended("t9", ShellState::Done)).unwrap();
        assert_eq!(done.command, "cmd 9");
        assert_eq!(lists["s"][0].state, ShellState::Done);
        // Ended twice, or begun before control: nothing to end.
        assert!(hear_shell(&mut lists, ended("t9", ShellState::Failed(Some(1)))).is_none());
        assert!(hear_shell(&mut lists, ended("t0", ShellState::Done)).is_none());
        assert!(hear_shell(&mut lists, ended("", ShellState::Done)).is_none());
        let other = ShellNote::Ended {
            session: "x".into(),
            id: "t8".into(),
            state: ShellState::Done,
        };
        assert!(hear_shell(&mut lists, other).is_none());
        assert_eq!(lists["s"][1].state, ShellState::Running);
    }

    #[test]
    fn the_fold_counts_what_runs() {
        let mut lists = HashMap::new();
        assert_eq!(shells_count(&VecDeque::new()), "none yet");
        hear_shell(&mut lists, began("a", "x"));
        hear_shell(&mut lists, began("b", "y"));
        assert_eq!(shells_count(&lists["s"]), "2, 2 running");
        lists.get_mut("s").unwrap()[0].state = ShellState::Done;
        lists.get_mut("s").unwrap()[1].state = ShellState::Done;
        assert_eq!(shells_count(&lists["s"]), "2");
    }

    #[test]
    fn each_shell_state_reads_and_shows_its_own_way() {
        assert_eq!(ShellState::Failed(Some(2)).word(), "exit 2");
        assert_eq!(ShellState::Failed(None).word(), "failed");
        assert_eq!(ShellState::TimedOut.word(), "timed out, in background");
        let states = [
            ShellState::Running,
            ShellState::Done,
            ShellState::Failed(None),
            ShellState::Interrupted,
            ShellState::TimedOut,
        ];
        let glyphs: std::collections::HashSet<char> = states.iter().map(|s| s.glyph()).collect();
        assert_eq!(glyphs.len(), states.len());
    }

    #[test]
    fn the_running_command_shows_more_of_itself() {
        assert_eq!(command_height(20, 14.0, ShellState::Running), 8.0 * 14.0);
        assert_eq!(command_height(20, 14.0, ShellState::Done), 2.0 * 14.0);
        assert_eq!(command_height(1, 14.0, ShellState::Running), 14.0);
        assert_eq!(command_height(0, 14.0, ShellState::Done), 14.0);
        assert_eq!(hidden_lines(20, ShellState::Running), 12);
        assert_eq!(hidden_lines(3, ShellState::Done), 1);
        assert_eq!(hidden_lines(2, ShellState::Done), 0);
        assert_eq!(more_lines(0), None);
        assert_eq!(more_lines(1).as_deref(), Some("+1 line"));
        assert_eq!(more_lines(12).as_deref(), Some("+12 lines"));
        let long = "é".repeat(COMMAND_CHARS + 10);
        assert_eq!(command_shown(&long).chars().count(), COMMAND_CHARS);
        assert_eq!(command_shown("ls"), "ls");
    }

    #[test]
    fn the_shell_section_sits_under_the_rest_and_grows_with_its_commands() {
        let body = body_height(0.0, 2, false);
        let folded = shells_height(false, &[14.0, 28.0]);
        assert_eq!(folded, SHELLS_GAP + FOLD_H);
        assert_eq!(shells_height(true, &[]), folded + ROW);
        let open = shells_height(true, &[14.0, 28.0]);
        assert_eq!(open - folded, shell_row(14.0) + shell_row(28.0));
        assert_eq!(box_height(0.0, 2, false, open), (body + open + PAD).ceil());
        assert_eq!(box_height(0.0, 2, false, 0.0), (body + PAD).ceil());
    }

    #[test]
    fn only_the_newest_commands_that_fit_on_screen_are_shown() {
        let rows = [14.0, 112.0, 28.0];
        let head = SHELLS_GAP + FOLD_H;
        assert_eq!(shells_fit(&rows, 10_000.0), 3);
        assert_eq!(shells_fit(&rows, head + shell_row(14.0)), 1);
        assert_eq!(shells_fit(&rows, head + shell_row(14.0) - 1.0), 0);
        assert_eq!(shells_fit(&rows, 0.0), 0);
    }

    #[test]
    fn the_fold_header_is_a_control_and_the_rest_of_the_section_is_not() {
        let top = 300.0;
        let header = top + SHELLS_GAP + 4.0;
        assert_eq!(
            control_at(PAD + 2.0, header, Some(top)),
            Some(Control::Shells)
        );
        assert_eq!(
            control_at(BOX_W - PAD - 1.0, header, Some(top)),
            Some(Control::Shells)
        );
        assert_eq!(control_at(PAD + 2.0, header, None), None);
        assert_eq!(control_at(2.0, header, Some(top)), None);
        assert_eq!(control_at(PAD + 2.0, top + 2.0, Some(top)), None);
        assert_eq!(
            control_at(PAD + 2.0, top + SHELLS_GAP + FOLD_H + 2.0, Some(top)),
            None
        );
        let mid = CONTROLS_Y + CONTROLS_H / 2.0;
        assert_eq!(
            control_at(BYPASS_X + 5.0, mid, Some(top)),
            Some(Control::Bypass)
        );
    }

    #[test]
    fn the_log_says_what_ran_where_and_how_it_went() {
        let mut shell = Shell {
            id: "t".into(),
            tool: "PowerShell".into(),
            command: "Get-Process |\n  Select -First 3".into(),
            description: "List processes".into(),
            agent: false,
            at: "10:00:00".into(),
            state: ShellState::Running,
        };
        assert_eq!(
            shell_log_line("2026-11-26T10:00:00Z", "fix plc", &shell),
            "2026-11-26T10:00:00Z  fix plc  shell command started  PowerShell  \
             (Get-Process | Select -First 3)  [List processes]\n"
        );
        shell.state = ShellState::Failed(Some(1));
        shell.agent = true;
        shell.description.clear();
        assert_eq!(
            shell_log_line("t", "s", &shell),
            "t  s  shell command exit 1 by a subagent  PowerShell  \
             (Get-Process | Select -First 3)\n"
        );
    }
}
