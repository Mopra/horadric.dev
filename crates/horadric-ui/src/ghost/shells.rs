//! The shell commands the sessions in control run beside it: what the
//! hook thread hears of them, the list the plate shows, and their lines
//! in `takeover.log`, the audit trail.
//!
//! The list is for the plate and starts empty with each run of control.
//! The ledger is for the log, and outlives the run: a command begun under
//! control gets its end line whenever that end comes, also after the
//! plate is gone and after eight newer commands pushed it off the list.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use horadric_core::event::HookEvent;
use horadric_core::folio::{read_result, ResultKind};

use super::rules::{BOX_W, CHECK, CROSS, ERROR, PAD, ROW, STOP};
use crate::theme::{self, Color};

/// Shell commands the plate lists for one session, newest first.
pub(crate) const SHELLS: usize = 8;
/// How many lines of a command show: the running one is what the human
/// must be able to read, the finished ones only to recognise.
pub(crate) const RUNNING_LINES: usize = 8;
pub(crate) const DONE_LINES: usize = 2;
/// The shell section's room above its header, the header, the padding
/// round a command's well and the room between commands, in DIPs.
pub(crate) const SHELLS_GAP: f32 = 14.0;
pub(crate) const FOLD_H: f32 = 22.0;
pub(crate) const WELL_PAD: f32 = 3.0;
pub(crate) const SHELL_GAP: f32 = 6.0;
/// A command's width in its well, and how much of it is laid out at all:
/// a script written inline can be thousands of lines, of which the plate
/// shows eight.
pub(crate) const COMMAND_W: f32 = BOX_W - 2.0 * PAD - 12.0;
pub(crate) const COMMAND_CHARS: usize = 4000;

// More glyphs of the system's icon font.
pub(crate) const RUNNING: char = '\u{E768}';
pub(crate) const CLOCK: char = '\u{E823}';
pub(crate) const OPEN: char = '\u{E70D}';
pub(crate) const FOLDED: char = '\u{E76C}';

/// Where a shell command the plate lists stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShellState {
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
    /// The human, or a rule of theirs, said no before it ran.
    Denied,
    /// Its turn ended without its end ever coming: cancelled.
    Stopped,
}

impl ShellState {
    /// The state in a word or two, beside the command.
    pub(crate) fn word(self) -> String {
        match self {
            ShellState::Running => "running".into(),
            ShellState::Done => "done".into(),
            ShellState::Failed(Some(code)) => format!("exit {code}"),
            ShellState::Failed(None) => "failed".into(),
            ShellState::Interrupted => "interrupted".into(),
            ShellState::TimedOut => "timed out, in background".into(),
            ShellState::Background => "in background".into(),
            ShellState::Denied => "denied, not run".into(),
            ShellState::Stopped => "stopped".into(),
        }
    }

    pub(crate) fn glyph(self) -> char {
        match self {
            ShellState::Running => RUNNING,
            ShellState::Done => CHECK,
            ShellState::Failed(_) => ERROR,
            ShellState::Interrupted => STOP,
            ShellState::TimedOut | ShellState::Background => CLOCK,
            ShellState::Denied | ShellState::Stopped => CROSS,
        }
    }

    pub(crate) fn colour(self) -> Color {
        match self {
            ShellState::Running => theme::working(),
            ShellState::Done => theme::done(),
            ShellState::Failed(_) | ShellState::Interrupted => theme::error(),
            ShellState::TimedOut | ShellState::Background => theme::waiting(),
            ShellState::Denied => theme::waiting(),
            ShellState::Stopped => theme::legend(),
        }
    }

    /// How many lines of its command the plate shows.
    pub(crate) fn lines(self) -> usize {
        match self {
            ShellState::Running => RUNNING_LINES,
            _ => DONE_LINES,
        }
    }
}

/// One shell command a session ran while control was on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Shell {
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
    /// Its number in this run of the app, which keys its laid out text.
    pub serial: u64,
}

impl Shell {
    /// What the row says the command is for: its description, else which
    /// shell it ran in.
    pub(crate) fn title(&self) -> String {
        match self.description.trim() {
            "" => format!("{} command", self.tool),
            d => d.to_string(),
        }
    }
}

/// What the hook thread heard of a shell command, for the overlay.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ShellNote {
    Began {
        session: String,
        shell: Shell,
    },
    Ended {
        session: String,
        /// Claude Code's id for the call; empty when it gave none.
        id: String,
        /// `Bash` or `PowerShell`, and the command, to pair an end that
        /// has no id.
        tool: String,
        command: String,
        state: ShellState,
    },
    /// The session's turn ended, or a new prompt began one: whatever of
    /// its commands still runs never got its end.
    TurnEnded {
        session: String,
    },
}

impl ShellNote {
    pub(crate) fn session(&self) -> &str {
        match self {
            ShellNote::Began { session, .. }
            | ShellNote::Ended { session, .. }
            | ShellNote::TurnEnded { session } => session,
        }
    }
}

/// Whether a tool runs commands in a shell, under any MCP prefix.
pub(crate) fn is_shell_tool(tool: &str) -> bool {
    matches!(bare_tool(tool), "Bash" | "PowerShell")
}

fn bare_tool(tool: &str) -> &str {
    tool.rsplit("__").next().unwrap_or(tool)
}

/// How a shell command ended, from its `PostToolUse` (`error` none) or
/// its `PostToolUseFailure`.
pub(crate) fn shell_ended(
    tool: &str,
    input: &serde_json::Value,
    response: &serde_json::Value,
    error: Option<&str>,
    interrupt: bool,
) -> ShellState {
    if interrupt {
        return ShellState::Interrupted;
    }
    let bare = bare_tool(tool);
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
/// wall clock `at`, or of the end of its turn. None for anything else.
pub(crate) fn shell_note(target: &str, e: &HookEvent, at: &str) -> Option<ShellNote> {
    let session = target.to_string();
    let turn_edge = matches!(
        e.hook_event_name.as_str(),
        "Stop" | "StopFailure" | "SessionEnd" | "UserPromptSubmit"
    );
    if turn_edge && !e.is_subagent() {
        return Some(ShellNote::TurnEnded { session });
    }
    let tool = e.tool_name.as_deref().filter(|t| is_shell_tool(t))?;
    let id = e.tool_use_id.clone().unwrap_or_default();
    let input = e.tool_input.clone().unwrap_or_default();
    let text = |key: &str| input[key].as_str().unwrap_or_default().to_string();
    match e.hook_event_name.as_str() {
        "PreToolUse" => Some(ShellNote::Began {
            session,
            shell: Shell {
                id,
                tool: bare_tool(tool).to_string(),
                command: text("command"),
                description: text("description"),
                agent: e.is_subagent(),
                at: at.to_string(),
                state: ShellState::Running,
                serial: 0,
            },
        }),
        // Asked and refused: it never ran, and its end is that, not the
        // "stopped" its turn's end would give it.
        "PermissionDenied" => Some(ShellNote::Ended {
            session,
            id,
            tool: bare_tool(tool).to_string(),
            command: text("command"),
            state: ShellState::Denied,
        }),
        "PostToolUse" | "PostToolUseFailure" => Some(ShellNote::Ended {
            session,
            id,
            tool: bare_tool(tool).to_string(),
            command: text("command"),
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

/// How many commands of one session the log can owe an end at once. Past
/// it the oldest is given up, so a session that never ends its commands
/// can not grow the ledger without bound.
pub(crate) const LEDGER: usize = 64;

/// The commands begun under control whose end the log is still owed, by
/// session, oldest first. Apart from the plate's lists: it is neither cut
/// at [`SHELLS`] nor emptied when control rests.
pub(crate) type Ledger = HashMap<String, Vec<Shell>>;

/// Takes a note into the ledger and the sessions' lists, and gives back
/// the commands it began or ended, for the log. An end is paired by its
/// id, or without one with the oldest running command of its shell that
/// ran the same, else the oldest of its shell; an end whose start was
/// never heard, one begun before control, is dropped. `list` is false for
/// a session control no longer runs for: its ends are still logged, and
/// nothing of it is listed or begun.
pub(crate) fn hear_shell(
    lists: &mut HashMap<String, VecDeque<Shell>>,
    ledger: &mut Ledger,
    note: ShellNote,
    list: bool,
) -> Vec<Shell> {
    let mut show = |session: &str, shell: &Shell| {
        let listed = lists
            .get_mut(session)
            .and_then(|l| l.iter_mut().find(|s| s.serial == shell.serial));
        if let Some(s) = listed {
            s.state = shell.state;
        }
    };
    match note {
        ShellNote::Began { .. } if !list => Vec::new(),
        ShellNote::Began { session, shell } => {
            let owed = ledger.entry(session.clone()).or_default();
            owed.push(shell.clone());
            if owed.len() > LEDGER {
                owed.remove(0);
            }
            let list = lists.entry(session).or_default();
            list.push_front(shell.clone());
            list.truncate(SHELLS);
            vec![shell]
        }
        ShellNote::Ended {
            session,
            id,
            tool,
            command,
            state,
        } => {
            let Some(owed) = ledger.get_mut(&session) else {
                return Vec::new();
            };
            let at = if id.is_empty() {
                let mine = |s: &Shell| s.tool == tool;
                owed.iter()
                    .position(|s| mine(s) && s.command == command)
                    .or_else(|| owed.iter().position(mine))
            } else {
                owed.iter().position(|s| s.id == id)
            };
            let Some(at) = at else {
                return Vec::new();
            };
            let mut shell = owed.remove(at);
            if owed.is_empty() {
                ledger.remove(&session);
            }
            shell.state = state;
            show(&session, &shell);
            vec![shell]
        }
        ShellNote::TurnEnded { session } => {
            let mut ended = ledger.remove(&session).unwrap_or_default();
            for shell in &mut ended {
                shell.state = ShellState::Stopped;
                show(&session, shell);
            }
            ended
        }
    }
}

/// The fold's header: how many commands, and how many still run.
pub(crate) fn shells_count(list: &VecDeque<Shell>) -> String {
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

/// How many lines a whole command has, from the lines of the part laid
/// out: the rest of it adds its line breaks.
pub(crate) fn command_lines(laid: usize, command: &str) -> usize {
    let rest = &command[command_shown(command).len()..];
    laid.max(1) + rest.matches('\n').count()
}

/// The tag on a command cut short, none when it shows whole.
pub(crate) fn more_lines(hidden: usize) -> Option<String> {
    match hidden {
        0 => None,
        1 => Some("+1 line".into()),
        n => Some(format!("+{n} lines")),
    }
}

/// The start of a command, as much as is laid out and logged.
pub(crate) fn command_shown(command: &str) -> &str {
    match command.char_indices().nth(COMMAND_CHARS) {
        Some((at, _)) => &command[..at],
        None => command,
    }
}

/// One listed command's whole row, `lines` of its command shown.
pub(crate) fn shell_row(lines: usize, line_h: f32) -> f32 {
    ROW + lines as f32 * line_h + 2.0 * WELL_PAD + SHELL_GAP
}

/// How the shell section fits the room it has.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Fit {
    /// Lines shown of each command shown, newest first.
    pub shown: Vec<usize>,
    /// Commands left out for want of room.
    pub more: usize,
    /// A line under them: there are none yet, or how many are left out.
    pub note: bool,
}

/// Fits the commands, newest first, each its lines and state, into `room`
/// DIPs under the header, so the plate stays on the screen. A running
/// command gives up lines, down to what a finished one shows, before any
/// command is left out, and what is left out is counted.
pub(crate) fn fit_shells(rows: &[(usize, ShellState)], line_h: f32, room: f32) -> Fit {
    let room = room - SHELLS_GAP - FOLD_H;
    let height = |shown: &[usize]| shown.iter().map(|&l| shell_row(l, line_h)).sum::<f32>();
    if rows.is_empty() {
        return Fit {
            note: room >= ROW,
            ..Fit::default()
        };
    }
    let full: Vec<usize> = rows.iter().map(|&(l, s)| l.clamp(1, s.lines())).collect();
    if height(&full) <= room {
        return Fit {
            shown: full,
            ..Fit::default()
        };
    }
    let mut shown: Vec<usize> = rows.iter().map(|&(l, _)| l.clamp(1, DONE_LINES)).collect();
    if height(&shown) > room {
        let mut k = shown.len();
        while k > 0 && height(&shown[..k]) + ROW > room {
            k -= 1;
        }
        shown.truncate(k);
    }
    // What room is left goes back to the running ones, newest first.
    let mut left = room - height(&shown);
    let more = rows.len() - shown.len();
    if more > 0 {
        left -= ROW;
    }
    for (lines, &want) in shown.iter_mut().zip(&full) {
        let extra = (want - *lines).min((left / line_h).floor().max(0.0) as usize);
        *lines += extra;
        left -= extra as f32 * line_h;
    }
    Fit {
        shown,
        more,
        note: more > 0 && room >= ROW,
    }
}

/// The shell section's height: its header, and while open what `fit`
/// shows, `line_h` a command's line.
pub(crate) fn shells_height(open: bool, fit: &Fit, line_h: f32) -> f32 {
    let rows = if open {
        fit.shown.iter().map(|&l| shell_row(l, line_h)).sum::<f32>()
            + if fit.note { ROW } else { 0.0 }
    } else {
        0.0
    };
    SHELLS_GAP + FOLD_H + rows
}

/// The line under the commands: none yet, or how many are left out.
pub(crate) fn shells_note(fit: &Fit) -> String {
    match fit.more {
        0 => "No shell command since control began".into(),
        n => format!("{n} more not shown, no room on the screen"),
    }
}

/// How long a command's end line in the log quotes it: the start line
/// has it whole, as far as [`COMMAND_CHARS`].
pub(crate) const LOG_END_CHARS: usize = 80;

/// One line of the desktop log for a shell command: when, which session,
/// what happened, in which shell, the command and what it is for. Its
/// start gives the command, its end only enough of it to find the start.
pub(crate) fn shell_log_line(utc: &str, session: &str, shell: &Shell) -> String {
    let flat = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let from = if shell.agent { " by a subagent" } else { "" };
    let (how, command) = match shell.state {
        ShellState::Running => (
            format!("shell command started{from}"),
            flat(command_shown(&shell.command)),
        ),
        state => {
            let all = flat(&shell.command);
            let short = match all.char_indices().nth(LOG_END_CHARS) {
                Some((at, _)) => format!("{}...", &all[..at]),
                None => all,
            };
            (format!("shell command {}{from}", state.word()), short)
        }
    };
    let mut line = format!(
        "{utc}  {}  {how}  {}  ({command})",
        flat(session),
        shell.tool
    );
    if !shell.description.trim().is_empty() {
        line.push_str(&format!("  [{}]", flat(&shell.description)));
    }
    line.push('\n');
    line
}

/// The sessions that hold control in this run of it, those of an earlier
/// run whose commands still run, and what the hook thread heard of their
/// shells, kept under one lock so a run's end moves them at once.
struct ShellWatch {
    holders: Vec<String>,
    /// Control rested with a command of theirs still running: its end is
    /// still heard, for the log, and nothing new of theirs is.
    trailing: Vec<String>,
    notes: Vec<ShellNote>,
}

static SHELL_WATCH: Mutex<ShellWatch> = Mutex::new(ShellWatch {
    holders: Vec::new(),
    trailing: Vec::new(),
    notes: Vec::new(),
});
/// Each command's number, for its laid out text.
static SHELL_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A session holds control from its first allowed desktop call, or from
/// bypass turned on for it: its shell commands are listed from then.
pub(crate) fn hold_shells(session: &str) {
    if session.is_empty() {
        return;
    }
    if let Ok(mut w) = SHELL_WATCH.lock() {
        w.trailing.retain(|t| t != session);
        if !w.holders.iter().any(|h| h == session) {
            w.holders.push(session.to_string());
        }
    }
}

/// Control's run is over: nobody holds it. The sessions in `owed` still
/// have commands running, whose ends the log is owed.
pub(crate) fn release_shells(owed: Vec<String>) {
    if let Ok(mut w) = SHELL_WATCH.lock() {
        w.holders.clear();
        w.trailing = owed;
    }
}

/// Whether a session's shell events are wanted, and whether only their
/// ends are.
fn wanted(target: &str) -> Option<bool> {
    let w = SHELL_WATCH.lock().ok()?;
    if w.holders.iter().any(|h| h == target) {
        Some(false)
    } else if w.trailing.iter().any(|t| t == target) {
        Some(true)
    } else {
        None
    }
}

/// A hook event for session `target`, from the thread that reads hooks:
/// a shell command's start or end, or its turn's end, is kept for the
/// plate and the log when the session holds control. Every session's
/// every hook comes by here, so who holds control is asked first, and
/// nothing is read out of an event nobody wants.
pub(crate) fn shell_heard(target: &str, e: &HookEvent) {
    let Some(ends_only) = wanted(target) else {
        return;
    };
    let Some(mut note) = shell_note(target, e, &super::now_clock()) else {
        return;
    };
    match &mut note {
        ShellNote::Began { .. } if ends_only => return,
        ShellNote::Began { shell, .. } => {
            shell.serial = SHELL_SERIAL.fetch_add(1, Ordering::Relaxed);
        }
        _ => {}
    }
    if let Ok(mut w) = SHELL_WATCH.lock() {
        // Asked again: control may have rested while the note was made.
        if w.holders.iter().chain(&w.trailing).any(|h| h == target) {
            w.notes.push(note);
        }
    }
    // No frame runs while nothing on screen moves, and none at all once
    // control has rested, so the overlay is told a note waits.
    super::wake();
}

pub(crate) fn take_shell_notes() -> Vec<ShellNote> {
    SHELL_WATCH
        .lock()
        .map(|mut w| std::mem::take(&mut w.notes))
        .unwrap_or_default()
}

/// Whether control runs for a session now, so its commands are listed.
pub(crate) fn holds_shells(session: &str) -> bool {
    SHELL_WATCH
        .lock()
        .is_ok_and(|w| w.holders.iter().any(|h| h == session))
}

/// Appends to `takeover.log` on a thread of its own, in the order given,
/// so the overlay's thread, which is the app's, never waits on the disk.
pub(crate) fn log_lines(lines: String) {
    static WRITER: Mutex<Option<std::sync::mpsc::Sender<String>>> = Mutex::new(None);
    let Ok(mut writer) = WRITER.lock() else {
        return;
    };
    let tx = writer.get_or_insert_with(|| {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            for lines in rx {
                let Some(file) = horadric_hooks::tasks::desktop_log_file() else {
                    continue;
                };
                if let Some(dir) = file.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                // Kept under its size as every other line of the log is.
                crate::screen_share::roll(&file);
                let _ = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&file)
                    .and_then(|mut f| std::io::Write::write_all(&mut f, lines.as_bytes()));
            }
        });
        tx
    });
    let _ = tx.send(lines);
}
