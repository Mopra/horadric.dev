//! Watching the desktop programs a session works with, so an agent can ask
//! cheaply what changed, or wait for something, instead of taking a new
//! snapshot or picture each time.
//!
//! A thread of its own holds WinEvent hooks, out of context, on each
//! program this session has looked at or operated, and writes what
//! happens into a ring of the last few hundred changes: windows and
//! dialogs shown and closed, a window coming to the front, menus, and
//! elements whose name, value or state changed. The hooks only queue the
//! event: the element is read later on the same thread, after a burst is
//! folded, so a program that fires a thousand events reads as one change.
//!
//! What holds, and what the rest leans on:
//!
//! - One process serves one session. There is one ring, one set of
//!   followed programs and one `handed` mark for the whole process, and
//!   the places a session has read up to are kept by its `Takeover`
//!   (`seen`). Whatever is in the ring may be told to whoever asks in
//!   this process. Should this ever be hosted in a process that serves
//!   several sessions, the ring, the followed set, the mark and the
//!   grants must each become one per session first, or one session reads
//!   what only another was allowed to see.
//! - A program is watched only while its grant holds. `follow` is called
//!   after the app said yes, the caller asks the app again before telling
//!   changes it did not name the window for, and `unfollow` unhooks the
//!   program and drops what the ring kept of it.
//! - A hook is set by a program's id, so the id is pinned: a process
//!   handle is held from `follow` until the program is unhooked, and the
//!   thread looks each round whether it ended.
//! - Reading an element is a call into the watched program, with no time
//!   limit of its own, on the one watching thread. So what is worth a
//!   read is settled before reading, a round reads for a limited time,
//!   and the thread's heartbeat lets every answer say that the watcher is
//!   stuck instead of that nothing changed.
//! - A protected element's value (a password) is never read or kept.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::SystemInformation::{GetLocalTime, GetTickCount};
use windows::Win32::System::Threading::{
    GetCurrentThreadId, GetExitCodeProcess, OpenProcess, WaitForSingleObject,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    AccessibleObjectFromEvent, IAccessible, SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetClassNameW, GetForegroundWindow, GetMessageW,
    GetWindowThreadProcessId, IsHungAppWindow, IsWindowVisible, PeekMessageW, PostThreadMessageW,
    SetTimer, GA_ROOT, MSG, PM_NOREMOVE, WM_APP,
};

use crate::takeover::{info, short, Window};

/// How many changes are kept. An agent asks every few seconds, and a
/// busy program makes a few a second once folded.
pub const KEPT: usize = 500;

/// Changes of the same element closer together than this are one change,
/// from the first value to the last.
pub const FOLD_MS: u64 = 1000;

/// The most changes recorded in one second; past it they are counted and
/// said as skipped, so a program in a storm does not wipe the ring.
pub const MOST_PER_SECOND: u32 = 60;

/// The most lines one answer holds.
pub const MOST_LINES: usize = 150;

// The WinEvents watched, by number, as winuser.h has them.
pub const EVENT_FOREGROUND: u32 = 0x0003;
pub const EVENT_MENUSTART: u32 = 0x0004;
pub const EVENT_MENUEND: u32 = 0x0005;
pub const EVENT_MENUPOPUPSTART: u32 = 0x0006;
pub const EVENT_MENUPOPUPEND: u32 = 0x0007;
pub const EVENT_DIALOGSTART: u32 = 0x0010;
pub const EVENT_DIALOGEND: u32 = 0x0011;
pub const EVENT_DESTROY: u32 = 0x8001;
pub const EVENT_SHOW: u32 = 0x8002;
pub const EVENT_HIDE: u32 = 0x8003;
pub const EVENT_STATECHANGE: u32 = 0x800A;
pub const EVENT_NAMECHANGE: u32 = 0x800C;
pub const EVENT_VALUECHANGE: u32 = 0x800E;

/// The ranges hooked. Caret, location, reorder and selection events lie
/// between them and are left out: they come in floods and say little.
const HOOKED: [(u32, u32); 6] = [
    (EVENT_FOREGROUND, EVENT_MENUPOPUPEND),
    (EVENT_DIALOGSTART, EVENT_DIALOGEND),
    (EVENT_DESTROY, EVENT_HIDE),
    (EVENT_STATECHANGE, EVENT_STATECHANGE),
    (EVENT_NAMECHANGE, EVENT_NAMECHANGE),
    (EVENT_VALUECHANGE, EVENT_VALUECHANGE),
];

const OBJID_WINDOW: i32 = 0;
const OBJID_CLIENT: i32 = -4;
const CHILDID_SELF: i32 = 0;
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;

// The MSAA states a change of is worth telling.
const STATE_UNAVAILABLE: u32 = 0x1;
const STATE_SELECTED: u32 = 0x2;
const STATE_CHECKED: u32 = 0x10;
const STATE_MIXED: u32 = 0x20;
const STATE_READONLY: u32 = 0x40;
const STATE_EXPANDED: u32 = 0x200;
const STATE_COLLAPSED: u32 = 0x400;
const STATE_BUSY: u32 = 0x800;
/// The element holds a password or the like, and its value is not read.
const STATE_PROTECTED: u32 = 0x2000_0000;

/// The watching thread comes round ten times a second. Silent for this
/// long, it is stuck in a read of a program that does not answer.
pub const STALLED_MS: u64 = 2000;

/// What happened.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum What {
    Shown,
    Closed,
    Front,
    Title,
    MenuOpened,
    MenuClosed,
    Name,
    Value,
    State,
    /// This many changes were not recorded, too many came at once.
    Skipped(u32),
}

/// One change, as the ring keeps it.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    /// Rising with each change recorded, for asking what came after.
    pub seq: u64,
    /// When, in milliseconds since watching began, for folding and quiet.
    pub at_ms: u64,
    /// When, as seconds since local midnight, for the human reading.
    pub clock: u32,
    pub what: What,
    pub pid: u32,
    pub exe: String,
    /// The top level window it happened in, or is.
    pub window: isize,
    pub title: String,
    /// For a window: whether it is a dialog.
    pub dialog: bool,
    /// The element's type and name, empty for a window's own change.
    pub role: String,
    pub name: String,
    /// The element: its window, object and child, as the event names it.
    pub key: Option<(isize, i32, i32)>,
    /// The value or name before, when it was known.
    pub from: Option<String>,
    /// The value or name after.
    pub to: String,
    /// For a state change: the states before, when known, and after.
    pub states: Option<(Option<u32>, u32)>,
    /// The value changed but is protected (a password), so it was not
    /// read: `from` and `to` say nothing.
    pub hidden: bool,
}

impl Change {
    /// A change of `what` in `w`, the rest left empty.
    pub fn of(what: What, w: &Window) -> Change {
        Change {
            seq: 0,
            at_ms: 0,
            clock: 0,
            what,
            pid: w.pid,
            exe: w.exe.clone(),
            window: w.hwnd,
            title: w.title.clone(),
            dialog: w.owner.is_some() || w.class == "#32770",
            role: String::new(),
            name: String::new(),
            key: None,
            from: None,
            to: String::new(),
            states: None,
            hidden: false,
        }
    }

    /// The note that `n` changes were not recorded. It is of no program
    /// and no window, so nothing it carries can be taken for a change.
    pub fn skipped(n: u32) -> Change {
        Change {
            seq: 0,
            at_ms: 0,
            clock: 0,
            what: What::Skipped(n),
            pid: 0,
            exe: String::new(),
            window: 0,
            title: String::new(),
            dialog: false,
            role: String::new(),
            name: String::new(),
            key: None,
            from: None,
            to: String::new(),
            states: None,
            hidden: false,
        }
    }

    /// The text this change brought: a value, name or title it changed
    /// to, or the title of a window shown. What merely is in a change
    /// (the window it happened in, the element's name beside a new
    /// state) did not come with it.
    pub fn brought(&self) -> Option<&str> {
        match self.what {
            What::Value | What::Name | What::Title if !self.hidden => Some(&self.to),
            What::Shown => Some(&self.title),
            _ => None,
        }
    }

    fn folds(&self) -> bool {
        matches!(
            self.what,
            What::Front | What::Title | What::Name | What::Value | What::State
        )
    }
}

/// The last changes, oldest first.
#[derive(Debug)]
pub struct Ring {
    items: VecDeque<Change>,
    next: u64,
    most: usize,
    /// Every change up to this one has been pushed out.
    lost_upto: u64,
    /// Every change up to this one may have been read by an agent, so it
    /// stays as it was told: folding it away would leave the reader with a
    /// change that was undone.
    handed: u64,
}

impl Ring {
    pub fn new(most: usize) -> Ring {
        Ring {
            items: VecDeque::new(),
            next: 1,
            most,
            lost_upto: 0,
            handed: 0,
        }
    }

    /// Notes that every change up to `upto` has gone to a reader.
    pub fn hand_out(&mut self, upto: u64) {
        self.handed = self.handed.max(upto);
    }

    /// The last change recorded, 0 before any.
    pub fn newest(&self) -> u64 {
        self.next - 1
    }

    /// Records `c`, folded into an earlier change of the same element when
    /// they come close together and no reader has had that one yet.
    /// Whether anything new is there to read.
    pub fn push(&mut self, mut c: Change) -> bool {
        // A count of what was skipped is never the same news twice.
        if matches!(c.what, What::Skipped(_)) {
            self.append(c);
            return true;
        }
        // The front moves through several windows in a moment (a dialog
        // closing hands it back); only where it came to rest matters.
        let anywhere = c.what == What::Front;
        let near = |e: &Change| {
            e.what == c.what
                && (anywhere || e.window == c.window)
                && e.key == c.key
                && c.at_ms.saturating_sub(e.at_ms) <= FOLD_MS
        };
        let back = self.items.len().saturating_sub(30);
        if let Some(i) = (back..self.items.len())
            .rev()
            .find(|&i| near(&self.items[i]))
        {
            let same = &self.items[i];
            // A window closed in between makes a second showing news.
            let since_touched = !c.folds()
                && self
                    .items
                    .range(i + 1..)
                    .any(|later| later.window == c.window);
            if same.window == c.window
                && same.to == c.to
                && same.states == c.states
                && !since_touched
            {
                return false;
            }
            if c.folds() && same.seq > self.handed {
                let old = self.items.remove(i).expect("found just above");
                c.from = old.from;
                if let (Some((first, _)), Some((_, now))) = (old.states, c.states) {
                    c.states = Some((first, now));
                }
                let back_where_it_was = match c.states {
                    Some((Some(a), b)) => state_change(Some(a), b).is_none(),
                    Some((None, _)) => false,
                    None => c.from.as_deref() == Some(c.to.as_str()),
                };
                if back_where_it_was {
                    return true;
                }
            }
        }
        self.append(c);
        true
    }

    fn append(&mut self, mut c: Change) {
        c.seq = self.next;
        self.next += 1;
        self.items.push_back(c);
        while self.items.len() > self.most {
            if let Some(gone) = self.items.pop_front() {
                self.lost_upto = gone.seq;
            }
        }
    }

    /// Drops what is kept of program `pid`, which may no longer be
    /// looked at. The numbering goes on, so no reader's place moves.
    pub fn forget(&mut self, pid: u32) {
        self.items.retain(|c| c.pid != pid);
    }

    /// The changes after `seq`, and whether some that came after it have
    /// been pushed out already.
    pub fn since(&self, seq: u64) -> (Vec<Change>, bool) {
        let list = self.items.iter().filter(|c| c.seq > seq).cloned().collect();
        (list, self.lost_upto > seq)
    }
}

/// Counts what comes in each second, so a storm is cut short.
#[derive(Debug, Default)]
pub struct Intake {
    second: u64,
    count: u32,
    skipped: u32,
    /// The second the last of the skipped came in.
    skipped_in: u64,
}

impl Intake {
    /// Whether an element's change that happened at `at_ms` is worth
    /// reading. Asked before the read, which is the costly part: what is
    /// turned away is only counted.
    pub fn admit(&mut self, at_ms: u64) -> bool {
        // Events are read a little out of order; time does not go back.
        let second = (at_ms / 1000).max(self.second);
        if second != self.second {
            self.second = second;
            self.count = 0;
        }
        if self.count >= MOST_PER_SECOND {
            self.lose(1);
            return false;
        }
        self.count += 1;
        true
    }

    /// Counts `n` changes lost before they could be asked about, the
    /// queue being full.
    pub fn lose(&mut self, n: u32) {
        if n > 0 {
            self.skipped = self.skipped.saturating_add(n);
            self.skipped_in = self.second;
        }
    }

    /// How many were skipped and not said yet, once the second they came
    /// in is over at `now_ms` or nothing more is coming (`idle`): said
    /// without waiting for a later change, which may never come.
    pub fn take_skipped(&mut self, now_ms: u64, idle: bool) -> Option<u32> {
        if self.skipped == 0 || !(idle || now_ms / 1000 > self.skipped_in) {
            return None;
        }
        Some(std::mem::take(&mut self.skipped))
    }
}

/// An element, as an event names it: its window, object and child.
pub type Key = (isize, i32, i32);

/// What elements last held, for telling a change from what it was. Kept
/// to a limit by forgetting the ones longest untouched: forgetting all
/// at once would make the next change of every element look like a first
/// sight, and a first name is mostly not told.
#[derive(Debug)]
pub struct Recall<V> {
    /// Each with its program and when it was last set.
    items: HashMap<Key, (V, u32, u64)>,
    clock: u64,
    most: usize,
}

impl<V> Recall<V> {
    pub fn new(most: usize) -> Recall<V> {
        Recall {
            items: HashMap::new(),
            clock: 0,
            most,
        }
    }

    /// Sets what `key`, an element of program `pid`, holds, and gives
    /// what it held before.
    pub fn insert(&mut self, key: Key, pid: u32, v: V) -> Option<V> {
        self.clock += 1;
        let before = self.items.insert(key, (v, pid, self.clock)).map(|e| e.0);
        if self.items.len() > self.most {
            // A quarter at once, so this is not done on every insert.
            let mut touched: Vec<u64> = self.items.values().map(|e| e.2).collect();
            let cut = (touched.len() / 4).max(1);
            let (_, line, _) = touched.select_nth_unstable(cut);
            let line = *line;
            self.items.retain(|_, e| e.2 >= line);
        }
        before
    }

    pub fn remove(&mut self, key: &Key) {
        self.items.remove(key);
    }

    /// Forgets the elements of program `pid`.
    pub fn forget(&mut self, pid: u32) {
        self.items.retain(|_, e| e.1 != pid);
    }

    // Only the tests count what is held.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// An MSAA role as the snapshot's control types name it.
pub fn role_name(role: i32) -> &'static str {
    match role {
        1 => "TitleBar",
        2 => "MenuBar",
        3 => "ScrollBar",
        9 => "Window",
        10 => "Pane",
        11 => "Menu",
        12 => "MenuItem",
        13 => "ToolTip",
        15 => "Document",
        16 => "Pane",
        18 => "Dialog",
        20 => "Group",
        22 => "ToolBar",
        23 => "StatusBar",
        24 => "Table",
        25 | 26 => "HeaderItem",
        28 => "Row",
        29 => "DataItem",
        30 => "Hyperlink",
        33 => "List",
        34 => "ListItem",
        35 => "Tree",
        36 => "TreeItem",
        37 => "TabItem",
        40 => "Image",
        41 => "Text",
        42 => "Edit",
        43 => "Button",
        44 => "CheckBox",
        45 => "RadioButton",
        46 => "ComboBox",
        48 => "ProgressBar",
        51 => "Slider",
        52 => "Spinner",
        56 | 57 | 62 => "SplitButton",
        60 => "Tab",
        _ => "Element",
    }
}

/// The states worth telling, in words.
fn state_words(state: u32) -> Vec<&'static str> {
    [
        (STATE_CHECKED, "checked"),
        (STATE_MIXED, "partly checked"),
        (STATE_SELECTED, "selected"),
        (STATE_EXPANDED, "expanded"),
        (STATE_COLLAPSED, "collapsed"),
        (STATE_UNAVAILABLE, "disabled"),
        (STATE_READONLY, "read only"),
        (STATE_BUSY, "busy"),
    ]
    .into_iter()
    .filter(|(bit, _)| state & bit != 0)
    .map(|(_, w)| w)
    .collect()
}

/// What changed between two states, in words, none when nothing worth
/// telling did (focus and hover come and go all the time). With no state
/// known before, only selection is left out, as it moves with the focus.
pub fn state_change(before: Option<u32>, now: u32) -> Option<String> {
    let mut said = Vec::new();
    match before {
        Some(b) => {
            let (was, is) = (state_words(b), state_words(now));
            let on: Vec<_> = is.iter().filter(|w| !was.contains(w)).collect();
            let off: Vec<_> = was.iter().filter(|w| !is.contains(w)).collect();
            if !on.is_empty() {
                said.push(format!(
                    "now {}",
                    on.iter().map(|w| **w).collect::<Vec<_>>().join(", ")
                ));
            }
            if !off.is_empty() {
                said.push(format!(
                    "no longer {}",
                    off.iter().map(|w| **w).collect::<Vec<_>>().join(", ")
                ));
            }
        }
        None => {
            let is: Vec<_> = state_words(now)
                .into_iter()
                .filter(|w| *w != "selected")
                .collect();
            if !is.is_empty() {
                said.push(format!("now {}", is.join(", ")));
            }
        }
    }
    (!said.is_empty()).then(|| said.join(", "))
}

/// A time of day as a person reads it.
pub fn clock_text(secs: u32) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600 % 24,
        secs / 60 % 60,
        secs % 60
    )
}

/// A change in a few words, without its time.
pub fn brief(c: &Change) -> String {
    let title = short(&c.title, 50);
    let kind = if c.dialog { "dialog" } else { "window" };
    let element = || {
        if c.name.is_empty() {
            c.role.clone()
        } else {
            format!("{} {:?}", c.role, short(&c.name, 50))
        }
    };
    let value = |v: &str| format!("{:?}", short(v, 80));
    match c.what {
        What::Shown => format!("{kind} shown {title:?} id={} ({})", c.window, c.exe),
        What::Closed => format!("{kind} closed {title:?} id={}", c.window),
        What::Front => format!("{title:?} id={} came to the front", c.window),
        What::Title => match &c.from {
            Some(f) => format!(
                "window id={} title {} -> {}",
                c.window,
                value(f),
                value(&c.to)
            ),
            None => format!("window id={} title now {}", c.window, value(&c.to)),
        },
        What::MenuOpened => format!("menu opened in {title:?}"),
        What::MenuClosed => format!("menu closed in {title:?}"),
        What::Name => match &c.from {
            Some(f) => format!(
                "{} named {} -> {} in {title:?}",
                c.role,
                value(f),
                value(&c.to)
            ),
            None => format!("{} now named {} in {title:?}", c.role, value(&c.to)),
        },
        What::Value if c.hidden => format!(
            "{} changed in {title:?} (its value is protected and was not read)",
            element()
        ),
        What::Value if c.role.is_empty() => {
            format!("something changed its value in {title:?}")
        }
        What::Value => match &c.from {
            Some(f) => format!(
                "{} value {} -> {} in {title:?}",
                element(),
                value(f),
                value(&c.to)
            ),
            None => format!("{} value now {} in {title:?}", element(), value(&c.to)),
        },
        What::State => {
            let (before, now) = c.states.unwrap_or((None, 0));
            let how = state_change(before, now).unwrap_or_else(|| "changed state".into());
            format!("{} {how} in {title:?}", element())
        }
        What::Skipped(n) => format!("{n} more changes not recorded, too many came at once"),
    }
}

/// A change as one line of an answer.
pub fn line(c: &Change) -> String {
    format!("{} {}", clock_text(c.clock), brief(c))
}

/// Whether `c` is in the program `pid` names, or any when none.
pub fn in_scope(c: &Change, pid: Option<u32>) -> bool {
    pid.is_none_or(|p| c.pid == p || matches!(c.what, What::Skipped(_)))
}

/// For how many seconds the watching thread has been silent at `now_ms`,
/// having last come round at `beat_ms`, when that is long enough to mean
/// it is stuck.
pub fn stall(beat_ms: u64, now_ms: u64) -> Option<u64> {
    let silent = now_ms.saturating_sub(beat_ms);
    (silent >= STALLED_MS).then_some(silent / 1000)
}

/// What is said in place of "nothing changed" while the watcher is stuck.
pub fn stall_text(secs: u64) -> String {
    format!("the watcher has not answered for {secs} s (a watched program may be hung)")
}

/// Where a `takeover_changes` reads from: the change the agent `asked`
/// after, else the place the session `kept`. A number past the `newest`
/// change is from before `horadric mcp` started again, when the count
/// began anew: all that is kept is told then, with a note saying why.
pub fn cursor_from(asked: Option<u64>, kept: u64, newest: u64) -> (u64, Option<String>) {
    match asked {
        Some(a) if a > newest => (
            0,
            Some(format!(
                "(There is no change #{a} yet: the numbering started over, as it does when \
                 horadric mcp starts again, and stands at #{newest}. Everything kept is told.)\n"
            )),
        ),
        Some(a) => (a, None),
        None => (kept.min(newest), None),
    }
}

/// What a wait read from the session's place, in two: the changes up to
/// `mark`, where the wait began, which came before it and were not told
/// yet, and those after. Only the second decide what the wait found. A
/// change folded into a later one has the later one's number, so one that
/// began before the wait and went on during it counts as during.
pub fn split_at(list: Vec<Change>, mark: u64) -> (Vec<Change>, Vec<Change>) {
    list.into_iter().partition(|c| c.seq <= mark)
}

/// What a wait tells beside its verdict: what changed `before` it that
/// the session had not been told, and what changed during it, `after`.
/// Empty when there is nothing to tell and `quietly` is set.
pub fn meanwhile_text(
    before: &[Change],
    after: &[Change],
    lost: bool,
    stalled: Option<u64>,
    quietly: bool,
) -> String {
    if quietly && before.is_empty() && after.is_empty() && !lost && stalled.is_none() {
        return String::new();
    }
    let mut out = String::new();
    if lost {
        out.push_str(&format!(
            "\n(Some changes were pushed out before they were told: only the last {KEPT} are \
             kept.)"
        ));
    }
    // The lines of the wait itself come first when there is not room.
    let room = MOST_LINES.saturating_sub(after.len().min(MOST_LINES));
    let old = &before[before.len().saturating_sub(room)..];
    let mut lines = |head: &str, all: usize, shown: &[Change]| {
        out.push_str(head);
        if all > shown.len() {
            out.push_str(&format!(
                "\n({} earlier changes left out.)",
                all - shown.len()
            ));
        }
        for c in shown {
            out.push('\n');
            out.push_str(&line(c));
        }
    };
    if !before.is_empty() {
        lines("\nBefore the wait:", before.len(), old);
    }
    let apart = lost || !before.is_empty();
    let lead = if apart { "\n" } else { " " };
    if !after.is_empty() {
        let new = &after[after.len().saturating_sub(MOST_LINES)..];
        lines(&format!("{lead}Meanwhile:"), after.len(), new);
    }
    match stalled {
        Some(secs) => out.push_str(&format!(
            "{}What changed meanwhile is not fully known: {}.",
            if apart || !after.is_empty() {
                "\n"
            } else {
                " "
            },
            stall_text(secs)
        )),
        None if after.is_empty() => out.push_str(&format!("{lead}Nothing changed meanwhile.")),
        None => {}
    }
    out
}

/// The answer to `takeover_changes`: the lines, newest last, and the
/// change to ask after next time.
pub fn changes_text(
    list: &[Change],
    since: u64,
    newest: u64,
    lost: bool,
    scope: &str,
    stalled: Option<u64>,
) -> String {
    let mut out = String::new();
    if lost {
        out.push_str(&format!(
            "(Some changes after #{since} were pushed out: only the last {KEPT} are kept.)\n"
        ));
    }
    if let Some(secs) = stalled {
        out.push_str(&format!(
            "What changed{scope} is not fully known: {}.\n",
            stall_text(secs)
        ));
    }
    if list.is_empty() && stalled.is_some() {
        out.push_str(&format!("No change was recorded since #{since}."));
    } else if list.is_empty() {
        out.push_str(&format!("Nothing changed{scope} since #{since}."));
    } else {
        push_lines(&mut out, list);
        out.push_str(&format!(
            "{} change{}{scope}, up to #{newest}.",
            list.len(),
            if list.len() == 1 { "" } else { "s" }
        ));
    }
    out.push_str(&format!(
        " The next takeover_changes starts after #{newest} by itself."
    ));
    out
}

/// The changes the helper read in programs that run as administrator, as
/// part of another answer. Its ring counts on its own, so no numbers: the
/// agent's `since` names the other ring's.
pub fn changes_part(list: &[Change], lost: bool, scope: &str, stalled: Option<u64>) -> String {
    let mut out = String::new();
    if lost {
        out.push_str(&format!(
            "(Some changes were pushed out: only the last {KEPT} are kept.)\n"
        ));
    }
    if let Some(secs) = stalled {
        out.push_str(&format!(
            "What changed{scope} is not fully known: {}.\n",
            stall_text(secs)
        ));
    }
    if list.is_empty() && stalled.is_some() {
        out.push_str("No change was recorded since the last look.");
    } else if list.is_empty() {
        out.push_str(&format!("Nothing changed{scope} since the last look."));
    } else {
        push_lines(&mut out, list);
        out.push_str(&format!(
            "{} change{}{scope}.",
            list.len(),
            if list.len() == 1 { "" } else { "s" }
        ));
    }
    out
}

/// The last lines of `list`, one a line, saying how many were left out.
fn push_lines(out: &mut String, list: &[Change]) {
    let skip = list.len().saturating_sub(MOST_LINES);
    if skip > 0 {
        out.push_str(&format!("({skip} earlier changes left out.)\n"));
    }
    for c in &list[skip..] {
        out.push_str(&line(c));
        out.push('\n');
    }
}

/// What came of an action, in a sentence: windows first, as a dialog that
/// opened matters most, then the rest, at most `most` of them.
pub fn digest(list: &[Change], most: usize) -> String {
    // The front is last: an action brings its own window forward anyway.
    let rank = |c: &Change| match c.what {
        What::Shown | What::Closed => 0,
        What::Title | What::MenuOpened => 1,
        What::Front => 3,
        _ => 2,
    };
    let mut picked: Vec<&Change> = list
        .iter()
        .filter(|c| !matches!(c.what, What::MenuClosed))
        .collect();
    picked.sort_by_key(|c| rank(c));
    if picked.is_empty() {
        return String::new();
    }
    let more = picked.len().saturating_sub(most);
    let mut said: Vec<String> = picked.iter().take(most).map(|c| brief(c)).collect();
    if more > 0 {
        said.push(format!("{more} more (takeover_changes has them)"));
    }
    format!(" Then: {}.", said.join("; "))
}

/// What `takeover_wait` waits for. It ends when any of them holds.
#[derive(Debug, Default)]
pub struct Until {
    /// A window whose title contains this is open.
    pub appears: Option<String>,
    /// An element's name or value, or a title, came to contain this.
    pub text: Option<String>,
    /// No window whose title contains this is open.
    pub gone: Option<String>,
    /// Nothing changed for this long.
    pub quiet_ms: Option<u64>,
}

impl Until {
    pub fn is_empty(&self) -> bool {
        self.appears.is_none()
            && self.text.is_none()
            && self.gone.is_none()
            && self.quiet_ms.is_none()
    }
}

/// Which of `u` holds, given the open windows' `titles`, the changes
/// `seen` since the wait began at `started_ms`, and the time `now_ms`.
/// While the watcher is `stalled` nothing is known to be quiet: no change
/// seen then means none could be read, not that none happened.
pub fn verdict(
    u: &Until,
    titles: &[String],
    seen: &[Change],
    started_ms: u64,
    now_ms: u64,
    stalled: Option<u64>,
) -> Option<String> {
    let has = |s: &str, part: &str| s.to_lowercase().contains(&part.to_lowercase());
    if let Some(a) = u.appears.as_deref() {
        if let Some(t) = titles.iter().find(|t| has(t, a)) {
            return Some(format!("a window titled like {a:?} is open: {t:?}"));
        }
    }
    if let Some(g) = u.gone.as_deref() {
        if !titles.iter().any(|t| has(t, g)) {
            return Some(format!("no window titled like {g:?} is open"));
        }
    }
    if let Some(x) = u.text.as_deref() {
        // Only what a change brought: a window's title or an element's
        // name is in every change there, whatever it was that changed.
        let hit = seen.iter().find(|c| c.brought().is_some_and(|t| has(t, x)));
        if let Some(c) = hit {
            return Some(format!("{x:?} appeared: {}", brief(c)));
        }
    }
    if let Some(q) = u.quiet_ms.filter(|_| stalled.is_none()) {
        // Each change has its own time, and they are not always in order.
        let last = seen
            .iter()
            .map(|c| c.at_ms)
            .max()
            .map_or(started_ms, |at| at.max(started_ms));
        if now_ms.saturating_sub(last) >= q {
            return Some(format!("nothing changed for {q} ms"));
        }
    }
    None
}

// The rest touches Windows, and is tried by hand rather than tested,
// but for the small pure parts it leans on.

/// An open process handle, which keeps Windows from handing the program's
/// id to another program for as long as it is held. A hook is set by id
/// alone, so without it a hook would outlive its program and deliver the
/// events of whatever got the id next, a program nobody approved.
struct Held {
    handle: HANDLE,
    /// Whether the handle may be waited on, else its exit code is read.
    waitable: bool,
}

// The handle is only waited on, asked for its exit code and closed, which
// any thread may do.
unsafe impl Send for Held {}

impl Drop for Held {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

impl Held {
    /// Opens program `pid`, to wait on when Windows allows, else only to
    /// ask. Either way the id is pinned.
    fn open(pid: u32) -> Option<Held> {
        unsafe {
            let both = PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION;
            if let Ok(handle) = OpenProcess(both, false, pid) {
                return Some(Held {
                    handle,
                    waitable: true,
                });
            }
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
                .ok()
                .map(|handle| Held {
                    handle,
                    waitable: false,
                })
        }
    }

    /// Whether the program has ended. Costs no call into the program.
    fn ended(&self) -> bool {
        const STILL_ACTIVE: u32 = 259;
        unsafe {
            if self.waitable {
                return WaitForSingleObject(self.handle, 0) == WAIT_OBJECT_0;
            }
            let mut code = 0u32;
            GetExitCodeProcess(self.handle, &mut code).is_ok() && code != STILL_ACTIVE
        }
    }
}

/// A program to hook next.
struct Ask {
    /// The window the human approved the program by, kept so the app can
    /// be asked again whether the grant still holds.
    window: Window,
    held: Held,
}

/// What the watching thread shares with the tools.
struct Shared {
    ring: Ring,
    /// The programs hooked, each with the window it was approved by.
    followed: HashMap<u32, Window>,
    /// The programs to hook next.
    asked: Vec<Ask>,
    /// The programs to stop watching.
    dropped: Vec<u32>,
    /// The programs that could not be watched, by name, and why.
    failed: HashMap<u32, (String, String)>,
    /// How many times the thread has caught up, for waiting on it.
    rounds: u64,
    /// Whether the last round left events unread, its time being up.
    behind: bool,
    /// Whether the thread was started.
    spawned: bool,
}

struct Watch {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    /// The watching thread's id, 0 until it runs.
    thread: Arc<AtomicU32>,
    /// When the thread last came round or finished a read, in
    /// milliseconds since `start`.
    beat: Arc<AtomicU64>,
    start: Instant,
}

static WATCH: OnceLock<Watch> = OnceLock::new();

fn watch() -> &'static Watch {
    WATCH.get_or_init(|| Watch {
        shared: Arc::new((
            Mutex::new(Shared {
                ring: Ring::new(KEPT),
                followed: HashMap::new(),
                asked: Vec::new(),
                dropped: Vec::new(),
                failed: HashMap::new(),
                rounds: 0,
                behind: false,
                spawned: false,
            }),
            Condvar::new(),
        )),
        thread: Arc::new(AtomicU32::new(0)),
        beat: Arc::new(AtomicU64::new(0)),
        start: Instant::now(),
    })
}

/// The watching thread's id, starting the thread when it does not run. A
/// start that failed or has not answered is said, and tried or waited for
/// again by the next call.
fn thread_of(w: &'static Watch) -> Result<u32, String> {
    let id = w.thread.load(Ordering::Acquire);
    if id != 0 {
        return Ok(id);
    }
    let (lock, changed) = &*w.shared;
    let mut s = lock
        .lock()
        .map_err(|_| "the watcher broke down".to_string())?;
    let first = !s.spawned;
    if first {
        let (theirs, id, beat, start) =
            (w.shared.clone(), w.thread.clone(), w.beat.clone(), w.start);
        thread::Builder::new()
            .name("desktop-watch".into())
            .spawn(move || run(theirs, start, id, beat))
            .map_err(|e| format!("the thread that watches could not be started: {e}"))?;
        s.spawned = true;
    }
    // A thread started by an earlier call and silent since is given only
    // a moment more, so each action is not slowed by five seconds.
    let most = Duration::from_millis(if first { 5000 } else { 200 });
    let _ = changed.wait_timeout_while(s, most, |_| w.thread.load(Ordering::Acquire) == 0);
    match w.thread.load(Ordering::Acquire) {
        0 => Err("the thread that watches has not started yet".into()),
        id => Ok(id),
    }
}

/// Milliseconds since watching began.
pub fn now_ms() -> u64 {
    watch().start.elapsed().as_millis() as u64
}

fn poke(thread: u32) {
    if thread != 0 {
        let _ = unsafe { PostThreadMessageW(thread, WM_APP, WPARAM(0), LPARAM(0)) };
    }
}

fn poke_watch(w: &Watch) {
    poke(w.thread.load(Ordering::Acquire));
}

fn pid_of(h: isize) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(HWND(h as *mut _), Some(&mut pid)) };
    pid
}

/// Starts watching the program of `window`, the one the human approved,
/// and returns once it is watched or could not be: [`trouble`] says why.
pub fn follow(window: &Window) {
    let w = watch();
    let pid = window.pid;
    let (lock, changed) = &*w.shared;
    if let Ok(mut s) = lock.lock() {
        if let Some(known) = s.followed.get_mut(&pid) {
            *known = window.clone();
            return;
        }
    }
    let fail = |why: String| {
        if let Ok(mut s) = lock.lock() {
            s.failed.insert(pid, (window.exe.clone(), why));
        }
    };
    let thread = match thread_of(w) {
        Ok(t) => t,
        Err(why) => return fail(why),
    };
    // Taken here, as soon as the program is approved, and checked against
    // the window: from now on the id can not pass to another program.
    let Some(held) = Held::open(pid) else {
        return fail("Windows would not open the program, so its end could not be seen".into());
    };
    if pid_of(window.hwnd) != pid {
        return fail("its window is gone".into());
    }
    let Ok(mut s) = lock.lock() else { return };
    s.failed.remove(&pid);
    s.dropped.retain(|p| *p != pid);
    s.asked.retain(|a| a.window.pid != pid);
    s.asked.push(Ask {
        window: window.clone(),
        held,
    });
    poke(thread);
    let _ = changed.wait_timeout_while(s, Duration::from_secs(2), |s| {
        !s.followed.contains_key(&pid) && !s.failed.contains_key(&pid)
    });
}

/// Stops watching program `pid` and drops what was kept of it: the grant
/// it was watched under no longer holds.
pub fn unfollow(pid: u32) {
    let Some(w) = WATCH.get() else { return };
    if let Ok(mut s) = w.shared.0.lock() {
        s.followed.remove(&pid);
        s.failed.remove(&pid);
        s.asked.retain(|a| a.window.pid != pid);
        s.ring.forget(pid);
        if !s.dropped.contains(&pid) {
            s.dropped.push(pid);
        }
    }
    poke_watch(w);
}

fn shared<T>(read: impl FnOnce(&mut Shared) -> T) -> Option<T> {
    let w = WATCH.get()?;
    let mut s = w.shared.0.lock().ok()?;
    Some(read(&mut s))
}

/// Whether program `pid` is watched.
pub fn following(pid: u32) -> bool {
    shared(|s| s.followed.contains_key(&pid)).unwrap_or(false)
}

/// Whether anything is watched at all.
pub fn any_followed() -> bool {
    shared(|s| !s.followed.is_empty()).unwrap_or(false)
}

/// The programs watched, each as the window it was approved by.
pub fn followed() -> Vec<Window> {
    shared(|s| s.followed.values().cloned().collect()).unwrap_or_default()
}

/// Why program `pid` could not be watched, when it could not.
pub fn trouble(pid: u32) -> Option<String> {
    shared(|s| s.failed.get(&pid).map(|(_, why)| why.clone())).flatten()
}

/// The programs that could not be watched, by name, and why.
pub fn troubles() -> Vec<(String, String)> {
    shared(|s| s.failed.values().cloned().collect()).unwrap_or_default()
}

/// The last change recorded.
pub fn newest() -> u64 {
    shared(|s| s.ring.newest()).unwrap_or(0)
}

/// The last change, once what came before now has been read, so changes
/// after it are what an action about to be made did.
pub fn mark() -> u64 {
    flush();
    newest()
}

/// Waits until the thread has read what the programs said so far.
pub fn flush() {
    let Some(w) = WATCH.get() else { return };
    let (lock, changed) = &*w.shared;
    let Ok(s) = lock.lock() else { return };
    let from = s.rounds;
    poke_watch(w);
    // One round may be under way already with older events in hand, and
    // a round whose time ran out leaves the rest to the next.
    let _ = changed.wait_timeout_while(s, Duration::from_millis(600), |s| {
        s.rounds < from + 2 || s.behind
    });
}

/// For how many seconds the watching thread has not come round, when
/// that is long enough to mean it is stuck in a read of a program that
/// does not answer. Nothing can be said of what changed meanwhile.
pub fn stalled() -> Option<u64> {
    let w = WATCH.get()?;
    if w.thread.load(Ordering::Acquire) == 0 {
        return None;
    }
    stall(
        w.beat.load(Ordering::Acquire),
        w.start.elapsed().as_millis() as u64,
    )
}

/// The changes after `seq` in program `pid`, or all, whether some were
/// pushed out, and the newest change, for an agent to read: they are
/// not folded away after.
pub fn since(seq: u64, pid: Option<u32>) -> (Vec<Change>, bool, u64) {
    read_ring(seq, pid, true)
}

/// As [`since`], for a look no agent sees, which leaves them free to fold.
pub fn peek(seq: u64, pid: Option<u32>) -> (Vec<Change>, bool, u64) {
    read_ring(seq, pid, false)
}

fn read_ring(seq: u64, pid: Option<u32>, handed: bool) -> (Vec<Change>, bool, u64) {
    shared(|s| {
        let (list, lost) = s.ring.since(seq);
        let newest = s.ring.newest();
        if handed {
            s.ring.hand_out(newest);
        }
        let list = list.into_iter().filter(|c| in_scope(c, pid)).collect();
        (list, lost, newest)
    })
    .unwrap_or_default()
}

/// Waits up to `most` for a change to be recorded.
pub fn wait_for_change(after: u64, most: Duration) {
    let Some(w) = WATCH.get() else {
        thread::sleep(most);
        return;
    };
    let (lock, changed) = &*w.shared;
    let Ok(s) = lock.lock() else { return };
    let _ = changed.wait_timeout_while(s, most, |s| s.ring.newest() <= after);
}

/// An event as the hook hands it over, to be read later.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Raw {
    event: u32,
    hwnd: isize,
    object: i32,
    child: i32,
    /// When it happened, as Windows' tick count has it.
    tick: u32,
}

impl Raw {
    /// It is about the window itself, not something in it.
    fn whole(&self) -> bool {
        self.object == OBJID_WINDOW && self.child == CHILDID_SELF
    }

    /// A window or dialog coming.
    fn opens(&self) -> bool {
        self.event == EVENT_DIALOGSTART || (self.event == EVENT_SHOW && self.whole())
    }

    /// A window or dialog going.
    fn closes(&self) -> bool {
        self.event == EVENT_DIALOGEND
            || (matches!(self.event, EVENT_HIDE | EVENT_DESTROY) && self.whole())
    }

    /// A change of an element, which takes a read of the program to tell.
    fn element(&self) -> bool {
        matches!(
            self.event,
            EVENT_NAMECHANGE | EVENT_VALUECHANGE | EVENT_STATECHANGE
        )
    }

    /// Whether anything is made of it: the parts of a window showing,
    /// hiding and being destroyed come in floods and are told by nobody.
    fn matters(&self) -> bool {
        self.whole() || !matches!(self.event, EVENT_SHOW | EVENT_HIDE | EVENT_DESTROY)
    }

    /// What it is of, whenever it came.
    fn same(&self) -> (u32, isize, i32, i32) {
        (self.event, self.hwnd, self.object, self.child)
    }
}

/// The most events held before the thread reads them.
const MOST_RAW: usize = 5000;

/// Takes `r` into the queue of at most `most`, and says whether nothing
/// was lost for it. A window going is never lost, as its entry would
/// stay behind as open: the oldest element change makes room for it.
fn enqueue(q: &mut VecDeque<Raw>, r: Raw, most: usize) -> bool {
    if q.len() < most {
        q.push_back(r);
        return true;
    }
    if !r.closes() {
        return false;
    }
    match q.iter().position(Raw::element) {
        Some(i) => {
            q.remove(i);
            q.push_back(r);
            false
        }
        // Nothing but windows coming and going: held past the limit, as
        // long as that is not a flood of its own.
        None if q.len() < most * 4 => {
            q.push_back(r);
            true
        }
        None => false,
    }
}

/// Of a burst, what is read and in which order. Of the same change of
/// one element only the last is read: it has the value the others led to.
/// A window shown goes ahead of what came before it, as the elements
/// filled in while it is made say so before it shows and are known as its
/// making only once it is, but never ahead of the same window going: one
/// hidden and shown again in a moment is open, not closed.
fn fold_burst(raw: Vec<Raw>) -> Vec<Raw> {
    let mut kept = Vec::with_capacity(raw.len());
    let mut had = HashSet::new();
    for r in raw.into_iter().rev() {
        if !r.element() || had.insert(r.same()) {
            kept.push(r);
        }
    }
    kept.reverse();
    let mut went: HashMap<isize, i64> = HashMap::new();
    let mut ordered: Vec<((i64, u8, usize), Raw)> = Vec::with_capacity(kept.len());
    for (i, r) in kept.into_iter().enumerate() {
        let place = if r.opens() {
            (went.get(&r.hwnd).copied().unwrap_or(-1), 1, i)
        } else {
            (i as i64, 0, i)
        };
        if r.closes() {
            went.insert(r.hwnd, i as i64);
        }
        ordered.push((place, r));
    }
    ordered.sort_by_key(|(place, _)| *place);
    ordered.into_iter().map(|(_, r)| r).collect()
}

/// Cuts a queue down to `most` by its oldest element changes, and says
/// how many went. Windows coming and going all stay.
fn shed(queue: &mut VecDeque<Raw>, most: usize) -> u32 {
    let mut over = queue.len().saturating_sub(most);
    let mut gone = 0;
    queue.retain(|r| {
        if over > 0 && r.element() {
            over -= 1;
            gone += 1;
            false
        } else {
            true
        }
    });
    gone
}

/// When an event with tick count `tick` happened, in milliseconds since
/// watching began, given both clocks now. An event read late keeps its
/// own time, so a long round is not counted as one moment.
fn happened_ms(now_ms: u64, now_tick: u32, tick: u32) -> u64 {
    now_ms.saturating_sub(age_ms(now_tick, tick))
}

/// How long ago `tick` was. A tick that is not a time (some programs
/// send none) or lies ahead counts as now.
fn age_ms(now_tick: u32, tick: u32) -> u64 {
    const A_DAY: u32 = 86_400_000;
    let age = now_tick.wrapping_sub(tick);
    if tick == 0 || age > A_DAY {
        0
    } else {
        age as u64
    }
}

/// The time of day `age_ms` before `clock`, both in seconds of the day.
fn clock_before(clock: u32, age_ms: u64) -> u32 {
    let back = (age_ms / 1000 % 86_400) as u32;
    (clock + 86_400 - back) % 86_400
}

thread_local! {
    static RAW: RefCell<VecDeque<Raw>> = const { RefCell::new(VecDeque::new()) };
    static POKED: Cell<bool> = const { Cell::new(false) };
    /// Events the queue had no room for, to be said.
    static DROPPED: Cell<u32> = const { Cell::new(0) };
}

unsafe extern "system" fn on_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    time: u32,
) {
    let r = Raw {
        event,
        hwnd: hwnd.0 as isize,
        object,
        child,
        tick: time,
    };
    if !r.matters() {
        return;
    }
    let taken = RAW.with(|q| enqueue(&mut q.borrow_mut(), r, MOST_RAW));
    if !taken {
        DROPPED.set(DROPPED.get().saturating_add(1));
    }
    if !POKED.replace(true) {
        poke(unsafe { GetCurrentThreadId() });
    }
}

/// A window seen shown, kept so its closing can name it.
struct Known {
    window: Window,
    visible: bool,
    /// When it was shown while watched, none for one open before.
    shown: Option<Instant>,
}

/// How long after a window is shown its elements are still being filled
/// in, so their first values are its making and not a change.
const MAKING: Duration = Duration::from_millis(1000);

/// How long one round reads elements before it leaves the rest to the
/// next, so a storm in one program does not hold up the answers about
/// every other, nor the marks taken before each click.
const READ_BUDGET: Duration = Duration::from_millis(50);

/// How many elements' last values are remembered.
const MOST_RECALLED: usize = 4000;

/// A program hooked.
struct Hooked {
    hooks: Vec<HWINEVENTHOOK>,
    /// Pins the program's id for as long as it is hooked.
    held: Held,
}

/// What the watching thread keeps to itself.
struct Local {
    windows: HashMap<isize, Known>,
    values: Recall<String>,
    names: Recall<String>,
    states: Recall<u32>,
    intake: Intake,
    start: Instant,
    hooked: HashMap<u32, Hooked>,
    /// Events worth a read that the last round had no time for.
    pending: VecDeque<Raw>,
    beat: Arc<AtomicU64>,
}

impl Local {
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// Notes that the thread is alive, for [`stalled`].
    fn beat(&self) {
        self.beat.store(self.now_ms(), Ordering::Release);
    }

    /// Unhooks program `pid` and forgets its windows and values, so a
    /// window handle Windows uses again does not get the old title.
    fn forget(&mut self, pid: u32) {
        if let Some(h) = self.hooked.remove(&pid) {
            for hook in h.hooks {
                let _ = unsafe { UnhookWinEvent(hook) };
            }
        }
        self.windows.retain(|_, k| k.window.pid != pid);
        self.values.forget(pid);
        self.names.forget(pid);
        self.states.forget(pid);
    }
}

/// Classes of windows that come and go with no meaning to the agent.
const NOISE_CLASSES: &[&str] = &[
    "tooltips_class32",
    "#32768",
    "SysShadow",
    "IME",
    "MSCTFIME UI",
    "ComboLBox",
];

fn run(
    shared: Arc<(Mutex<Shared>, Condvar)>,
    start: Instant,
    id: Arc<AtomicU32>,
    beat: Arc<AtomicU64>,
) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let mut msg = MSG::default();
        // A thread has a queue for posted messages once it has asked.
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        let mut local = Local {
            windows: HashMap::new(),
            values: Recall::new(MOST_RECALLED),
            names: Recall::new(MOST_RECALLED),
            states: Recall::new(MOST_RECALLED),
            intake: Intake::default(),
            start,
            hooked: HashMap::new(),
            pending: VecDeque::new(),
            beat,
        };
        local.beat();
        id.store(GetCurrentThreadId(), Ordering::Release);
        // Under the lock, so a caller about to wait for the id hears it.
        drop(shared.0.lock());
        shared.1.notify_all();
        // A poke the hooks post can be missed, so the thread also looks
        // ten times a second, which is its heartbeat too.
        SetTimer(None, 0, 100, None);
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            round(&shared, &mut local);
        }
        let pids: Vec<u32> = local.hooked.keys().copied().collect();
        for pid in pids {
            local.forget(pid);
        }
        // Nothing is watched any more, and the next follow starts anew.
        id.store(0, Ordering::Release);
        if let Ok(mut s) = shared.0.lock() {
            s.followed.clear();
            s.spawned = false;
        }
    }
}

/// Hooks newly asked programs, unhooks those that ended or were dropped,
/// and reads the events that came, as many as its time allows.
fn round(shared: &Arc<(Mutex<Shared>, Condvar)>, local: &mut Local) {
    local.beat();
    let (lock, changed) = &**shared;
    let (asked, dropped) = lock
        .lock()
        .map(|mut s| (std::mem::take(&mut s.asked), std::mem::take(&mut s.dropped)))
        .unwrap_or_default();
    // Looked at every round: the handle answers without a call into the
    // program, and an ended program's hooks must not last.
    let ended: Vec<u32> = local
        .hooked
        .iter()
        .filter(|(_, h)| h.held.ended())
        .map(|(pid, _)| *pid)
        .collect();
    for pid in ended.iter().chain(&dropped) {
        local.forget(*pid);
    }
    let mut hooked = Vec::new();
    let mut failed = Vec::new();
    for ask in asked {
        let pid = ask.window.pid;
        // Asked twice before the first round answered: hooked once.
        if local.hooked.contains_key(&pid) {
            hooked.push(ask.window);
            continue;
        }
        if ask.held.ended() {
            failed.push((pid, ask.window.exe, "the program has ended".to_string()));
            continue;
        }
        let mut hooks = Vec::new();
        for (min, max) in HOOKED {
            let h = unsafe {
                SetWinEventHook(
                    min,
                    max,
                    None,
                    Some(on_event),
                    pid,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            };
            if h.is_invalid() {
                break;
            }
            hooks.push(h);
        }
        // Half hooked would read as watched and tell half: all or none.
        if hooks.len() < HOOKED.len() {
            for h in hooks {
                let _ = unsafe { UnhookWinEvent(h) };
            }
            failed.push((
                pid,
                ask.window.exe,
                "Windows would not let its events be hooked".to_string(),
            ));
            continue;
        }
        for w in windows_of(pid) {
            local.windows.insert(
                w.hwnd,
                Known {
                    window: w,
                    visible: true,
                    shown: None,
                },
            );
        }
        local.hooked.insert(
            pid,
            Hooked {
                hooks,
                held: ask.held,
            },
        );
        hooked.push(ask.window);
    }

    POKED.set(false);
    let came: Vec<Raw> = RAW.with(|r| r.borrow_mut().drain(..).collect());
    let (now_ms, now_tick) = (local.now_ms(), unsafe { GetTickCount() });
    let idle = came.is_empty() && local.pending.is_empty();
    local.intake.lose(DROPPED.replace(0));
    // Which are worth a read is settled before any is read: a read is a
    // call into the program, and a grid of thousands of cells refreshing
    // must not cost thousands of them.
    for r in fold_burst(came) {
        if !r.element() || local.intake.admit(happened_ms(now_ms, now_tick, r.tick)) {
            local.pending.push_back(r);
        }
    }
    let mut queue: VecDeque<Raw> = fold_burst(local.pending.drain(..).collect()).into();
    let over = shed(&mut queue, MOST_RAW);
    local.intake.lose(over);
    let clock = unsafe {
        let t = GetLocalTime();
        t.wHour as u32 * 3600 + t.wMinute as u32 * 60 + t.wSecond as u32
    };
    let began = Instant::now();
    let mut fresh = Vec::new();
    while let Some(r) = queue.pop_front() {
        if let Some(mut c) = read(local, r) {
            let age = age_ms(now_tick, r.tick);
            c.at_ms = now_ms.saturating_sub(age);
            c.clock = clock_before(clock, age);
            fresh.push(c);
        }
        local.beat();
        if began.elapsed() >= READ_BUDGET {
            break;
        }
    }
    local.pending = queue;
    let behind = !local.pending.is_empty();
    if behind && !POKED.replace(true) {
        // The rest at once, not at the next tick of the timer.
        poke(unsafe { GetCurrentThreadId() });
    }
    let skipped = local.intake.take_skipped(now_ms, idle).map(|n| {
        let mut note = Change::skipped(n);
        note.at_ms = now_ms;
        note.clock = clock;
        note
    });
    if let Ok(mut s) = lock.lock() {
        for c in fresh {
            s.ring.push(c);
        }
        if let Some(note) = skipped {
            s.ring.push(note);
        }
        for pid in &ended {
            s.followed.remove(pid);
        }
        for pid in &dropped {
            // Again, for what a round under way recorded since it was
            // asked to stop.
            s.followed.remove(pid);
            s.ring.forget(*pid);
        }
        for w in hooked {
            s.failed.remove(&w.pid);
            s.followed.insert(w.pid, w);
        }
        for (pid, exe, why) in failed {
            s.failed.insert(pid, (exe, why));
        }
        s.rounds += 1;
        s.behind = behind;
    }
    changed.notify_all();
}

unsafe extern "system" fn each_window(h: HWND, list: LPARAM) -> windows::core::BOOL {
    unsafe { (*(list.0 as *mut Vec<isize>)).push(h.0 as isize) };
    true.into()
}

/// Program `pid`'s windows shown now, with a title or as a dialog.
fn windows_of(pid: u32) -> Vec<Window> {
    let mut all: Vec<isize> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(each_window), LPARAM(&mut all as *mut _ as isize));
    }
    all.into_iter()
        .filter(|&h| pid_of(h) == pid && unsafe { IsWindowVisible(HWND(h as *mut _)) }.as_bool())
        .map(info)
        .filter(worth_showing)
        .collect()
}

fn worth_showing(w: &Window) -> bool {
    (!w.title.is_empty() || w.class == "#32770") && !NOISE_CLASSES.contains(&w.class.as_str())
}

fn is_top(h: isize) -> bool {
    let hw = HWND(h as *mut _);
    let root = unsafe { GetAncestor(hw, GA_ROOT) };
    root == hw
}

fn class_of(h: isize) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(HWND(h as *mut _), &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn root_of(h: isize) -> isize {
    let root = unsafe { GetAncestor(HWND(h as *mut _), GA_ROOT) };
    if root.is_invalid() {
        h
    } else {
        root.0 as isize
    }
}

/// The top level window `h` is in, named, known ones from memory.
fn top_of(local: &Local, h: isize) -> Window {
    let root = root_of(h);
    match local.windows.get(&root) {
        Some(k) => k.window.clone(),
        None => info(root),
    }
}

/// The change one event makes, none when it says nothing new.
fn read(local: &mut Local, r: Raw) -> Option<Change> {
    // A hook is set by a program's id, so what it delivers is checked
    // against the programs watched now: an event still queued from one
    // that ended or was dropped tells nothing. A window going is known by
    // its entry instead, as Windows no longer says whose it was.
    if !r.closes() && !local.hooked.contains_key(&pid_of(r.hwnd)) {
        return None;
    }
    match r.event {
        EVENT_SHOW | EVENT_DIALOGSTART if r.opens() => {
            if !is_top(r.hwnd) || !unsafe { IsWindowVisible(HWND(r.hwnd as *mut _)) }.as_bool() {
                return None;
            }
            if local.windows.get(&r.hwnd).is_some_and(|k| k.visible) {
                return None;
            }
            let w = info(r.hwnd);
            if !worth_showing(&w) {
                return None;
            }
            let c = Change::of(What::Shown, &w);
            local.windows.insert(
                r.hwnd,
                Known {
                    window: w,
                    visible: true,
                    shown: Some(Instant::now()),
                },
            );
            Some(c)
        }
        EVENT_HIDE | EVENT_DESTROY | EVENT_DIALOGEND if r.closes() => {
            let k = local.windows.get_mut(&r.hwnd)?;
            let was = std::mem::replace(&mut k.visible, false);
            let c = Change::of(What::Closed, &k.window);
            if r.event == EVENT_DESTROY {
                local.windows.remove(&r.hwnd);
            }
            was.then_some(c)
        }
        EVENT_FOREGROUND => {
            let w = top_of(local, r.hwnd);
            (!w.title.is_empty()).then(|| Change::of(What::Front, &w))
        }
        EVENT_MENUSTART | EVENT_MENUPOPUPSTART | EVENT_MENUEND | EVENT_MENUPOPUPEND => {
            let mut w = top_of(local, r.hwnd);
            // A popup menu is a window of its own; the one it belongs to
            // is the program's window in front.
            if w.title.is_empty() {
                let front = unsafe { GetForegroundWindow() };
                let f = top_of(local, front.0 as isize);
                if f.pid == w.pid {
                    w = f;
                }
            }
            let opened = matches!(r.event, EVENT_MENUSTART | EVENT_MENUPOPUPSTART);
            let what = if opened {
                What::MenuOpened
            } else {
                What::MenuClosed
            };
            Some(Change::of(what, &w))
        }
        // A top level window's name is its title, told only once it is
        // known: before, it is still being made.
        EVENT_NAMECHANGE if r.whole() && is_top(r.hwnd) => {
            let k = local.windows.get_mut(&r.hwnd)?;
            let title = info(r.hwnd).title;
            if k.window.title == title {
                return None;
            }
            let mut c = Change::of(What::Title, &k.window);
            c.from = Some(std::mem::replace(&mut k.window.title, title.clone()));
            c.to = title.clone();
            c.title = title;
            Some(c)
        }
        EVENT_NAMECHANGE | EVENT_VALUECHANGE | EVENT_STATECHANGE => element(local, r),
        _ => None,
    }
}

/// An element's change, read through MSAA.
fn element(local: &mut Local, mut r: Raw) -> Option<Change> {
    if NOISE_CLASSES.contains(&class_of(r.hwnd).as_str()) {
        return None;
    }
    // A control that is a window of its own says a change twice, as the
    // window and as its content. The content has its real role.
    if r.whole() {
        r.object = OBJID_CLIENT;
    }
    let root = root_of(r.hwnd);
    let making = match local.windows.get(&root) {
        Some(k) => k.shown.is_some_and(|at| at.elapsed() < MAKING),
        None => !unsafe { IsWindowVisible(HWND(root as *mut _)) }.as_bool(),
    };
    let w = top_of(local, r.hwnd);
    let key = (r.hwnd, r.object, r.child);
    let what = match r.event {
        EVENT_NAMECHANGE => What::Name,
        EVENT_VALUECHANGE => What::Value,
        _ => What::State,
    };
    let mut c = Change::of(what, &w);
    c.key = Some(key);
    // A read is a call into the program, with no limit of its own: one
    // that Windows already knows does not answer is not called at all.
    // A window that runs as administrator answers nothing to a reader
    // that does not. Either way the change is kept without what it was:
    // still worth knowing for a wait until things are quiet.
    let hung = unsafe { IsHungAppWindow(HWND(root as *mut _)) }.as_bool();
    let seen = if hung {
        None
    } else {
        unsafe { read_element(r, what == What::Value) }
    };
    let Some(seen) = seen else {
        return (what == What::Value).then_some(c);
    };
    c.role = role_name(seen.role).to_string();
    c.name = seen.name.clone();
    if seen.protected {
        // Should it have been readable before it turned protected.
        local.values.remove(&key);
    }
    match what {
        What::Name => {
            let name = seen.name;
            let before = local.names.insert(key, w.pid, name.clone());
            if before.as_deref() == Some(name.as_str()) || name.is_empty() {
                return None;
            }
            // Controls say their name again for many reasons; a name seen
            // for the first time is news only on a text that tells status.
            if before.is_none() && !matches!(c.role.as_str(), "Text" | "StatusBar") {
                return None;
            }
            c.from = before;
            c.to = name;
        }
        // A password's field: that it changed is told, never to what.
        What::Value if seen.protected => c.hidden = true,
        What::Value => {
            let value = seen.value?;
            let before = local.values.insert(key, w.pid, value.clone());
            if before.as_deref() == Some(value.as_str()) {
                return None;
            }
            c.from = before;
            c.to = value;
        }
        _ => {
            let before = local.states.insert(key, w.pid, seen.state);
            if before == Some(seen.state) {
                return None;
            }
            state_change(before, seen.state)?;
            c.states = Some((before, seen.state));
        }
    }
    let first = c.from.is_none() && c.states.is_none_or(|(b, _)| b.is_none());
    (!(making && first)).then_some(c)
}

/// What an element says of itself.
struct Seen {
    role: i32,
    name: String,
    /// None when it has none, was not asked for, or is protected.
    value: Option<String>,
    state: u32,
    protected: bool,
}

/// The element an event names, its value only when `with_value` and the
/// element is not protected: the state is read first, so a password is
/// never asked for, whatever the program would answer.
unsafe fn read_element(r: Raw, with_value: bool) -> Option<Seen> {
    unsafe {
        let mut acc: Option<IAccessible> = None;
        let mut child = VARIANT::default();
        AccessibleObjectFromEvent(
            HWND(r.hwnd as *mut _),
            r.object as u32,
            r.child as u32,
            &mut acc,
            &mut child,
        )
        .ok()?;
        let acc = acc?;
        let state = acc
            .get_accState(&child)
            .ok()
            .and_then(|v| i32::try_from(&v).ok())
            .unwrap_or(0) as u32;
        let protected = state & STATE_PROTECTED != 0;
        let role = acc
            .get_accRole(&child)
            .ok()
            .and_then(|v| i32::try_from(&v).ok())
            .unwrap_or(0);
        let name = acc
            .get_accName(&child)
            .map(|b| b.to_string())
            .unwrap_or_default();
        let value = if with_value && !protected {
            acc.get_accValue(&child).ok().map(|b| b.to_string())
        } else {
            None
        };
        Some(Seen {
            role,
            name,
            value,
            state,
            protected,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn win(title: &str) -> Window {
        Window {
            hwnd: 66,
            title: title.into(),
            exe: "app.exe".into(),
            path: r"C:\Apps\app.exe".into(),
            pid: 7,
            owner: None,
            class: String::new(),
        }
    }

    fn value(at_ms: u64, from: Option<&str>, to: &str) -> Change {
        let mut c = Change::of(What::Value, &win("Settings"));
        c.at_ms = at_ms;
        c.role = "Edit".into();
        c.name = "IP address".into();
        c.key = Some((5, -4, 0));
        c.from = from.map(String::from);
        c.to = to.into();
        c
    }

    #[test]
    fn a_dialog_is_owned_or_of_the_dialog_class() {
        let mut w = win("Save As");
        assert!(!Change::of(What::Shown, &w).dialog);
        w.owner = Some(1);
        assert!(Change::of(What::Shown, &w).dialog);
        w.owner = None;
        w.class = "#32770".into();
        assert!(Change::of(What::Shown, &w).dialog);
    }

    #[test]
    fn the_ring_numbers_changes_and_keeps_the_last() {
        let mut r = Ring::new(3);
        assert_eq!(r.newest(), 0);
        for i in 0..5 {
            let mut c = Change::of(What::Front, &win(&format!("w{i}")));
            c.at_ms = i * 5000;
            c.window = i as isize;
            assert!(r.push(c));
        }
        assert_eq!(r.newest(), 5);
        let (all, lost) = r.since(0);
        assert_eq!(all.iter().map(|c| c.seq).collect::<Vec<_>>(), [3, 4, 5]);
        assert!(lost);
        let (after, lost) = r.since(3);
        assert_eq!(after.len(), 2);
        assert!(!lost);
        assert!(r.since(5).0.is_empty());
    }

    #[test]
    fn a_fast_changing_value_folds_into_one_change_from_first_to_last() {
        let mut r = Ring::new(10);
        r.push(value(0, Some("10.0.0.3"), "10.0.0.4"));
        r.push(value(300, Some("10.0.0.4"), "10.0.0.5"));
        let (all, _) = r.since(0);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].from.as_deref(), Some("10.0.0.3"));
        assert_eq!(all[0].to, "10.0.0.5");
        // Folded under a new number, so a reader who had the first sees it.
        assert_eq!(all[0].seq, 2);
        // Later than the fold, a change of its own.
        r.push(value(5000, Some("10.0.0.5"), "10.0.0.6"));
        assert_eq!(r.since(0).0.len(), 2);
    }

    #[test]
    fn a_value_that_went_back_leaves_nothing() {
        let mut r = Ring::new(10);
        r.push(value(0, Some("a"), "b"));
        r.push(value(200, Some("b"), "a"));
        assert!(r.since(0).0.is_empty());
    }

    #[test]
    fn a_value_read_before_it_went_back_keeps_both_lines() {
        let mut r = Ring::new(10);
        r.push(value(0, Some("a"), "b"));
        r.hand_out(r.newest());
        r.push(value(200, Some("b"), "a"));
        let all = r.since(0).0;
        assert_eq!(all.len(), 2);
        assert_eq!(
            (all[1].from.as_deref(), all[1].to.as_str()),
            (Some("b"), "a")
        );
        // What no reader had yet still folds: there and back is nothing.
        r.push(value(400, Some("a"), "b"));
        let all = r.since(0).0;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].to, "b");
    }

    #[test]
    fn a_state_read_before_it_went_back_keeps_both_lines() {
        let mut r = Ring::new(10);
        let mut a = Change::of(What::State, &win("w"));
        a.key = Some((1, -4, 0));
        a.states = Some((Some(0), STATE_CHECKED));
        r.push(a.clone());
        r.hand_out(1);
        a.at_ms = 100;
        a.states = Some((Some(STATE_CHECKED), 0));
        r.push(a);
        assert_eq!(r.since(0).0.len(), 2);
        assert_eq!(r.since(1).0[0].states, Some((Some(STATE_CHECKED), 0)));
    }

    #[test]
    fn a_window_shown_closed_and_shown_again_ends_shown() {
        let mut r = Ring::new(10);
        let mut a = Change::of(What::Shown, &win("Save As"));
        a.at_ms = 10;
        r.push(a.clone());
        let mut closed = Change::of(What::Closed, &win("Save As"));
        closed.at_ms = 200;
        assert!(r.push(closed));
        a.at_ms = 400;
        assert!(r.push(a));
        let all = r.since(0).0;
        assert_eq!(
            all.iter().map(|c| c.what).collect::<Vec<_>>(),
            [What::Shown, What::Closed, What::Shown]
        );
    }

    #[test]
    fn the_same_event_twice_is_kept_once() {
        let mut r = Ring::new(10);
        let mut a = Change::of(What::Shown, &win("Save As"));
        a.at_ms = 10;
        assert!(r.push(a.clone()));
        a.at_ms = 400;
        assert!(!r.push(a.clone()));
        a.at_ms = 4000;
        assert!(r.push(a));
        assert_eq!(r.since(0).0.len(), 2);
    }

    #[test]
    fn the_front_is_told_where_it_came_to_rest() {
        let mut r = Ring::new(10);
        let mut a = Change::of(What::Front, &win("Save As"));
        a.window = 1;
        r.push(a.clone());
        a.window = 2;
        a.title = "Main".into();
        a.at_ms = 300;
        r.push(a.clone());
        let all = r.since(0).0;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].title, "Main");
        a.at_ms = 5000;
        r.push(a);
        assert_eq!(r.since(0).0.len(), 2);
    }

    #[test]
    fn other_elements_do_not_fold_together() {
        let mut r = Ring::new(10);
        r.push(value(0, None, "a"));
        let mut other = value(100, None, "b");
        other.key = Some((6, -4, 0));
        r.push(other);
        assert_eq!(r.since(0).0.len(), 2);
    }

    #[test]
    fn a_state_folds_to_the_first_and_last() {
        let mut r = Ring::new(10);
        let mut a = Change::of(What::State, &win("w"));
        a.key = Some((1, -4, 0));
        a.role = "CheckBox".into();
        a.states = Some((Some(0), STATE_CHECKED));
        r.push(a.clone());
        a.at_ms = 100;
        a.states = Some((Some(STATE_CHECKED), STATE_CHECKED | STATE_UNAVAILABLE));
        r.push(a.clone());
        let all = r.since(0).0;
        assert_eq!(all.len(), 1);
        assert_eq!(
            all[0].states,
            Some((Some(0), STATE_CHECKED | STATE_UNAVAILABLE))
        );
        a.at_ms = 200;
        a.states = Some((Some(STATE_CHECKED | STATE_UNAVAILABLE), 0));
        r.push(a);
        assert!(r.since(0).0.is_empty());
    }

    #[test]
    fn a_storm_is_cut_and_counted() {
        let mut i = Intake::default();
        for n in 0..MOST_PER_SECOND {
            assert!(i.admit(1000 + n as u64));
        }
        assert!(!i.admit(1500));
        assert!(!i.admit(1600));
        // Not said while the second it happened in still runs.
        assert_eq!(i.take_skipped(1900, false), None);
        assert!(i.admit(2100));
        assert_eq!(i.take_skipped(2100, false), Some(2));
        assert_eq!(i.take_skipped(2100, false), None);
        assert!(i.admit(2200));
    }

    #[test]
    fn what_was_skipped_is_said_without_a_later_change() {
        let mut i = Intake::default();
        for _ in 0..MOST_PER_SECOND + 3 {
            i.admit(1000);
        }
        // Nothing more came: said at once, not when something changes.
        assert_eq!(i.take_skipped(1100, true), Some(3));
        assert_eq!(i.take_skipped(5000, true), None);
        // What the queue had no room for is counted with it.
        i.lose(7);
        assert_eq!(i.take_skipped(1100, false), None);
        assert_eq!(i.take_skipped(2000, false), Some(7));
    }

    #[test]
    fn a_storm_admits_a_second_s_worth_however_late_it_is_read() {
        // 5000 cells refreshed in one second, read in a round much later:
        // their own times count, so 60 are read and the rest only counted.
        let mut i = Intake::default();
        let read = (0..5000u64).filter(|n| i.admit(3000 + n % 900)).count();
        assert_eq!(read, MOST_PER_SECOND as usize);
        assert_eq!(i.take_skipped(9000, false), Some(5000 - MOST_PER_SECOND));
        // A change read out of order does not open a second already over.
        let mut i = Intake::default();
        assert!(i.admit(5000));
        for _ in 0..MOST_PER_SECOND {
            i.admit(4999);
        }
        assert!(!i.admit(5000));
    }

    fn raw(event: u32, hwnd: isize) -> Raw {
        Raw {
            event,
            hwnd,
            object: OBJID_WINDOW,
            child: CHILDID_SELF,
            tick: 0,
        }
    }

    fn cell(event: u32, hwnd: isize, child: i32) -> Raw {
        Raw {
            event,
            hwnd,
            object: OBJID_CLIENT,
            child,
            tick: 0,
        }
    }

    #[test]
    fn a_window_hidden_and_shown_again_in_one_burst_ends_shown() {
        let burst = vec![raw(EVENT_HIDE, 7), raw(EVENT_SHOW, 7)];
        assert_eq!(fold_burst(burst.clone()), burst);
        // With other things between, the showing goes right behind the
        // hiding, not ahead of it.
        let burst = vec![
            cell(EVENT_VALUECHANGE, 9, 1),
            raw(EVENT_HIDE, 7),
            cell(EVENT_VALUECHANGE, 9, 2),
            raw(EVENT_SHOW, 7),
        ];
        let events: Vec<(u32, isize)> = fold_burst(burst)
            .iter()
            .map(|r| (r.event, r.hwnd))
            .collect();
        assert_eq!(
            events,
            [
                (EVENT_VALUECHANGE, 9),
                (EVENT_HIDE, 7),
                (EVENT_SHOW, 7),
                (EVENT_VALUECHANGE, 9)
            ]
        );
        // Shown, hidden and shown: all three, in the order they came.
        let burst = vec![
            raw(EVENT_SHOW, 7),
            raw(EVENT_DESTROY, 7),
            raw(EVENT_SHOW, 7),
        ];
        assert_eq!(fold_burst(burst.clone()), burst);
    }

    #[test]
    fn a_window_shown_goes_ahead_of_its_elements_filled_in_before() {
        let burst = vec![
            cell(EVENT_VALUECHANGE, 9, 1),
            cell(EVENT_NAMECHANGE, 9, 2),
            raw(EVENT_SHOW, 7),
            raw(EVENT_DIALOGSTART, 8),
            raw(EVENT_HIDE, 5),
        ];
        let events: Vec<u32> = fold_burst(burst).iter().map(|r| r.event).collect();
        assert_eq!(
            events,
            [
                EVENT_SHOW,
                EVENT_DIALOGSTART,
                EVENT_VALUECHANGE,
                EVENT_NAMECHANGE,
                EVENT_HIDE
            ]
        );
    }

    #[test]
    fn of_a_burst_on_one_element_only_the_last_is_read() {
        let mut burst: Vec<Raw> = (0..1000)
            .map(|n| Raw {
                tick: n,
                ..cell(EVENT_VALUECHANGE, 9, 1)
            })
            .collect();
        burst.push(cell(EVENT_STATECHANGE, 9, 1));
        burst.push(cell(EVENT_VALUECHANGE, 9, 2));
        let folded = fold_burst(burst);
        assert_eq!(folded.len(), 3);
        // The last of the thousand, with its own time.
        assert_eq!(folded[0].tick, 999);
        // Windows coming and going are never folded together.
        let twice = vec![raw(EVENT_HIDE, 7), raw(EVENT_HIDE, 7)];
        assert_eq!(fold_burst(twice).len(), 2);
    }

    #[test]
    fn only_whole_windows_coming_and_going_matter() {
        assert!(raw(EVENT_SHOW, 7).matters());
        assert!(raw(EVENT_DESTROY, 7).closes());
        assert!(raw(EVENT_DIALOGEND, 7).closes());
        assert!(raw(EVENT_DIALOGSTART, 7).opens());
        // A part of a window destroyed: a flood nobody tells.
        assert!(!cell(EVENT_DESTROY, 7, 3).matters());
        assert!(!cell(EVENT_DESTROY, 7, 3).closes());
        assert!(cell(EVENT_VALUECHANGE, 7, 3).matters());
        assert!(cell(EVENT_VALUECHANGE, 7, 3).element());
        assert!(raw(EVENT_FOREGROUND, 7).matters());
    }

    #[test]
    fn a_full_queue_never_loses_a_window_going() {
        let mut q = VecDeque::new();
        assert!(enqueue(&mut q, raw(EVENT_SHOW, 1), 3));
        assert!(enqueue(&mut q, cell(EVENT_VALUECHANGE, 9, 1), 3));
        assert!(enqueue(&mut q, cell(EVENT_VALUECHANGE, 9, 2), 3));
        // Full: an element's change is lost, and said to be.
        assert!(!enqueue(&mut q, cell(EVENT_VALUECHANGE, 9, 3), 3));
        assert_eq!(q.len(), 3);
        // A window going takes the place of the oldest element's change.
        assert!(!enqueue(&mut q, raw(EVENT_DESTROY, 1), 3));
        assert_eq!(q.len(), 3);
        assert_eq!(q.back().map(|r| r.event), Some(EVENT_DESTROY));
        assert_eq!(q.iter().filter(|r| r.element()).count(), 1);
        // With only windows queued it is held past the limit.
        assert!(!enqueue(&mut q, raw(EVENT_HIDE, 2), 3));
        assert!(enqueue(&mut q, raw(EVENT_HIDE, 3), 3));
        assert_eq!(q.len(), 4);
        assert_eq!(q.iter().filter(|r| r.closes()).count(), 3);
    }

    #[test]
    fn a_long_queue_sheds_its_oldest_element_changes_only() {
        let mut q: VecDeque<Raw> = vec![
            cell(EVENT_VALUECHANGE, 9, 1),
            raw(EVENT_DESTROY, 1),
            cell(EVENT_VALUECHANGE, 9, 2),
            cell(EVENT_VALUECHANGE, 9, 3),
        ]
        .into();
        assert_eq!(shed(&mut q, 10), 0);
        assert_eq!(shed(&mut q, 2), 2);
        let left: Vec<(u32, i32)> = q.iter().map(|r| (r.event, r.child)).collect();
        assert_eq!(left, [(EVENT_DESTROY, 0), (EVENT_VALUECHANGE, 3)]);
    }

    #[test]
    fn each_change_has_the_time_it_happened_not_the_time_it_was_read() {
        // Read 700 ms after it happened, in a round at 10 s.
        assert_eq!(happened_ms(10_000, 50_700, 50_000), 9_300);
        // The tick count wraps every 49 days.
        assert_eq!(age_ms(5, u32::MAX - 4), 10);
        // No time given, or one that can not be right, counts as now.
        assert_eq!(happened_ms(10_000, 50_700, 0), 10_000);
        assert_eq!(age_ms(1000, 2000), 0);
        assert_eq!(happened_ms(300, 50_700, 50_000), 0);
        assert_eq!(clock_before(3605, 6_900), 3599);
        assert_eq!(clock_before(2, 5_000), 86_397);
    }

    #[test]
    fn recall_forgets_the_longest_untouched_not_everything() {
        let mut r: Recall<String> = Recall::new(8);
        for n in 0..8 {
            assert_eq!(r.insert((n, 0, 0), 7, format!("v{n}")), None);
        }
        // Touched again, so it is not among the oldest.
        assert_eq!(
            r.insert((0, 0, 0), 7, "again".into()).as_deref(),
            Some("v0")
        );
        // One too many: the two longest untouched go, the rest stay.
        r.insert((100, 0, 0), 7, "new".into());
        assert_eq!(r.len(), 7);
        assert_eq!(r.insert((0, 0, 0), 7, "x".into()).as_deref(), Some("again"));
        assert_eq!(r.insert((1, 0, 0), 7, "x".into()), None);
        assert_eq!(r.insert((3, 0, 0), 7, "x".into()).as_deref(), Some("v3"));
    }

    #[test]
    fn recall_forgets_a_program_that_ended() {
        let mut r: Recall<u32> = Recall::new(100);
        r.insert((1, 0, 0), 7, 1);
        r.insert((2, 0, 0), 8, 2);
        r.forget(7);
        assert_eq!(r.len(), 1);
        // A window handle used again starts with nothing known.
        assert_eq!(r.insert((1, 0, 0), 9, 5), None);
        r.remove(&(1, 0, 0));
        assert_eq!(r.insert((1, 0, 0), 9, 6), None);
        assert!(!r.is_empty());
    }

    #[test]
    fn the_ring_forgets_a_program_no_longer_allowed() {
        let mut r = Ring::new(10);
        r.push(value(0, None, "secret"));
        let mut other = value(5000, None, "kept");
        other.pid = 8;
        r.push(other);
        r.push(Change::skipped(4));
        r.forget(7);
        let (all, lost) = r.since(0);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].to, "kept");
        assert_eq!(all[1].what, What::Skipped(4));
        // The count goes on, so a reader's place still means the same.
        assert!(!lost);
        assert_eq!(r.newest(), 3);
    }

    #[test]
    fn skipped_notes_are_each_kept() {
        let mut r = Ring::new(10);
        assert!(r.push(Change::skipped(4)));
        assert!(r.push(Change::skipped(4)));
        assert_eq!(r.since(0).0.len(), 2);
    }

    #[test]
    fn a_text_wait_matches_only_what_a_change_brought() {
        let u = |t: &str| Until {
            text: Some(t.into()),
            ..Default::default()
        };
        let hit = |t: &str, c: &Change| verdict(&u(t), &[], &[c.clone()], 0, 0, None).is_some();
        // The title of the window a value changed in did not appear.
        let v = value(0, Some("a"), "b");
        assert!(!hit("Settings", &v));
        // Nor did the element's own name, nor the value it had before.
        assert!(!hit("IP address", &v));
        assert!(!hit("a", &v));
        assert!(hit("b", &v));
        // A state change brings no text at all.
        let mut s = Change::of(What::State, &win("Online"));
        s.name = "Online".into();
        s.states = Some((Some(0), STATE_CHECKED));
        assert!(!hit("Online", &s));
        // The front moving to a window does not make its title appear.
        assert!(!hit("Settings", &Change::of(What::Front, &win("Settings"))));
        assert!(!hit(
            "Settings",
            &Change::of(What::Closed, &win("Settings"))
        ));
        // A window shown, a new title and a new name do.
        assert!(hit("Settings", &Change::of(What::Shown, &win("Settings"))));
        let mut t = Change::of(What::Title, &win("Done"));
        t.from = Some("Working".into());
        t.to = "Done".into();
        assert!(hit("done", &t));
        assert!(!hit("working", &t));
        let mut n = Change::of(What::Name, &win("w"));
        n.from = Some("Loading".into());
        n.to = "Ready".into();
        assert!(hit("ready", &n));
        assert!(!hit("loading", &n));
    }

    #[test]
    fn a_text_wait_never_matches_a_skipped_note_or_a_protected_value() {
        let u = Until {
            text: Some("5".into()),
            ..Default::default()
        };
        let note = Change::skipped(5);
        assert_eq!(verdict(&u, &[], &[note.clone()], 0, 0, None), None);
        assert_eq!(note.brought(), None);
        assert_eq!(note.title, "");
        let mut p = value(0, None, "");
        p.hidden = true;
        let any = Until {
            text: Some("IP".into()),
            ..Default::default()
        };
        assert_eq!(verdict(&any, &[], &[p.clone()], 0, 0, None), None);
        assert_eq!(
            brief(&p),
            "Edit \"IP address\" changed in \"Settings\" (its value is protected and was not \
             read)"
        );
    }

    #[test]
    fn a_silent_watcher_is_stalled_and_nothing_is_quiet_then() {
        assert_eq!(stall(1000, 1100), None);
        assert_eq!(stall(1000, 1000 + STALLED_MS - 1), None);
        assert_eq!(stall(1000, 1000 + STALLED_MS), Some(STALLED_MS / 1000));
        assert_eq!(stall(1000, 8500), Some(7));
        // A beat stamped a moment after the time was taken is not a stall.
        assert_eq!(stall(1200, 1000), None);
        let u = Until {
            quiet_ms: Some(1000),
            ..Default::default()
        };
        assert!(verdict(&u, &[], &[], 0, 5000, None).is_some());
        assert_eq!(verdict(&u, &[], &[], 0, 5000, Some(4)), None);
        // What does not lean on the watcher still holds.
        let both = Until {
            quiet_ms: Some(1000),
            appears: Some("Save".into()),
            ..Default::default()
        };
        assert!(verdict(&both, &["Save As".to_string()], &[], 0, 5000, Some(4)).is_some());
        assert_eq!(
            stall_text(7),
            "the watcher has not answered for 7 s (a watched program may be hung)"
        );
        let said = changes_text(&[], 4, 4, false, " in app.exe", Some(7));
        assert!(said.contains("has not answered for 7 s"), "{said}");
        assert!(!said.contains("Nothing changed"), "{said}");
        let part = changes_part(&[], false, "", Some(7));
        assert!(part.contains("has not answered for 7 s"), "{part}");
        assert!(!part.contains("Nothing changed"), "{part}");
    }

    #[test]
    fn quiet_counts_from_the_latest_change_whatever_their_order() {
        let u = Until {
            quiet_ms: Some(1000),
            ..Default::default()
        };
        let seen = [value(1900, None, "late"), value(1200, None, "early")];
        assert_eq!(verdict(&u, &[], &seen, 0, 2500, None), None);
        assert!(verdict(&u, &[], &seen, 0, 2900, None).is_some());
    }

    #[test]
    fn a_wait_tells_what_came_before_it_apart_from_what_it_saw() {
        let numbered = |seq: u64, to: &str| {
            let mut c = value(seq * 5000, None, to);
            c.seq = seq;
            c
        };
        // The session read up to #2; the wait began at #4 and ended at #6.
        let list = vec![numbered(3, "c"), numbered(4, "d"), numbered(6, "f")];
        let (before, after) = split_at(list, 4);
        assert_eq!(
            before.iter().map(|c| c.seq).collect::<Vec<_>>(),
            [3, 4],
            "untold changes from before the wait are told, not dropped"
        );
        assert_eq!(after.iter().map(|c| c.seq).collect::<Vec<_>>(), [6]);
        // Only what came after decides the verdict.
        let u = Until {
            text: Some("d".into()),
            ..Default::default()
        };
        assert_eq!(verdict(&u, &[], &after, 0, 0, None), None);
        let said = meanwhile_text(&before, &after, false, None, false);
        let lines: Vec<&str> = said.lines().collect();
        assert_eq!(lines[0], "");
        assert_eq!(lines[1], "Before the wait:");
        assert!(lines[2].contains("\"c\""));
        assert!(lines[3].contains("\"d\""));
        assert_eq!(lines[4], "Meanwhile:");
        assert!(lines[5].contains("\"f\""));
        assert_eq!(lines.len(), 6);
    }

    #[test]
    fn a_wait_says_what_it_left_out_and_what_was_lost() {
        assert_eq!(
            meanwhile_text(&[], &[], false, None, false),
            " Nothing changed meanwhile."
        );
        assert_eq!(meanwhile_text(&[], &[], false, None, true), "");
        let one = meanwhile_text(&[], &[value(0, None, "x")], false, None, false);
        assert!(one.starts_with(" Meanwhile:\n"), "{one}");
        let many: Vec<Change> = (0..MOST_LINES + 20).map(|_| value(0, None, "x")).collect();
        let said = meanwhile_text(&many[..30], &many, true, None, false);
        assert!(said.contains("pushed out before they were told"), "{said}");
        assert!(said.contains("(20 earlier changes left out.)"), "{said}");
        // No room for the lines from before the wait: said, not dropped.
        assert!(
            said.contains("Before the wait:\n(30 earlier changes left out.)"),
            "{said}"
        );
        assert_eq!(said.matches("value now").count(), MOST_LINES);
        let before = meanwhile_text(&many[..3], &[], false, None, false);
        assert!(before.ends_with("\nNothing changed meanwhile."), "{before}");
        let stuck = meanwhile_text(&[], &[], false, Some(9), true);
        assert!(stuck.contains("has not answered for 9 s"), "{stuck}");
        assert!(!stuck.contains("Nothing changed"), "{stuck}");
    }

    #[test]
    fn a_since_past_the_newest_says_the_numbering_started_over() {
        assert_eq!(cursor_from(None, 12, 20), (12, None));
        assert_eq!(cursor_from(Some(5), 12, 20), (5, None));
        assert_eq!(cursor_from(Some(20), 12, 20), (20, None));
        let (from, note) = cursor_from(Some(400), 0, 3);
        assert_eq!(from, 0);
        let note = note.unwrap();
        assert!(note.contains("no change #400"), "{note}");
        assert!(note.contains("started over"), "{note}");
        assert!(note.contains("#3"), "{note}");
    }

    #[test]
    fn roles_read_as_the_snapshot_names_them() {
        assert_eq!(role_name(42), "Edit");
        assert_eq!(role_name(43), "Button");
        assert_eq!(role_name(44), "CheckBox");
        assert_eq!(role_name(36), "TreeItem");
        assert_eq!(role_name(999), "Element");
    }

    #[test]
    fn state_changes_say_what_came_and_went() {
        assert_eq!(
            state_change(Some(0), STATE_CHECKED).as_deref(),
            Some("now checked")
        );
        assert_eq!(
            state_change(Some(STATE_CHECKED | STATE_UNAVAILABLE), STATE_CHECKED).as_deref(),
            Some("no longer disabled")
        );
        assert_eq!(
            state_change(Some(STATE_COLLAPSED), STATE_EXPANDED).as_deref(),
            Some("now expanded, no longer collapsed")
        );
        // Focus (4) and hover (0x80) come and go unsaid.
        assert_eq!(state_change(Some(0), 0x4 | 0x80), None);
        assert_eq!(state_change(None, STATE_SELECTED), None);
        assert_eq!(state_change(None, STATE_BUSY).as_deref(), Some("now busy"));
    }

    #[test]
    fn the_clock_reads_hours_minutes_seconds() {
        assert_eq!(clock_text(14 * 3600 + 2 * 60 + 11), "14:02:11");
        assert_eq!(clock_text(5), "00:00:05");
    }

    #[test]
    fn lines_say_what_happened_in_few_words() {
        let mut d = Change::of(What::Shown, &win("Synchronization"));
        d.dialog = true;
        d.clock = 14 * 3600 + 2 * 60 + 11;
        assert_eq!(
            line(&d),
            "14:02:11 dialog shown \"Synchronization\" id=66 (app.exe)"
        );
        d.what = What::Closed;
        assert_eq!(brief(&d), "dialog closed \"Synchronization\" id=66");
        assert_eq!(
            brief(&value(0, Some("10.0.0.3"), "10.0.0.5")),
            "Edit \"IP address\" value \"10.0.0.3\" -> \"10.0.0.5\" in \"Settings\""
        );
        assert_eq!(
            brief(&value(0, None, "10.0.0.5")),
            "Edit \"IP address\" value now \"10.0.0.5\" in \"Settings\""
        );
        let mut blind = Change::of(What::Value, &win("Admin"));
        blind.key = Some((1, 1, 1));
        assert_eq!(brief(&blind), "something changed its value in \"Admin\"");
        let mut t = Change::of(What::Title, &win("b"));
        t.from = Some("a".into());
        t.to = "b".into();
        assert_eq!(brief(&t), "window id=66 title \"a\" -> \"b\"");
        let mut n = Change::of(What::Name, &win("w"));
        n.role = "Text".into();
        n.from = Some("Loading".into());
        n.to = "Ready".into();
        assert_eq!(brief(&n), "Text named \"Loading\" -> \"Ready\" in \"w\"");
        let mut s = Change::of(What::State, &win("w"));
        s.role = "CheckBox".into();
        s.name = "Online".into();
        s.states = Some((Some(0), STATE_CHECKED));
        assert_eq!(brief(&s), "CheckBox \"Online\" now checked in \"w\"");
        assert_eq!(
            brief(&Change::of(What::Skipped(9), &win("w"))),
            "9 more changes not recorded, too many came at once"
        );
    }

    #[test]
    fn scope_is_one_program_or_all() {
        let c = value(0, None, "x");
        assert!(in_scope(&c, None));
        assert!(in_scope(&c, Some(7)));
        assert!(!in_scope(&c, Some(8)));
        assert!(in_scope(&Change::of(What::Skipped(1), &win("w")), Some(8)));
    }

    #[test]
    fn the_changes_answer_lists_newest_last_and_says_where_to_go_on() {
        assert_eq!(
            changes_text(&[], 4, 4, false, " in app.exe", None),
            "Nothing changed in app.exe since #4. The next takeover_changes starts after #4 by \
             itself."
        );
        let mut a = value(0, None, "1");
        a.seq = 5;
        let mut b = Change::of(What::Front, &win("Settings"));
        b.seq = 6;
        let text = changes_text(&[a, b], 4, 6, true, "", None);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains("pushed out"));
        assert!(lines[1].contains("value now \"1\""));
        assert!(lines[2].contains("came to the front"));
        assert!(lines[3].starts_with("2 changes, up to #6."));
    }

    #[test]
    fn the_administrator_part_says_no_numbers() {
        assert_eq!(
            changes_part(&[], false, " in the watched programs", None),
            "Nothing changed in the watched programs since the last look."
        );
        let mut a = value(0, None, "1");
        a.seq = 9;
        let text = changes_part(&[a], true, "", None);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains("pushed out"));
        assert!(lines[1].contains("value now \"1\""));
        assert_eq!(lines[2], "1 change.");
        assert!(!text.contains('#'));
    }

    #[test]
    fn a_digest_puts_windows_first_and_counts_the_rest() {
        assert_eq!(digest(&[], 2), "");
        let mut shown = Change::of(What::Shown, &win("Save As"));
        shown.dialog = true;
        let list = vec![
            value(0, None, "a"),
            shown,
            value(0, None, "b"),
            Change::of(What::MenuClosed, &win("w")),
        ];
        assert_eq!(
            digest(&list, 2),
            " Then: dialog shown \"Save As\" id=66 (app.exe); Edit \"IP address\" value now \
             \"a\" in \"Settings\"; 1 more (takeover_changes has them)."
        );
    }

    #[test]
    fn a_wait_ends_on_the_first_condition_that_holds() {
        let titles = vec!["Untitled - Notepad".to_string(), "Save As".to_string()];
        let mut u = Until {
            appears: Some("save as".into()),
            ..Default::default()
        };
        assert!(verdict(&u, &titles, &[], 0, 0, None)
            .unwrap()
            .contains("\"Save As\""));
        u.appears = Some("Print".into());
        assert_eq!(verdict(&u, &titles, &[], 0, 0, None), None);

        let u = Until {
            gone: Some("Save".into()),
            ..Default::default()
        };
        assert_eq!(verdict(&u, &titles, &[], 0, 0, None), None);
        assert!(verdict(&u, &titles[..1], &[], 0, 0, None).is_some());

        let u = Until {
            text: Some("ONLINE".into()),
            ..Default::default()
        };
        assert_eq!(
            verdict(&u, &[], &[value(0, None, "offline?")], 0, 0, None),
            None
        );
        assert!(verdict(&u, &[], &[value(0, None, "Online")], 0, 0, None)
            .unwrap()
            .starts_with("\"ONLINE\" appeared: Edit"));

        let u = Until {
            quiet_ms: Some(1000),
            ..Default::default()
        };
        assert_eq!(verdict(&u, &[], &[], 500, 1200, None), None);
        assert!(verdict(&u, &[], &[], 500, 1500, None).is_some());
        assert_eq!(
            verdict(&u, &[], &[value(1400, None, "x")], 500, 2000, None),
            None
        );
        assert!(verdict(&u, &[], &[value(1400, None, "x")], 500, 2400, None).is_some());
        assert!(!u.is_empty());
        assert!(Until::default().is_empty());
    }

    /// A small window of our own that changes by itself: a field's value,
    /// a box ticked, a label, a dialog shown and closed, a new title.
    pub(crate) const FORM: &str = r#"
Add-Type -AssemblyName System.Windows.Forms
$f = New-Object Windows.Forms.Form
$f.Text = 'Horadric watch test'
$t = New-Object Windows.Forms.TextBox; $t.AccessibleName = 'IP address'; $t.Text = '10.0.0.3'; $t.Top = 10
$c = New-Object Windows.Forms.CheckBox; $c.Text = 'Online'; $c.Top = 40
$l = New-Object Windows.Forms.Label; $l.Text = 'Loading'; $l.Top = 70
$f.Controls.AddRange(@($t, $c, $l))
$script:step = 0
$script:d = $null
$timer = New-Object Windows.Forms.Timer; $timer.Interval = 700
$timer.Add_Tick({
  $script:step++
  switch ($script:step) {
    3 { $t.Text = '10.0.0.5' }
    4 { $c.Checked = $true }
    5 { $l.Text = 'Ready' }
    6 { $script:d = New-Object Windows.Forms.Form; $script:d.Text = 'Watch dialog'; $script:d.Show($f) }
    8 { $script:d.Close() }
    9 { $f.Text = 'Horadric watch test done' }
    11 { $timer.Stop(); $f.Close() }
  }
})
$timer.Start()
[Windows.Forms.Application]::Run($f)
"#;

    /// Not a check but a run to read: starts the form above, as
    /// administrator when `ELEVATED=1` (on a machine that elevates without
    /// asking), watches it from this process, and prints what was seen.
    /// `ELEVATED=1 cargo test -p horadric watch_a_form_by_hand`, with the
    /// ignored and nocapture flags handed on to the test binary.
    #[test]
    #[ignore]
    fn watch_a_form_by_hand() {
        let script = std::env::temp_dir().join("horadric-watch-form.ps1");
        std::fs::write(&script, FORM).unwrap();
        let elevated = std::env::var("ELEVATED").is_ok_and(|v| v == "1");
        let start = format!(
            "(Start-Process powershell {} -PassThru -ArgumentList '-NoProfile','-STA',\
             '-ExecutionPolicy','Bypass','-File','{}').Id",
            if elevated { "-Verb RunAs" } else { "" },
            script.display()
        );
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &start])
            .output()
            .unwrap();
        let pid: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        println!("form pid {pid}, elevated {elevated}");
        // The form takes a moment to show its window.
        let mut shown = None;
        for _ in 0..50 {
            shown = windows_of(pid).into_iter().next();
            if shown.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        let shown = shown.expect("the form came up");
        follow(&shown);
        assert!(following(pid), "{:?}", trouble(pid));
        thread::sleep(Duration::from_secs(10));
        flush();
        let (list, _, _) = since(0, None);
        for c in &list {
            println!("{}", line(c));
        }
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output();
        let _ = std::fs::remove_file(&script);
    }
}
