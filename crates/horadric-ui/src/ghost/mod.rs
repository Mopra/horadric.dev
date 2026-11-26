//! What the human sees while a session drives a desktop program: a soft
//! light along the edges of every screen, a rune that travels to where
//! the next click goes before it is made, and a Thinking plate in the top
//! right corner saying what the session is about to do and what it did.
//!
//! All of it is drawn as the app draws: the runes are the twelve of
//! [`crate::sparks::RUNES`], the light round a screen is bloomed by the
//! app's own [`crate::light`] ([`frame`]), the mark lands with the sparks
//! a tool cast throws ([`mark`]), and the plate is a plate ([`plate`]).
//! On the plate and the mark activity is shown by runes. The light round
//! the screens is the one glow, which the human chose for it.
//!
//! The mark is slow on purpose. A click waits for it to arrive, two
//! seconds, and Esc held while it travels stops that click: the human
//! sees the aim before the act and has time to say no.
//!
//! Esc and the plate's Bypass listen to the human only. A session presses
//! Escape in the program it drives, and any process can post a click, so
//! both are told from the human's own by low level hooks on a thread of
//! their own ([`input`]), in for as long as control is on and no longer.
//! Esc is never taken from the program in front: a tap goes to it as ever,
//! and only a hold stops the session.
//!
//! Every window here is layered with its own alpha, topmost, never
//! activated, and the mouse goes through it. None is hidden from screen
//! captures: what takes the human's clicks must show in a recording and
//! to a remote tool. A session's own screenshot is the program's window
//! drawn by itself (`PrintWindow`), which holds none of this.
//!
//! Its state lives here, off the app, since its frames come to its own
//! window and must not wait on the app's borrow. Frames run only while
//! something moves. While control is merely on, the light round the
//! screens is moved by the compositor and costs nothing here, and a slow
//! beat moves the plate's runes, each drawn again alone. With animations
//! off nothing is drawn at all until something changes.
//!
//! The plate is the one window here the mouse can reach, and only over its
//! controls: a slider for how long the mark takes, kept across restarts,
//! and Bypass. Clicking them never activates it, so the program being
//! driven keeps the focus. Bypass lets what would be asked go unasked, for
//! a human who trusts the work and watches it: the runes and the plate
//! turn to the warning colour, each call it lets through is held for Esc
//! as a click is, and Esc held then stops the call and turns bypass off.
//! It is turned on by holding the mouse on it, starts off at every start
//! of the app and is never kept.
//!
//! Bypass is trust in the work at hand, so it ends with that work: once
//! every session it was on for has ended its turn (its `Stop`,
//! `StopFailure` or `SessionEnd`, or seen working and then resting, or
//! left waiting for the human) and no action is on its way, it turns
//! itself off and the plate says so. Only operating a program keeps it,
//! and after 30 minutes it ends whatever the sessions do.
//!
//! NOW says what the session last did in the program. When the session
//! goes on with other tools (a shell, files, the browser) or ends its turn,
//! the plate says that instead, so an old NOW is never taken for the
//! present. It names the session by its id and start too, since two
//! sessions can have the same name.
//!
//! While the mark is on its way, [`super::guard`] watches for a window
//! opening or another program taking the front, and stops the click as
//! Esc would: the point was chosen on a screen that is no longer there.

mod frame;
mod input;
mod layer;
mod mark;
mod plate;
mod rules;
mod shells;
#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use horadric_core::takeover_programs::{self, Mode, Source};
use horadric_hooks::listener::Reply;
use serde_json::json;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::DirectWrite::{IDWriteTextFormat, IDWriteTextLayout};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HDC, HMONITOR,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, ReleaseCapture, SetCapture, VK_LBUTTON, VK_RBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetSystemMetrics, KillTimer,
    PostMessageW, RegisterClassW, SetTimer, HTCLIENT, HTTRANSPARENT, MA_NOACTIVATE,
    SM_SWAPBUTTON, SPI_SETWORKAREA, WM_APP, WM_CAPTURECHANGED, WM_DISPLAYCHANGE, WM_DPICHANGED,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCHITTEST,
    WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

use self::layer::{Area, Layer};
use self::plate::{Laid, PlateScene, ShellRow, ShellsScene, Spots};
use self::rules::*;
use self::shells::*;
use super::guard;
use crate::backdrop;
use crate::render;
use crate::theme::{self, Color};
use crate::window::Shared;

pub(super) use self::rules::{turn_ended, turn_of, Turn};
pub(super) use self::shells::shell_heard;

const CLASS: PCWSTR = w!("HoradricGhost");
/// The frames of what moves, beating with the display.
const FRAME: usize = 1;
/// The beat while control is on and nothing moves but the runes.
const BEAT: usize = 2;
/// A show of it all for a dev instance, see [`show_armed`].
const SHOW: usize = 3;

/// What the guard posts to the clock window when it saw something.
const GUARD_MSG: u32 = WM_APP + 1;
/// What the input thread posts when the human held Esc.
const STOP_MSG: u32 = WM_APP + 2;
/// What the hook reader posts when a shell note waits, and what a window
/// of the overlay posts when the screens changed under it.
const WAKE_MSG: u32 = WM_APP + 4;

/// The beat while control is on: the runes move on it, at the pace a
/// tile's breathing lamp is drawn at, and the pointer is looked for on
/// it, since the plate lets the mouse in only over a control and can not
/// be told when the pointer comes. With animations off it draws nothing.
const BEAT_EVERY: Duration = crate::motion::FRAME_BREATH;
/// How often, while bypass is on, the sessions it is on for are looked at,
/// and how often the plate looks at what its session does now.
const TURNS_EVERY: Duration = Duration::from_millis(250);
/// How long after the light round the screens could not be made it is
/// tried again.
const FRAME_AGAIN: Duration = Duration::from_secs(3);
/// What a call bypass let through is told when bypass ends before it went.
const BYPASS_ENDED: &str =
    "the human stopped bypass, so this action was not made; ask again and they will be asked";
/// What a call bypass let through is told when the human's Esc can not be
/// heard, since then nothing would let the human stop it.
const NO_ESC: &str = "Horadric could not listen for the human's Esc key, so the human could \
    not stop this action and bypass did not let it through; bypass can not be used now, ask again";

/// The clock window, for the threads that wake the overlay.
static WAKE_AT: AtomicIsize = AtomicIsize::new(0);

/// Tells the overlay something waits for it, from any thread.
fn wake() {
    let hwnd = WAKE_AT.load(Ordering::Acquire);
    if hwnd != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WAKE_MSG, WPARAM(0), LPARAM(0));
        }
    }
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

/// An allowed call, as the plate and the mark show it.
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

/// A click on its way, its answer held until the mark arrives. A hold is
/// one without a point, the mark waiting where it is.
struct Glide {
    from: (f32, f32),
    to: (f32, f32),
    began: Instant,
    takes: Duration,
    held: bool,
    unasked: bool,
    /// Its number, so what the guard saw is matched to it.
    serial: u64,
    /// None for a show, which nobody waits on.
    reply: Option<Reply>,
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
    show_armed();
}

/// The glide speed for the state file.
pub(super) fn speed_saved() -> Option<u32> {
    glide_to_saved(SPEED.get())
}

/// The plate as it was last painted whole: where, and where its moving
/// parts are.
struct Geom {
    pos: (i32, i32),
    spots: Spots,
    /// The title's runes were drawn at work.
    working: bool,
    bypass: bool,
}

/// Everything shown while control runs.
struct Overlay {
    shared: Rc<Shared>,
    /// A window never shown, where the frames and the notes come.
    clock: HWND,
    /// Frames run, beating with the display.
    framing: bool,
    /// The slow beat runs, at this many milliseconds.
    beating: Option<u32>,
    /// The light round the screens, while control shows, and when it
    /// may be tried again after it could not be made.
    frame: Option<frame::Frame>,
    frame_after: Option<Instant>,
    /// The screens changed: the runes are made again, the plate placed
    /// again.
    screens_stale: bool,
    plate: Option<Layer>,
    geom: Option<Geom>,
    /// The plate's work area, its monitor's scale, and the window and
    /// point that chose the monitor.
    work: [i32; 4],
    scale: f32,
    plate_by: (isize, Option<(i32, i32)>),
    ghost: Option<Layer>,
    /// The mark's point, in physical pixels.
    ghost_at: Option<(f32, f32)>,
    ghost_name: String,
    /// The name laid out, for as long as it is the same name.
    name_laid: Option<(String, IDWriteTextLayout, f32)>,
    /// The mark must be painted again, not only moved.
    ghost_stale: bool,
    ghost_tint: Option<Color>,
    /// The last time the mark acted, for when it fades.
    ghost_acted: Instant,
    /// Stopped with Esc: it shows red and stays while control runs.
    ghost_stopped: bool,
    /// When the mark last landed, while its flourish plays.
    landed: Option<Instant>,
    glide: Option<Glide>,
    glides: u64,
    queue: VecDeque<Queued>,
    /// Watching for windows while a glide runs.
    watch: Option<guard::Watch>,
    /// The slider's knob is held.
    dragging: bool,
    hover: Option<Control>,
    /// Since when the human has held the mouse on Bypass.
    arming: Option<Instant>,
    /// What bypass let through and has not been told done yet.
    unasked: VecDeque<String>,
    /// The sessions bypass is on for, when it was turned on, until when
    /// it may last as the plate says it, and when they were last looked
    /// at.
    holders: HashMap<String, Holder>,
    bypass_at: Instant,
    bypass_until: String,
    turns_checked: Instant,
    /// When the plate last looked at what its session does now.
    now_checked: Instant,
    until: Option<Instant>,
    /// When control began, for the runes' clock, and how far it has faded
    /// in.
    began: Instant,
    shown: f32,
    ticked: Instant,
    /// When the runes were last moved on.
    runes_at: Instant,
    now: Now,
    history: VecDeque<Entry>,
    /// Each session's shell commands since control began, newest first.
    shells: HashMap<String, VecDeque<Shell>>,
    /// The commands whose end the log is still owed.
    ledger: Ledger,
    /// The shell section is open: the human folds it with its header.
    shells_open: bool,
    /// Where the shell section begins on the plate, in DIPs, for the mouse.
    shells_top: Option<f32>,
    /// The commands' monospaced face, made on the first paint.
    mono: Option<IDWriteTextFormat>,
    /// Each listed command laid out, by its serial, made once.
    laid: HashMap<u64, Laid>,
    /// The reason laid out, for as long as it says the same.
    reason_laid: Option<(String, IDWriteTextLayout)>,
    /// The plate's content changed since it was painted.
    stale: bool,
    /// A show for a dev instance runs, see [`Show`].
    show: Option<Show>,
}

thread_local! {
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

fn registered() -> bool {
    thread_local! {
        static DONE: Cell<bool> = const { Cell::new(false) };
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

/// A window of the overlay's class that is never shown.
fn unseen_window() -> windows::core::Result<HWND> {
    unsafe {
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
    }
}

/// Runs `f` on the overlay, made the first time. Nothing when its windows
/// cannot be made, since control goes on without its show.
fn with(shared: &Rc<Shared>, f: impl FnOnce(&mut Overlay)) {
    OVERLAY.with(|o| {
        let Ok(mut slot) = o.try_borrow_mut() else {
            return;
        };
        if slot.is_none() && registered() {
            match unseen_window() {
                Ok(clock) => {
                    WAKE_AT.store(clock.0 as isize, Ordering::Release);
                    let now = Instant::now();
                    *slot = Some(Overlay {
                        shared: Rc::clone(shared),
                        clock,
                        framing: false,
                        beating: None,
                        frame: None,
                        frame_after: None,
                        screens_stale: false,
                        plate: None,
                        geom: None,
                        work: [0, 0, 1280, 720],
                        scale: 1.0,
                        plate_by: (0, None),
                        ghost: None,
                        ghost_at: None,
                        ghost_name: String::new(),
                        name_laid: None,
                        ghost_stale: true,
                        ghost_tint: None,
                        ghost_acted: now,
                        ghost_stopped: false,
                        landed: None,
                        glide: None,
                        glides: 0,
                        queue: VecDeque::new(),
                        watch: None,
                        dragging: false,
                        hover: None,
                        arming: None,
                        unasked: VecDeque::new(),
                        holders: HashMap::new(),
                        bypass_at: now,
                        bypass_until: String::new(),
                        turns_checked: now,
                        now_checked: now,
                        until: None,
                        began: now,
                        shown: 0.0,
                        ticked: now,
                        runes_at: now,
                        now: Now::default(),
                        history: VecDeque::new(),
                        shells: HashMap::new(),
                        ledger: Ledger::new(),
                        shells_open: true,
                        shells_top: None,
                        mono: None,
                        laid: HashMap::new(),
                        reason_laid: None,
                        stale: true,
                        show: None,
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

/// An allowed call. With a point the mark glides there and the call is
/// answered when it arrives, or refused if the human holds Esc; without
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
        hold_shells(&call.session);
        o.until = extend(o.until, now, true);
        o.place_plate(call.hwnd, call.point);
        if bypass() && keeps_bypass(call.point.is_some(), call.unasked) {
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
        // In before the first glide starts, so the human can stop it.
        o.listen();
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

/// A session operated a program, for bypass, which only that keeps on.
/// The app knows which calls operate and says so here for each. What
/// [`keeps_bypass`] reads off a call is the part of it the overlay can
/// see for itself.
pub(super) fn operated(shared: &Rc<Shared>, session: &str) {
    with(shared, |o| {
        if bypass() {
            o.holders.entry(session.to_string()).or_default().called();
        }
    });
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

/// The wall clock `after` from now, to the minute.
fn clock_in(after: Duration) -> String {
    let t = unsafe { GetLocalTime() };
    let minutes = u64::from(t.wHour) * 60 + u64::from(t.wMinute) + after.as_secs().div_ceil(60);
    format!("{:02}:{:02}", minutes / 60 % 24, minutes % 60)
}

fn utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    horadric_core::tasks::utc(secs)
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

/// Whether the human holds the mouse's main button down this moment, as
/// the hardware says, not as a message does.
fn button_down() -> bool {
    unsafe {
        let main = if GetSystemMetrics(SM_SWAPBUTTON) != 0 {
            VK_RBUTTON
        } else {
            VK_LBUTTON
        };
        GetAsyncKeyState(main.0.into()) < 0
    }
}

impl Overlay {
    /// Bypass keeps it all up, so it is never on unseen.
    fn active(&self, now: Instant) -> bool {
        self.until.is_some_and(|u| u > now) || bypass()
    }

    /// Bypass as the overlay draws it: on, or shown as on by a show.
    fn bypass_look(&self) -> bool {
        bypass() || self.show.as_ref().is_some_and(|s| s.bypass)
    }

    fn note(&mut self, e: Entry) {
        let now = Instant::now();
        self.until = extend(self.until, now, false);
        remember(&mut self.history, e);
        self.stale = true;
        self.run();
    }

    /// Starts the frames, if they are not running. The first one decides
    /// whether they go on.
    fn run(&mut self) {
        if !self.framing {
            self.framing = true;
            self.ticked = Instant::now();
            crate::vsync::start(self.clock, FRAME);
        }
    }

    /// Runs frames while something moves and the slow beat while control
    /// is merely on, never both.
    fn pace(&mut self, moving: bool) {
        if moving {
            self.run();
        } else if self.framing {
            self.framing = false;
            crate::vsync::stop(self.clock, FRAME);
        }
        let beat = (!moving).then(|| BEAT_EVERY.as_millis() as u32);
        if beat != self.beating {
            self.beating = beat;
            unsafe {
                match beat {
                    Some(ms) => {
                        SetTimer(Some(self.clock), BEAT, ms, None);
                    }
                    None => {
                        let _ = KillTimer(Some(self.clock), BEAT);
                    }
                }
            }
        }
    }

    /// Listens for the human's Esc and clicks, for as long as control is
    /// on.
    fn listen(&mut self) {
        input::start(self.clock, STOP_MSG);
    }

    fn place_plate(&mut self, hwnd: isize, point: Option<(i32, i32)>) {
        self.plate_by = (hwnd, point);
        let monitor = unsafe {
            if hwnd != 0 {
                MonitorFromWindow(HWND(hwnd as *mut _), MONITOR_DEFAULTTONEAREST)
            } else if let Some((x, y)) = point {
                MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST)
            } else {
                MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY)
            }
        };
        if let Some((_, work)) = monitor_rects(monitor) {
            let scale = monitor_scale(monitor);
            if (scale - self.scale).abs() > f32::EPSILON {
                self.plate = None;
                self.geom = None;
            }
            if work != self.work {
                self.stale = true;
            }
            self.work = work;
            self.scale = scale;
        }
    }

    /// A screen came, went, moved or changed its scale, or the work area
    /// did: the runes are cut round the screens there are now, and the
    /// plate goes where its corner is now.
    fn follow_screens(&mut self) {
        self.screens_stale = false;
        self.frame = None;
        self.frame_after = None;
        let (hwnd, point) = self.plate_by;
        self.place_plate(hwnd, point);
        self.stale = true;
        self.ghost_stale = true;
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
            if !may_glide(q.unasked, input::armed()) {
                q.reply.send(json!({ "error": NO_ESC }));
                remember(
                    &mut self.history,
                    Entry {
                        at: now_clock(),
                        mark: Mark::Stopped,
                        what: q.what,
                        outcome: "not made: the human's Esc could not be heard".into(),
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
            if self.watch.is_none() {
                self.watch = Some(guard::watch(self.clock, GUARD_MSG));
            }
            guard::aim(self.glides + 1, q.hwnd, q.to);
            self.start_glide(
                now,
                from,
                q.to.map(|(x, y)| (x as f32, y as f32)),
                q.unasked,
                Some(q.reply),
                q.name,
                q.what,
            );
            return;
        }
        self.watch = None;
    }

    #[allow(clippy::too_many_arguments)]
    fn start_glide(
        &mut self,
        now: Instant,
        from: (f32, f32),
        to: Option<(f32, f32)>,
        unasked: bool,
        reply: Option<Reply>,
        name: String,
        what: String,
    ) {
        self.ghost_at = Some(from);
        self.ghost_name = name;
        self.ghost_stopped = false;
        self.ghost_acted = now;
        self.landed = None;
        self.ghost_stale = true;
        self.glides += 1;
        self.glide = Some(Glide {
            from,
            to: to.unwrap_or(from),
            began: now,
            takes: SPEED.get(),
            held: to.is_none(),
            unasked,
            serial: self.glides,
            reply,
            what,
        });
        // The plate gets its bar and its word on Esc.
        self.stale = true;
        self.paint_ghost(now);
    }

    /// The glide on its way is not made: `error` to its agent, `outcome`
    /// in the history.
    fn halt(&mut self, error: &str, outcome: String) {
        let now = Instant::now();
        let Some(g) = self.glide.take() else {
            return;
        };
        if let Some(r) = &g.reply {
            r.send(json!({ "error": error }));
        }
        self.ghost_stopped = true;
        self.ghost_acted = now;
        self.ghost_stale = true;
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

    /// The human held Esc: the click on its way is not made. With bypass
    /// on it ends bypass too, the human's word that the trust is over,
    /// even with nothing on its way.
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
        } else if halts {
            // The rest waiting were asked before the stop and still go,
            // each with its own glide to stop.
            self.next_glide(Instant::now());
        }
        self.run();
    }

    /// The human turned takeover off: nothing on its way is made, bypass
    /// ends, and everything shown goes at once, without its fade. The
    /// next look rests the overlay.
    fn turned_off(&mut self) {
        const OFF: &str = "the human turned takeover off, so this action was not made";
        self.halt(OFF, "not made: takeover turned off".into());
        for q in std::mem::take(&mut self.queue) {
            q.reply.send(json!({ "error": OFF }));
        }
        if bypass() {
            self.bypass_off(BypassEnd::Off);
        }
        self.until = None;
        self.shown = 0.0;
        self.watch = None;
        self.ghost_at = None;
        self.ghost_stopped = false;
        self.landed = None;
        if let Some(g) = self.ghost.as_mut() {
            g.hide();
        }
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

    /// Turns bypass on for the session the plate shows. Not without one:
    /// on for nobody, it would have no work to end with.
    fn bypass_on(&mut self) -> bool {
        if self.now.session.is_empty() {
            return false;
        }
        BYPASS.set(true);
        BYPASS_LIVE.store(true, Ordering::Release);
        take_turns_ended();
        let now = Instant::now();
        self.bypass_at = now;
        self.bypass_until = clock_in(BYPASS_MAX);
        self.turns_checked = now;
        self.holders.clear();
        // One that only rests has BYPASS_IDLE to start working.
        self.holders
            .insert(self.now.session.clone(), Holder::default());
        hold_shells(&self.now.session);
        self.note_bypass(None);
        self.stale = true;
        self.run();
        true
    }

    /// Writes bypass turned on or ended into the history and into
    /// `takeover.log`, which is where the human reads afterwards what went
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
        // Through the writer's thread: this one draws.
        log_lines(takeover_programs::log_line(
            &utc_now(),
            &self.now.name,
            source,
            &self.now.program,
            "",
        ));
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
        // Starts what the human asked for.
        self.next_glide(Instant::now());
        self.run();
    }

    /// Bypass on: looks at the sessions it is on for, and turns it off once
    /// their work is done and nothing is on its way, or once it has been
    /// on as long as it ever may.
    fn watch_turns(&mut self, now: Instant) {
        if !bypass() || now.duration_since(self.turns_checked) < TURNS_EVERY {
            return;
        }
        self.turns_checked = now;
        if bypass_spent(now.duration_since(self.bypass_at)) {
            self.bypass_off(BypassEnd::Ceiling);
            return;
        }
        for session in take_turns_ended() {
            if let Some(h) = self.holders.get_mut(&session) {
                h.ended = true;
            }
        }
        let waited = now.duration_since(self.bypass_at) >= BYPASS_IDLE;
        if let Ok(reg) = self.shared.registry.lock() {
            for (id, h) in &mut self.holders {
                let phase = reg.get(id).map(|s| &s.phase);
                h.saw(turn_of(phase), asks_human(phase), waited, now);
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
            || self.show.is_some()
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
        let phase = s.map(|s| &s.phase);
        let away = away_of(
            s.and_then(|s| s.tool.as_deref()),
            turn_of(phase),
            asks_human(phase),
            &self.now.program,
        );
        drop(reg);
        if away != self.now.away {
            self.now.away = away;
            self.stale = true;
        }
    }

    /// Takes in the shell commands the hook thread heard, each into its
    /// session's list and into `takeover.log`, the audit trail of what ran
    /// beside the control. Runs also after control rested: a command
    /// begun under it is owed its end line whenever that comes.
    fn hear_shells(&mut self) {
        let notes = take_shell_notes();
        if notes.is_empty() {
            return;
        }
        let mut lines = String::new();
        let utc = utc_now();
        for note in notes {
            let session = note.session().to_string();
            let listed = holds_shells(&session);
            let heard = hear_shell(&mut self.shells, &mut self.ledger, note, listed);
            if heard.is_empty() {
                continue;
            }
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
            for shell in &heard {
                lines.push_str(&shell_log_line(&utc, &name, shell));
            }
        }
        // Text laid out for a command no list holds any more goes.
        let kept: std::collections::HashSet<u64> =
            self.shells.values().flatten().map(|s| s.serial).collect();
        self.laid.retain(|serial, _| kept.contains(serial));
        if !lines.is_empty() {
            log_lines(lines);
        }
        if !self.on() {
            // Rested: whoever got their last end is no longer heard.
            release_shells(self.ledger.keys().cloned().collect());
        }
    }

    /// Whether the overlay runs at all: frames or the beat.
    fn on(&self) -> bool {
        self.framing || self.beating.is_some()
    }

    fn set_speed(&mut self, f: f32) {
        let d = glide_of_slider(f);
        if d != SPEED.get() {
            SPEED.set(d);
            self.stale = true;
        }
    }

    /// Whether a point on screen, in physical pixels, is over one of the
    /// plate's controls, or one of them is held.
    fn over_control(&self, hwnd: HWND, at: (i32, i32)) -> bool {
        let Some(p) = self.plate.as_ref().filter(|p| p.hwnd == hwnd && p.shown) else {
            return false;
        };
        self.dragging || self.arming.is_some() || self.control_under(p, at).is_some()
    }

    fn control_under(&self, plate: &Layer, at: (i32, i32)) -> Option<Control> {
        let x = (at.0 - plate.pos.0) as f32 / plate.scale;
        let y = (at.1 - plate.pos.1) as f32 / plate.scale;
        control_at(x, y, self.shells_top)
    }

    /// Whether the human, and not a message some process posted, holds
    /// the mouse on Bypass: the input thread saw their own button go down
    /// there, the hardware says it is still down, and the pointer is
    /// still on it.
    fn pressing_bypass(&self) -> bool {
        let Some(plate) = self.plate.as_ref().filter(|p| p.shown) else {
            return false;
        };
        let on_it = |at: (i32, i32)| self.control_under(plate, at) == Some(Control::Bypass);
        let mut cursor = POINT::default();
        let pointer = unsafe { GetCursorPos(&mut cursor) }.is_ok() && on_it((cursor.x, cursor.y));
        pointer && button_down() && input::left_held().is_some_and(on_it)
    }

    fn end_arming(&mut self) {
        if self.arming.take().is_some() {
            unsafe {
                let _ = ReleaseCapture();
            }
            self.stale = true;
        }
    }

    /// The mouse on the plate.
    fn mouse(&mut self, hwnd: HWND, msg: u32, lparam: LPARAM) {
        let Some(p) = self.plate.as_ref().filter(|p| p.hwnd == hwnd) else {
            return;
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
                // Off is the safe way, and anyone may turn it.
                Some(Control::Bypass) if bypass() => self.bypass_off(BypassEnd::Switched),
                // On takes the human's own hand, held: the message alone
                // says nothing of who sent it.
                Some(Control::Bypass) => {
                    if self.pressing_bypass() {
                        self.arming = Some(Instant::now());
                        unsafe {
                            SetCapture(hwnd);
                        }
                    }
                }
                Some(Control::Shells) => {
                    self.shells_open = !self.shells_open;
                    self.stale = true;
                }
                None => {}
            },
            WM_MOUSEMOVE if self.dragging => self.set_speed(slider_at(x)),
            WM_LBUTTONUP => {
                if self.dragging {
                    self.dragging = false;
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                }
                self.end_arming();
            }
            WM_CAPTURECHANGED => {
                self.dragging = false;
                if self.arming.take().is_some() {
                    self.stale = true;
                }
            }
            _ => {}
        }
        self.run();
    }

    /// One look at everything: what the human and the sessions did since
    /// the last, what moves, and what is drawn again for it. Comes on a
    /// frame, on the beat, and when something wakes the overlay. True
    /// when it turned bypass on, for the app to let the waiting calls go.
    fn tick(&mut self) -> bool {
        let now = Instant::now();
        let dt = now
            .duration_since(self.ticked)
            .min(Duration::from_millis(50));
        self.ticked = now;
        // The human's stop before anything is answered: a glide that
        // arrived in this same look must lose to an Esc held before it.
        if input::take_stop() {
            self.stop();
        }
        // Takeover turned off in the tray menu ends it here and now. The
        // switch is the app's, and nothing tells the overlay: it looks.
        if crate::takeover_mode::mode() == Mode::Off && self.show.is_none() {
            self.turned_off();
        }
        // A trip whose message came while the overlay was busy, or after
        // this frame, is taken before the glide is answered: the window
        // it saw opened before the click.
        self.tripped();
        self.play_show(now);
        self.watch_turns(now);
        self.watch_now(now);
        self.hear_shells();
        let mut turned_on = false;
        if let Some(since) = self.arming {
            if !self.pressing_bypass() {
                self.end_arming();
            } else if now.duration_since(since) >= BYPASS_HOLD {
                self.end_arming();
                turned_on = self.bypass_on();
            }
        }

        let active = self.active(now) || self.glide.is_some();
        if active {
            self.listen();
            self.shown = (self.shown + dt.as_secs_f32() / FADE_IN.as_secs_f32()).min(1.0);
        } else {
            self.shown = (self.shown - dt.as_secs_f32() / FADE_OUT.as_secs_f32()).max(0.0);
        }
        if self.screens_stale {
            self.follow_screens();
        }

        if let Some(g) = &self.glide {
            let elapsed = now.duration_since(g.began);
            self.ghost_at = Some(glide_at(g.from, g.to, elapsed, g.takes));
            if may_land(elapsed >= g.takes, input::esc_down()) {
                if let Some(g) = self.glide.take() {
                    if let Some(r) = &g.reply {
                        r.send(json!({ "allowed": true }));
                    }
                    if g.unasked {
                        note_unasked(&mut self.unasked, &g.what);
                    }
                }
                self.ghost_acted = now;
                self.landed = Some(now);
                self.ghost_stale = true;
                self.stale = true;
                self.next_glide(now);
            }
        }
        if self
            .landed
            .is_some_and(|t| now.duration_since(t) > mark::flourish())
        {
            self.landed = None;
            self.ghost_stale = true;
        }
        self.paint_ghost(now);

        let runes = self.runes_due(now);
        self.paint_frame(now, active);
        self.paint_plate(now, active, runes);

        let ghost_seen = self.ghost.as_ref().is_some_and(|g| g.shown);
        if self.shown <= 0.0 && !active && !ghost_seen {
            self.rest();
            return turned_on;
        }
        let fading = if active {
            self.shown < 1.0
        } else {
            self.shown > 0.0
        };
        let ghost_fading = ghost_seen
            && self.glide.is_none()
            && !(self.ghost_stopped && active)
            && now.duration_since(self.ghost_acted) >= GHOST_STAYS;
        let moving = self.glide.is_some()
            || fading
            || ghost_fading
            || self.landed.is_some()
            || self.dragging
            || self.arming.is_some()
            || self.stale;
        self.pace(moving);
        turned_on
    }

    /// Whether the runes move on in this look: once a beat, however often
    /// the looks come.
    fn runes_due(&mut self, now: Instant) -> bool {
        if now.duration_since(self.runes_at) < BEAT_EVERY {
            return false;
        }
        self.runes_at = now;
        true
    }

    /// The runes' clock, none with animations off: then they stand still.
    fn rune_clock(&self, now: Instant) -> Option<f32> {
        backdrop::animations_on().then(|| now.duration_since(self.began).as_secs_f32())
    }

    /// Nothing is shown: the windows go, the hooks come out and nothing
    /// runs until the next call or note.
    fn rest(&mut self) {
        self.hear_shells();
        // The next run of control lists only what it saw begin. What
        // still runs is owed its end line, and is heard for that alone.
        release_shells(self.ledger.keys().cloned().collect());
        self.shells.clear();
        self.laid.clear();
        self.frame = None;
        self.frame_after = None;
        if let Some(p) = self.plate.as_mut() {
            p.hide();
        }
        self.geom = None;
        self.stale = true;
        self.ghost_stopped = false;
        self.end_arming();
        self.dragging = false;
        input::stop();
        if self.framing {
            self.framing = false;
            crate::vsync::stop(self.clock, FRAME);
        }
        if self.beating.take().is_some() {
            unsafe {
                let _ = KillTimer(Some(self.clock), BEAT);
            }
        }
    }

    /// The light round the screens: made when control shows, made anew
    /// when its colour changes or animations are turned on or off, and
    /// otherwise only faded. It moves by itself.
    fn paint_frame(&mut self, now: Instant, active: bool) {
        if self.shown <= 0.0 && !active {
            self.frame = None;
            return;
        }
        let tint = if self.bypass_look() {
            theme::waiting()
        } else {
            theme::working()
        };
        let moving = backdrop::animations_on();
        if (self.frame.as_ref()).is_some_and(|f| f.tint != tint || f.moving != moving) {
            self.frame = None;
            self.frame_after = None;
        }
        if self.frame.is_none() && self.frame_after.is_none_or(|at| now >= at) {
            self.frame = frame::Frame::new(&self.shared.gpu, tint, moving);
            // Not tried on every look if it can not be made.
            self.frame_after = Some(now + FRAME_AGAIN);
        }
        if let Some(frame) = self.frame.as_mut() {
            frame.show(self.shown);
        }
    }

    fn paint_ghost(&mut self, now: Instant) {
        let Some(at) = self.ghost_at else {
            return;
        };
        // A stopped mark stays where it was stopped while control runs.
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
            (mark::SIZE.0 * scale).round() as i32,
            (mark::SIZE.1 * scale).round() as i32,
        );
        let off = (mark::TIP * scale).round() as i32;
        let pos = (at.0.round() as i32 - off, at.1.round() as i32 - off);
        if self.ghost.as_ref().is_some_and(|g| g.scale != scale) {
            self.ghost = None;
        }
        let gpu = &self.shared.gpu;
        if self.ghost.is_none() {
            match Layer::new(gpu, pos, size, scale) {
                Ok(l) => self.ghost = Some(l),
                Err(_) => return,
            }
            self.ghost_stale = true;
        }
        let tint = if self.ghost_stopped {
            theme::error()
        } else if self.glide.as_ref().is_some_and(|g| g.unasked) {
            theme::waiting()
        } else {
            theme::working()
        };
        let landed = self.landed.map(|t| now.duration_since(t).as_secs_f32());
        let Some(layer) = self.ghost.as_mut() else {
            return;
        };
        // Painted when it changed or while its flourish plays. Travelling
        // it is only moved, which sends no pixels.
        if !(self.ghost_stale || landed.is_some() || self.ghost_tint != Some(tint)) {
            layer.place(pos, alpha);
            return;
        }
        self.ghost_stale = false;
        self.ghost_tint = Some(tint);
        if self
            .name_laid
            .as_ref()
            .is_none_or(|(n, _, _)| *n != self.ghost_name)
        {
            let wide: Vec<u16> = self.ghost_name.encode_utf16().collect();
            self.name_laid = unsafe {
                gpu.dw
                    .CreateTextLayout(&wide, &gpu.chip, mark::NAME_MAX, 18.0)
            }
            .ok()
            .map(|l| {
                let w = render::text_size(&l).0.ceil();
                (self.ghost_name.clone(), l, w)
            });
        }
        let name = self
            .name_laid
            .as_ref()
            .filter(|(n, _, _)| !n.is_empty())
            .map(|(_, l, w)| (l, *w));
        let scene = mark::MarkScene {
            rune: mark::rune_of(&self.ghost_name),
            tint,
            name,
            landed: landed.map(|age| (age, self.glides)),
        };
        if layer.paint(|p| unsafe { mark::draw(p, &scene) }).is_ok() {
            let fresh = !layer.shown;
            layer.present(pos, alpha);
            if fresh {
                // Over the plate, which is as topmost as it is.
                layer.raise();
            }
        }
    }

    /// The pointer over the plate: it lets the mouse in only over a
    /// control, so the rest of it does not stand between the human and
    /// their screen. Looked at on every look, not only on a move, so a
    /// pointer that rests on a control when the plate comes up, or that
    /// the plate grew under, clicks it.
    fn look_at_pointer(&mut self) {
        let Some(plate) = self.plate.as_ref().filter(|p| p.shown) else {
            input::watch_mouse(false);
            return;
        };
        let mut cursor = POINT::default();
        let hover = unsafe { GetCursorPos(&mut cursor) }
            .ok()
            .and_then(|()| self.control_under(plate, (cursor.x, cursor.y)));
        plate.set_clickable(
            hover.is_some() || self.dragging || self.arming.is_some(),
            cursor,
        );
        // The human's own press is told from a sent one by a mouse hook,
        // in only while the pointer is on the one switch that asks.
        input::watch_mouse(hover == Some(Control::Bypass) || self.arming.is_some());
        if hover != self.hover {
            self.hover = hover;
            self.stale = true;
        }
    }

    fn paint_plate(&mut self, now: Instant, active: bool, runes: bool) {
        if self.shown <= 0.0 {
            if let Some(p) = self.plate.as_mut() {
                p.hide();
            }
            input::watch_mouse(false);
            return;
        }
        self.look_at_pointer();
        let bypass = self.bypass_look();
        let clock = self.rune_clock(now);
        // The title's runes are cut while the session is in the program:
        // on its way to a click, or with that as its latest doing.
        let working = self.glide.is_some() || (active && self.now.away.is_none());
        let gliding = self
            .glide
            .as_ref()
            .map(|g| left_of_glide(now.duration_since(g.began), g.takes));
        let arming = self
            .arming
            .map(|t| crate::motion::progress(now.duration_since(t), BYPASS_HOLD));
        let whole = self.stale
            || self.plate.is_none()
            || self
                .geom
                .as_ref()
                .is_none_or(|g| g.working != working || g.bypass != bypass);
        if whole {
            self.paint_plate_whole(bypass, working, gliding, arming, clock);
            return;
        }
        let (Some(plate), Some(geom)) = (self.plate.as_mut(), self.geom.as_ref()) else {
            return;
        };
        plate.place(geom.pos, self.shown);
        let gpu = &self.shared.gpu;
        let mut part = |area: Area, draw: &dyn Fn(&layer::Pen)| {
            if plate.paint_part(area, draw).is_ok() {
                plate.present_part(area);
            }
        };
        if let (Some(bar), Some(left)) = (geom.spots.bar, gliding) {
            part(bar, &|p| unsafe { plate::draw_bar(p, bar, left) });
        }
        if let Some(f) = arming {
            let hover = self.hover == Some(Control::Bypass);
            part(plate::bypass_area(), &|p| unsafe {
                plate::draw_bypass(gpu, p, false, hover, Some(f), input::armed())
            });
        }
        if let (true, Some(t)) = (runes, clock) {
            if working {
                part(plate::title_area(), &|p| unsafe {
                    plate::draw_title_runes(p, Some(t), true, bypass)
                });
            }
            for &(spot, rune) in &geom.spots.running {
                part(spot, &|p| unsafe {
                    plate::draw_running(p, spot, rune, Some(t))
                });
            }
        }
    }

    /// Lays the plate out and paints all of it: when what it says changed.
    fn paint_plate_whole(
        &mut self,
        bypass: bool,
        working: bool,
        gliding: Option<f32>,
        arming: Option<f32>,
        clock: Option<f32>,
    ) {
        let shared = Rc::clone(&self.shared);
        let gpu = &shared.gpu;
        let content_w = BOX_W - 2.0 * PAD;
        // Away, what it does now takes the place of why it acted then.
        let said = self.now.away.as_deref().unwrap_or(&self.now.reason).trim();
        if self.reason_laid.as_ref().is_none_or(|(t, _)| t != said) {
            self.reason_laid = (!said.is_empty())
                .then(|| render::wrapped(gpu, &gpu.small, said, content_w).ok())
                .flatten()
                .map(|l| (said.to_string(), l));
        }
        let reason = self.reason_laid.as_ref().map(|(_, l)| l);
        let reason_h = reason.map_or(0.0, |l| render::text_size(l).1.ceil());
        let body = body_height(reason_h, self.history.len(), bypass);
        if self.mono.is_none() {
            self.mono = unsafe { plate::mono_format(gpu) };
        }
        // The commands of the session the plate shows, laid out once each,
        // fitted to the room the screen has under the rest.
        let room =
            (self.work[3] - self.work[1]) as f32 / self.scale - 2.0 * BOX_MARGIN - body - PAD;
        let list = self.shells.get(&self.now.session);
        let mut listed: Vec<&Shell> = Vec::new();
        if let (Some(list), Some(mono)) = (list, self.mono.as_ref()) {
            for shell in list {
                if let std::collections::hash_map::Entry::Vacant(slot) =
                    self.laid.entry(shell.serial)
                {
                    let Ok(layout) =
                        render::wrapped(gpu, mono, command_shown(&shell.command), COMMAND_W)
                    else {
                        continue;
                    };
                    let (lines, line_h) = plate::line_metrics(&layout);
                    slot.insert(Laid {
                        layout,
                        total: command_lines(lines, &shell.command),
                        line_h,
                        max_h: f32::INFINITY,
                    });
                }
                listed.push(shell);
            }
        }
        let line_h = listed
            .first()
            .and_then(|s| self.laid.get(&s.serial))
            .map_or(REASON_LINE, |l| l.line_h);
        let wants: Vec<(usize, ShellState)> = listed
            .iter()
            .filter_map(|s| self.laid.get(&s.serial).map(|l| (l.total, s.state)))
            .collect();
        let fit = fit_shells(&wants, line_h, room);
        let mut rows: Vec<ShellRow> = Vec::new();
        if self.shells_open {
            for (shell, &lines) in listed.iter().zip(&fit.shown) {
                let Some(laid) = self.laid.get_mut(&shell.serial) else {
                    continue;
                };
                let h = lines as f32 * laid.line_h;
                // Past its lines the layout trims the last one with an
                // ellipsis; set only when it changes, as it lays out again.
                if (laid.max_h - h).abs() > 0.1 {
                    laid.max_h = h;
                    unsafe {
                        let _ = laid.layout.SetMaxHeight(h + 0.5);
                    }
                }
                rows.push(ShellRow {
                    shell,
                    command: laid.layout.clone(),
                    h,
                    hidden: laid.total.saturating_sub(lines),
                });
            }
        }
        let shells_h = shells_height(self.shells_open, &fit, line_h);
        let h = box_height(reason_h, self.history.len(), bypass, shells_h);
        self.shells_top = Some(body);
        let count = list.map_or_else(|| shells_count(&VecDeque::new()), shells_count);
        let note = (self.shells_open && fit.note).then(|| shells_note(&fit));
        let s = self.scale;
        let size = ((BOX_W * s).round() as i32, (h * s).round() as i32);
        let margin = (BOX_MARGIN * s).round() as i32;
        let pos = box_place(self.work, size, margin);
        let mut fresh = false;
        if self.plate.is_none() {
            match Layer::new(gpu, pos, size, s) {
                Ok(l) => self.plate = Some(l),
                Err(_) => return,
            }
            fresh = true;
        }
        let Some(plate) = self.plate.as_mut() else {
            return;
        };
        fresh |= !plate.shown;
        self.stale = false;
        if plate.resize(size).is_err() {
            return;
        }
        let esc = input::armed();
        let banner = if esc {
            format!("BYPASS ON until {}. Hold Esc to end it", self.bypass_until)
        } else {
            format!("BYPASS ON until {}", self.bypass_until)
        };
        let scene = PlateScene {
            h,
            radius: shared.metrics.window_radius,
            now: &self.now,
            reason,
            reason_h,
            gliding,
            history: &self.history,
            speed: SPEED.get(),
            bypass,
            banner: &banner,
            hover: self.hover,
            dragging: self.dragging,
            esc,
            arming,
            t: clock,
            working,
        };
        let shells = ShellsScene {
            top: body,
            open: self.shells_open,
            count,
            rows,
            note,
            hover: self.hover == Some(Control::Shells),
            t: clock,
        };
        let mut spots = Spots::default();
        let painted = plate.paint(|p| unsafe {
            plate::draw_plate(gpu, p, &scene, &mut spots);
            plate::draw_shells(gpu, p, &shells, &mut spots);
        });
        if painted.is_err() {
            return;
        }
        plate.present(pos, self.shown);
        self.geom = Some(Geom {
            pos,
            spots,
            working,
            bypass,
        });
        if fresh {
            // The plate came up after the mark: the mark stays over it.
            if let Some(g) = self.ghost.as_ref().filter(|g| g.shown) {
                g.raise();
            }
        }
    }
}

/// A show of the overlay for a dev instance, to look at it without a
/// session driving anything: the runes round the screens, the plate with
/// sample content and the mark gliding between a few points, for a
/// quarter of a minute. `HORADRIC_GHOST_SHOW=1`, `bypass` to see it as
/// bypass draws it, or `idle` for control merely on with no mark, which
/// is the state to measure. After a comma, how many seconds after the
/// app's start it begins, and after another how many it lasts: an app
/// just started is busy with itself for a while, which spoils a
/// measurement. Nothing is clicked, no call is answered and bypass is
/// not on: only its colours are.
struct Show {
    began: Instant,
    lasts: Duration,
    bypass: bool,
    /// The mark glides.
    glides: bool,
    /// How many glides it has started.
    step: usize,
}

/// What the show's switch asks for: which show, when and how long.
fn show_asked() -> Option<(String, Duration, Duration)> {
    let asked = std::env::var("HORADRIC_GHOST_SHOW").ok()?;
    let mut parts = asked.split(',');
    let kind = parts.next()?.trim().to_string();
    let mut secs = |or: f32| {
        let s = parts.next().and_then(|p| p.trim().parse::<f32>().ok());
        Duration::from_secs_f32(s.unwrap_or(or).clamp(0.5, 120.0))
    };
    let after = secs(2.5);
    Some((kind, after, secs(15.0)))
}

thread_local! {
    /// The window that waits for the app to be up before the show starts.
    static SHOW_WINDOW: Cell<Option<HWND>> = const { Cell::new(None) };
}

/// Arms the show, when a dev instance was started with it asked for.
fn show_armed() {
    if !horadric_hooks::dev() {
        return;
    }
    let Some((_, after, _)) = show_asked() else {
        return;
    };
    if SHOW_WINDOW.get().is_some() || !registered() {
        return;
    }
    if let Ok(hwnd) = unseen_window() {
        SHOW_WINDOW.set(Some(hwnd));
        unsafe {
            SetTimer(Some(hwnd), SHOW, after.as_millis() as u32, None);
        }
    }
}

/// Starts the show once the app is there to give its fonts and metrics.
fn show_begin(hwnd: HWND) {
    let Some(shared) = super::with_app(|app| Rc::clone(&app.shared)) else {
        return;
    };
    unsafe {
        let _ = KillTimer(Some(hwnd), SHOW);
        let _ = DestroyWindow(hwnd);
    }
    SHOW_WINDOW.set(None);
    if let Some((kind, _, lasts)) = show_asked() {
        with(&shared, |o| o.begin_show(kind == "bypass", kind != "idle", lasts));
    }
}

impl Overlay {
    fn begin_show(&mut self, bypass: bool, glides: bool, lasts: Duration) {
        let now = Instant::now();
        self.began = now;
        self.until = Some(now + lasts);
        self.place_plate(0, None);
        let session = "show".to_string();
        hold_shells(&session);
        self.now = Now {
            session: session.clone(),
            ident: "3f9a12c0, started 09:41".into(),
            away: None,
            name: "Akara".into(),
            program: "Acme Studio".into(),
            reason: "The watch window still shows the old value, so the variable is typed \
                     in again and the list refreshed before the next reading."
                .into(),
            what: "click Button \"Refresh\"".into(),
        };
        self.bypass_until = clock_in(BYPASS_MAX);
        let entry = |at: &str, mark, what: &str, outcome: &str, unasked| Entry {
            at: at.into(),
            mark,
            what: what.into(),
            outcome: outcome.into(),
            unasked,
        };
        self.history = VecDeque::from([
            entry("09:42:10", Mark::Ok, "type \"Speed\" in Edit \"Name\"", "the field holds Speed", false),
            entry("09:42:02", Mark::Stopped, "click Button \"Delete\"", "stopped by the human", false),
            entry("09:41:55", Mark::Refused, "press Delete", "refused by the human", false),
            entry("09:41:40", Mark::Ok, "click MenuItem \"File\"", "the menu opened", bypass),
        ]);
        let shell = |serial, command: &str, description: &str, state, at: &str| Shell {
            id: format!("show{serial}"),
            tool: "PowerShell".into(),
            command: command.into(),
            description: description.into(),
            agent: serial == 2,
            at: at.into(),
            state,
            serial,
        };
        self.shells.insert(
            session,
            VecDeque::from([
                shell(
                    u64::MAX - 1,
                    "Get-Content .\\watch.log -Tail 40 |\n  Select-String 'Speed'",
                    "Read the watch log",
                    ShellState::Running,
                    "09:42:12",
                ),
                shell(
                    u64::MAX - 2,
                    "Copy-Item .\\project.smc2 .\\backup\\",
                    "Back the project up",
                    ShellState::Done,
                    "09:41:30",
                ),
            ]),
        );
        self.show = Some(Show {
            began: now,
            lasts,
            bypass,
            glides,
            step: 0,
        });
        self.stale = true;
        self.listen();
        self.run();
    }

    /// Moves the show on: a glide to the next of its points whenever the
    /// mark has rested a moment, and its end.
    fn play_show(&mut self, now: Instant) {
        let Some(show) = self.show.as_mut() else {
            return;
        };
        if now.duration_since(show.began) >= show.lasts {
            self.show = None;
            self.until = None;
            self.shells.clear();
            self.history.clear();
            self.now = Now::default();
            self.stale = true;
            return;
        }
        let rested = now.duration_since(self.ghost_acted) >= Duration::from_millis(1400);
        if !show.glides || self.glide.is_some() || !(show.step == 0 || rested) {
            return;
        }
        // Points on the plate's screen, as shares of its work area.
        const POINTS: [(f32, f32); 4] = [(0.34, 0.36), (0.58, 0.52), (0.44, 0.30), (0.66, 0.40)];
        let [l, t, r, b] = self.work.map(|v| v as f32);
        let at = |i: usize| {
            let (fx, fy) = POINTS[i % POINTS.len()];
            (l + (r - l) * fx, t + (b - t) * fy)
        };
        let step = show.step;
        show.step += 1;
        let unasked = show.bypass;
        let from = self.ghost_at.unwrap_or(at(step));
        self.start_glide(
            now,
            from,
            Some(at(step + 1)),
            unasked,
            None,
            "Akara".into(),
            "click Button \"Refresh\"".into(),
        );
    }
}

/// Runs `f` on the overlay if there is one and nothing holds it.
fn on_overlay<R>(f: impl FnOnce(&mut Overlay) -> R) -> Option<R> {
    OVERLAY.with(|o| o.try_borrow_mut().ok()?.as_mut().map(f))
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
            on_overlay(|o| o.mouse(hwnd, msg, lparam));
            LRESULT(0)
        }
        GUARD_MSG => {
            on_overlay(|o| o.tripped());
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_TIMER if wparam.0 == SHOW => {
            show_begin(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == FRAME || wparam.0 == BEAT => {
            if wparam.0 == FRAME {
                crate::vsync::took(hwnd, FRAME);
            }
            took_look();
            LRESULT(0)
        }
        // The human held Esc: for the next look, taken now.
        STOP_MSG => {
            if on_overlay(|o| o.on()).unwrap_or(false) {
                took_look();
            }
            LRESULT(0)
        }
        WAKE_MSG => {
            // A shell note is taken also when control has rested, for the
            // log. The rest is for a look, if looks run.
            let on = on_overlay(|o| {
                o.hear_shells();
                o.on()
            });
            if on == Some(true) {
                took_look();
            }
            LRESULT(0)
        }
        // A screen came or went, the work area moved, or a window of the
        // overlay is on a screen whose scale changed. Followed on the
        // next look, not here: this may be the message of a window the
        // following takes down.
        WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE
            if msg != WM_SETTINGCHANGE || wparam.0 as u32 == SPI_SETWORKAREA.0 =>
        {
            let waking = on_overlay(|o| {
                let first = !o.screens_stale;
                o.screens_stale = true;
                first && o.on()
            });
            if waking == Some(true) {
                wake();
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// One look of the overlay, and what follows it outside its borrow.
fn took_look() {
    let turned_on = on_overlay(|o| o.tick()).unwrap_or(false);
    if turned_on {
        // The calls waiting for a question go through bypass now, the
        // one whose question is on screen too. Outside the overlay's
        // borrow: the app may hand it calls.
        crate::dialog::release_aside();
        super::with_app(|app| app.desktop_bypass_on());
    }
}
