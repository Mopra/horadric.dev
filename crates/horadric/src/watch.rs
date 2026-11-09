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

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{
    GetCurrentThreadId, GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    AccessibleObjectFromEvent, IAccessible, SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetClassNameW, GetForegroundWindow, GetMessageW,
    GetWindowThreadProcessId, IsWindowVisible, PeekMessageW, PostThreadMessageW, SetTimer, GA_ROOT,
    MSG, PM_NOREMOVE, WM_APP,
};

use crate::desktop::{info, short, Window};

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
        c.seq = self.next;
        self.next += 1;
        self.items.push_back(c);
        while self.items.len() > self.most {
            if let Some(gone) = self.items.pop_front() {
                self.lost_upto = gone.seq;
            }
        }
        true
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
}

impl Intake {
    /// Whether a change at `at_ms` is recorded, and when a new second
    /// begins, how many the last one skipped.
    pub fn admit(&mut self, at_ms: u64) -> (bool, Option<u32>) {
        let second = at_ms / 1000;
        let mut said = None;
        if second != self.second {
            if self.skipped > 0 {
                said = Some(self.skipped);
            }
            *self = Intake {
                second,
                count: 0,
                skipped: 0,
            };
        }
        if self.count >= MOST_PER_SECOND {
            self.skipped += 1;
            return (false, said);
        }
        self.count += 1;
        (true, said)
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

/// The answer to `desktop_changes`: the lines, newest last, and the
/// change to ask after next time.
pub fn changes_text(list: &[Change], since: u64, newest: u64, lost: bool, scope: &str) -> String {
    let mut out = String::new();
    if lost {
        out.push_str(&format!(
            "(Some changes after #{since} were pushed out: only the last {KEPT} are kept.)\n"
        ));
    }
    if list.is_empty() {
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
        " The next desktop_changes starts after #{newest} by itself."
    ));
    out
}

/// The changes the helper read in programs that run as administrator, as
/// part of another answer. Its ring counts on its own, so no numbers: the
/// agent's `since` names the other ring's.
pub fn changes_part(list: &[Change], lost: bool, scope: &str) -> String {
    let mut out = String::new();
    if lost {
        out.push_str(&format!(
            "(Some changes were pushed out: only the last {KEPT} are kept.)\n"
        ));
    }
    if list.is_empty() {
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
        said.push(format!("{more} more (desktop_changes has them)"));
    }
    format!(" Then: {}.", said.join("; "))
}

/// What `desktop_wait` waits for. It ends when any of them holds.
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
pub fn verdict(
    u: &Until,
    titles: &[String],
    seen: &[Change],
    started_ms: u64,
    now_ms: u64,
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
        let hit = seen.iter().find(|c| {
            !matches!(c.what, What::Closed | What::MenuClosed)
                && (has(&c.to, x) || has(&c.name, x) || has(&c.title, x))
        });
        if let Some(c) = hit {
            return Some(format!("{x:?} appeared: {}", brief(c)));
        }
    }
    if let Some(q) = u.quiet_ms {
        let last = seen.last().map_or(started_ms, |c| c.at_ms.max(started_ms));
        if now_ms.saturating_sub(last) >= q {
            return Some(format!("nothing changed for {q} ms"));
        }
    }
    None
}

// The rest touches Windows, and is tried by hand rather than tested.

/// What the watching thread shares with the tools.
struct Shared {
    ring: Ring,
    /// The programs hooked.
    followed: HashSet<u32>,
    /// The programs to hook next.
    asked: Vec<u32>,
    /// How many times the thread has caught up, for waiting on it.
    rounds: u64,
}

struct Watch {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    thread: u32,
    start: Instant,
}

static WATCH: OnceLock<Watch> = OnceLock::new();

fn watch() -> &'static Watch {
    WATCH.get_or_init(|| {
        let shared = Arc::new((
            Mutex::new(Shared {
                ring: Ring::new(KEPT),
                followed: HashSet::new(),
                asked: Vec::new(),
                rounds: 0,
            }),
            Condvar::new(),
        ));
        let start = Instant::now();
        let (tx, rx) = mpsc::channel();
        let theirs = shared.clone();
        let spawned = thread::Builder::new()
            .name("desktop-watch".into())
            .spawn(move || run(theirs, start, tx));
        let thread = match spawned {
            Ok(_) => rx.recv_timeout(Duration::from_secs(5)).unwrap_or(0),
            Err(_) => 0,
        };
        Watch {
            shared,
            thread,
            start,
        }
    })
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

/// Starts watching program `pid`, and returns once it is.
pub fn follow(pid: u32) {
    let w = watch();
    // The thread never started: nothing would ever hook it, and waiting
    // each call would only slow every action down.
    if w.thread == 0 {
        return;
    }
    let (lock, changed) = &*w.shared;
    let Ok(mut s) = lock.lock() else { return };
    if s.followed.contains(&pid) {
        return;
    }
    if !s.asked.contains(&pid) {
        s.asked.push(pid);
    }
    poke(w.thread);
    let _ = changed.wait_timeout_while(s, Duration::from_secs(2), |s| !s.followed.contains(&pid));
}

/// Whether program `pid` is watched.
pub fn following(pid: u32) -> bool {
    WATCH
        .get()
        .and_then(|w| w.shared.0.lock().ok().map(|s| s.followed.contains(&pid)))
        .unwrap_or(false)
}

/// Whether anything is watched at all.
pub fn any_followed() -> bool {
    WATCH
        .get()
        .and_then(|w| w.shared.0.lock().ok().map(|s| !s.followed.is_empty()))
        .unwrap_or(false)
}

/// The last change recorded.
pub fn newest() -> u64 {
    WATCH
        .get()
        .and_then(|w| w.shared.0.lock().ok().map(|s| s.ring.newest()))
        .unwrap_or(0)
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
    poke(w.thread);
    // One round may be under way already with older events in hand.
    let _ = changed.wait_timeout_while(s, Duration::from_millis(600), |s| s.rounds < from + 2);
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
    let Some(w) = WATCH.get() else {
        return (Vec::new(), false, 0);
    };
    let Ok(mut s) = w.shared.0.lock() else {
        return (Vec::new(), false, 0);
    };
    let (list, lost) = s.ring.since(seq);
    let newest = s.ring.newest();
    if handed {
        s.ring.hand_out(newest);
    }
    let list = list.into_iter().filter(|c| in_scope(c, pid)).collect();
    (list, lost, newest)
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
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Raw {
    event: u32,
    hwnd: isize,
    object: i32,
    child: i32,
}

/// The most events held before the thread reads them.
const MOST_RAW: usize = 5000;

thread_local! {
    static RAW: RefCell<VecDeque<Raw>> = const { RefCell::new(VecDeque::new()) };
    static POKED: Cell<bool> = const { Cell::new(false) };
}

unsafe extern "system" fn on_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    _time: u32,
) {
    RAW.with(|r| {
        let mut r = r.borrow_mut();
        if r.len() < MOST_RAW {
            r.push_back(Raw {
                event,
                hwnd: hwnd.0 as isize,
                object,
                child,
            });
        }
    });
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

/// What the watching thread keeps to itself.
struct Local {
    windows: HashMap<isize, Known>,
    values: HashMap<(isize, i32, i32), String>,
    names: HashMap<(isize, i32, i32), String>,
    states: HashMap<(isize, i32, i32), u32>,
    intake: Intake,
    start: Instant,
    /// Each followed program's hooks, removed when it ends or the thread
    /// does.
    hooks: HashMap<u32, Vec<HWINEVENTHOOK>>,
    /// Rounds since the followed programs were last checked for having
    /// ended.
    unchecked: u32,
}

/// How many rounds, of a tenth of a second at least, between looks for
/// programs that ended.
const CHECK_ENDED: u32 = 50;

/// Classes of windows that come and go with no meaning to the agent.
const NOISE_CLASSES: &[&str] = &[
    "tooltips_class32",
    "#32768",
    "SysShadow",
    "IME",
    "MSCTFIME UI",
    "ComboLBox",
];

fn run(shared: Arc<(Mutex<Shared>, Condvar)>, start: Instant, ready: mpsc::Sender<u32>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let mut msg = MSG::default();
        // A thread has a queue for posted messages once it has asked.
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        let _ = ready.send(GetCurrentThreadId());
        // A poke the hooks post can be missed, so the thread also looks
        // ten times a second.
        SetTimer(None, 0, 100, None);
        let mut local = Local {
            windows: HashMap::new(),
            values: HashMap::new(),
            names: HashMap::new(),
            states: HashMap::new(),
            intake: Intake::default(),
            start,
            hooks: HashMap::new(),
            unchecked: 0,
        };
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            round(&shared, &mut local);
        }
        for h in local.hooks.into_values().flatten() {
            let _ = UnhookWinEvent(h);
        }
    }
}

/// Whether program `pid` has ended. One that can not be opened for some
/// other reason than being gone is taken as running.
fn ended(pid: u32) -> bool {
    const STILL_ACTIVE: u32 = 259;
    unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(p) => {
                let mut code = 0u32;
                let read = GetExitCodeProcess(p, &mut code).is_ok();
                let _ = CloseHandle(p);
                read && code != STILL_ACTIVE
            }
            Err(e) => e.code() == ERROR_INVALID_PARAMETER.to_hresult(),
        }
    }
}

/// Unhooks the programs that ended, so their numbers, once Windows hands
/// them to another program, are hooked afresh.
fn drop_ended(shared: &Arc<(Mutex<Shared>, Condvar)>, local: &mut Local) {
    local.unchecked += 1;
    if local.unchecked < CHECK_ENDED {
        return;
    }
    local.unchecked = 0;
    let gone: Vec<u32> = local.hooks.keys().copied().filter(|&p| ended(p)).collect();
    if gone.is_empty() {
        return;
    }
    for pid in &gone {
        for h in local.hooks.remove(pid).unwrap_or_default() {
            let _ = unsafe { UnhookWinEvent(h) };
        }
    }
    if let Ok(mut s) = shared.0.lock() {
        for pid in &gone {
            s.followed.remove(pid);
        }
    }
}

/// Hooks newly asked programs and reads the events that came.
fn round(shared: &Arc<(Mutex<Shared>, Condvar)>, local: &mut Local) {
    let (lock, changed) = &**shared;
    let asked: Vec<u32> = lock
        .lock()
        .map(|mut s| std::mem::take(&mut s.asked))
        .unwrap_or_default();
    drop_ended(shared, local);
    for pid in &asked {
        let hooks = local.hooks.entry(*pid).or_default();
        // Asked twice before the first round answered: hooked once.
        if !hooks.is_empty() {
            continue;
        }
        for (min, max) in HOOKED {
            let h = unsafe {
                SetWinEventHook(
                    min,
                    max,
                    None,
                    Some(on_event),
                    *pid,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            };
            if !h.is_invalid() {
                hooks.push(h);
            }
        }
        for w in windows_of(*pid) {
            local.windows.insert(
                w.hwnd,
                Known {
                    window: w,
                    visible: true,
                    shown: None,
                },
            );
        }
    }
    POKED.set(false);
    let mut raw: Vec<Raw> = RAW.with(|r| r.borrow_mut().drain(..).collect());
    // Of a burst on one element only the last is read: it has the value
    // the others led to.
    let mut kept = Vec::with_capacity(raw.len());
    let mut had = HashSet::new();
    while let Some(r) = raw.pop() {
        let element = matches!(
            r.event,
            EVENT_NAMECHANGE | EVENT_VALUECHANGE | EVENT_STATECHANGE
        );
        if !element || had.insert(r) {
            kept.push(r);
        }
    }
    kept.reverse();
    // Windows shown first: the elements filled in as one is made say so
    // before it shows, and are known as its making only once it is.
    kept.sort_by_key(|r| !matches!(r.event, EVENT_SHOW | EVENT_DIALOGSTART));
    let mut fresh = Vec::new();
    for r in kept {
        if let Some(c) = read(local, r) {
            fresh.push(c);
        }
    }
    if local.values.len() > 4000 {
        local.values.clear();
        local.names.clear();
        local.states.clear();
    }
    let clock = unsafe {
        let t = GetLocalTime();
        t.wHour as u32 * 3600 + t.wMinute as u32 * 60 + t.wSecond as u32
    };
    if let Ok(mut s) = lock.lock() {
        for mut c in fresh {
            c.at_ms = local.start.elapsed().as_millis() as u64;
            c.clock = clock;
            let (take, skipped) = local.intake.admit(c.at_ms);
            if let Some(n) = skipped {
                let mut note = c.clone();
                note.what = What::Skipped(n);
                note.key = None;
                note.to = n.to_string();
                s.ring.push(note);
            }
            if take {
                s.ring.push(c);
            }
        }
        s.followed.extend(asked);
        s.rounds += 1;
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
        .filter(|&h| {
            let mut p = 0;
            unsafe { GetWindowThreadProcessId(HWND(h as *mut _), Some(&mut p)) };
            p == pid && unsafe { IsWindowVisible(HWND(h as *mut _)) }.as_bool()
        })
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

/// The top level window `h` is in, named, known ones from memory.
fn top_of(local: &Local, h: isize) -> Window {
    let root = unsafe { GetAncestor(HWND(h as *mut _), GA_ROOT) };
    let root = if root.is_invalid() {
        h
    } else {
        root.0 as isize
    };
    match local.windows.get(&root) {
        Some(k) => k.window.clone(),
        None => info(root),
    }
}

/// The change one event makes, none when it says nothing new.
fn read(local: &mut Local, r: Raw) -> Option<Change> {
    let whole = r.object == OBJID_WINDOW && r.child == CHILDID_SELF;
    match r.event {
        EVENT_SHOW | EVENT_DIALOGSTART if whole || r.event == EVENT_DIALOGSTART => {
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
        EVENT_HIDE | EVENT_DESTROY | EVENT_DIALOGEND if whole || r.event == EVENT_DIALOGEND => {
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
        EVENT_NAMECHANGE if whole && is_top(r.hwnd) => {
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
    if r.object == OBJID_WINDOW && r.child == CHILDID_SELF {
        r.object = OBJID_CLIENT;
    }
    let root = unsafe { GetAncestor(HWND(r.hwnd as *mut _), GA_ROOT) }.0 as isize;
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
    // A window that runs as administrator answers nothing to a reader that
    // does not, so the change is kept without what it was: still worth
    // knowing for a wait until things are quiet.
    let Some((role, name, value, state)) = (unsafe { read_element(r) }) else {
        return (what == What::Value).then_some(c);
    };
    c.role = role_name(role).to_string();
    c.name = name.clone();
    match what {
        What::Name => {
            let before = local.names.insert(key, name.clone());
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
        What::Value => {
            let value = value?;
            let before = local.values.insert(key, value.clone());
            if before.as_deref() == Some(value.as_str()) {
                return None;
            }
            c.from = before;
            c.to = value;
        }
        _ => {
            let before = local.states.insert(key, state);
            if before == Some(state) {
                return None;
            }
            state_change(before, state)?;
            c.states = Some((before, state));
        }
    }
    let first = c.from.is_none() && c.states.is_none_or(|(b, _)| b.is_none());
    (!(making && first)).then_some(c)
}

/// The role, name, value and state of the element an event names.
unsafe fn read_element(r: Raw) -> Option<(i32, String, Option<String>, u32)> {
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
        let role = acc
            .get_accRole(&child)
            .ok()
            .and_then(|v| i32::try_from(&v).ok())
            .unwrap_or(0);
        let name = acc
            .get_accName(&child)
            .map(|b| b.to_string())
            .unwrap_or_default();
        let value = acc.get_accValue(&child).ok().map(|b| b.to_string());
        let state = acc
            .get_accState(&child)
            .ok()
            .and_then(|v| i32::try_from(&v).ok())
            .unwrap_or(0) as u32;
        Some((role, name, value, state))
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
            assert_eq!(i.admit(1000 + n as u64), (true, None));
        }
        assert_eq!(i.admit(1500), (false, None));
        assert_eq!(i.admit(1600), (false, None));
        assert_eq!(i.admit(2100), (true, Some(2)));
        assert_eq!(i.admit(2200), (true, None));
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
            changes_text(&[], 4, 4, false, " in app.exe"),
            "Nothing changed in app.exe since #4. The next desktop_changes starts after #4 by \
             itself."
        );
        let mut a = value(0, None, "1");
        a.seq = 5;
        let mut b = Change::of(What::Front, &win("Settings"));
        b.seq = 6;
        let text = changes_text(&[a, b], 4, 6, true, "");
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains("pushed out"));
        assert!(lines[1].contains("value now \"1\""));
        assert!(lines[2].contains("came to the front"));
        assert!(lines[3].starts_with("2 changes, up to #6."));
    }

    #[test]
    fn the_administrator_part_says_no_numbers() {
        assert_eq!(
            changes_part(&[], false, " in the watched programs"),
            "Nothing changed in the watched programs since the last look."
        );
        let mut a = value(0, None, "1");
        a.seq = 9;
        let text = changes_part(&[a], true, "");
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
             \"a\" in \"Settings\"; 1 more (desktop_changes has them)."
        );
    }

    #[test]
    fn a_wait_ends_on_the_first_condition_that_holds() {
        let titles = vec!["Untitled - Notepad".to_string(), "Save As".to_string()];
        let mut u = Until {
            appears: Some("save as".into()),
            ..Default::default()
        };
        assert!(verdict(&u, &titles, &[], 0, 0)
            .unwrap()
            .contains("\"Save As\""));
        u.appears = Some("Print".into());
        assert_eq!(verdict(&u, &titles, &[], 0, 0), None);

        let u = Until {
            gone: Some("Save".into()),
            ..Default::default()
        };
        assert_eq!(verdict(&u, &titles, &[], 0, 0), None);
        assert!(verdict(&u, &titles[..1], &[], 0, 0).is_some());

        let u = Until {
            text: Some("ONLINE".into()),
            ..Default::default()
        };
        assert_eq!(verdict(&u, &[], &[value(0, None, "offline?")], 0, 0), None);
        assert!(verdict(&u, &[], &[value(0, None, "Online")], 0, 0)
            .unwrap()
            .starts_with("\"ONLINE\" appeared: Edit"));

        let u = Until {
            quiet_ms: Some(1000),
            ..Default::default()
        };
        assert_eq!(verdict(&u, &[], &[], 500, 1200), None);
        assert!(verdict(&u, &[], &[], 500, 1500).is_some());
        assert_eq!(verdict(&u, &[], &[value(1400, None, "x")], 500, 2000), None);
        assert!(verdict(&u, &[], &[value(1400, None, "x")], 500, 2400).is_some());
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
        follow(pid);
        assert!(following(pid));
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
