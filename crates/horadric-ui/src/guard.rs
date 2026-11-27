//! Watches the desktop while the ghost is on its way, and stops the click
//! when what it aimed at may have changed: a new window or dialog shows, or
//! another program takes the front. The agent chose its point from a
//! screenshot taken before; a click that lands on a window that opened
//! since lands on something nobody looked at.
//!
//! The hooks run out of context, so their callback comes here, on the UI
//! thread, between messages. It only notes what tripped and posts to the
//! ghost's own window, which acts on it in its own turn.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetClassNameW, GetWindowLongPtrW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsWindowVisible, PostMessageW, CHILDID_SELF, EVENT_OBJECT_SHOW,
    EVENT_SYSTEM_DIALOGSTART, EVENT_SYSTEM_FOREGROUND, GA_ROOT, GWL_EXSTYLE, OBJID_WINDOW,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WS_EX_TOOLWINDOW,
};

/// What the desktop did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    /// A window was shown.
    Shown,
    /// A dialog began.
    Dialog,
    /// Another window took the front.
    Front,
}

/// Window classes that come and go on their own and change nothing a click
/// lands on: tooltips, menus, shadows, the input method's windows, and the
/// window Windows parks the front on while it switches.
const IGNORED: [&str; 7] = [
    "tooltips_class32",
    "#32768",
    "SysShadow",
    "IME",
    "MSCTFIME UI",
    "ForegroundStaging",
    "Xaml_WindowedPopupClass",
];

/// What is known of a window an event came for.
#[derive(Clone, Debug)]
pub(super) struct Seen<'a> {
    pub kind: Kind,
    pub object: i32,
    pub child: i32,
    /// It is its own root, not a control inside another window.
    pub top: bool,
    pub tool: bool,
    pub size: (i32, i32),
    pub class: &'a str,
    pub pid: u32,
    /// Horadric's own process, whose overlay and dialogs never count.
    pub own: u32,
    /// The program being driven, 0 when not known.
    pub target: u32,
    /// The window lies over the point the ghost aims at.
    pub covers: bool,
}

/// Whether a window event should stop the click on its way. A window of
/// the driven program opening anywhere trips it, since that program's
/// state has moved; another program's only when it lands over the point,
/// so a notification in a corner does not.
pub(super) fn trips(s: &Seen) -> bool {
    if s.object != OBJID_WINDOW.0 || s.child != CHILDID_SELF as i32 || !s.top {
        return false;
    }
    if s.pid == 0 || s.pid == s.own {
        return false;
    }
    if s.tool || s.size.0 <= 0 || s.size.1 <= 0 || IGNORED.contains(&s.class) {
        return false;
    }
    match s.kind {
        Kind::Front => s.target != 0 && s.pid != s.target,
        Kind::Shown | Kind::Dialog => s.pid == s.target || s.covers,
    }
}

/// What tripped, for the agent's error and the history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Trip {
    pub title: String,
    pub kind: Kind,
    /// The glide it tripped, so a late one does not stop the next.
    pub glide: u64,
}

/// What the watch looks for, and where it says so.
#[derive(Clone, Copy, Default)]
struct Aim {
    notify: isize,
    msg: u32,
    pid: u32,
    point: Option<(i32, i32)>,
    glide: u64,
}

thread_local! {
    static AIM: Cell<Aim> = Cell::new(Aim::default());
    static TRIPPED: RefCell<Option<Trip>> = const { RefCell::new(None) };
}

/// The hooks, removed when dropped.
pub(super) struct Watch {
    hooks: Vec<HWINEVENTHOOK>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        for h in self.hooks.drain(..) {
            unsafe {
                let _ = UnhookWinEvent(h);
            }
        }
    }
}

/// Starts watching, on the calling thread, which must pump messages. A
/// trip posts `msg` to `notify`, to be read with [`take`].
pub(super) fn watch(notify: HWND, msg: u32) -> Watch {
    AIM.with(|a| {
        a.set(Aim {
            notify: notify.0 as isize,
            msg,
            ..a.get()
        })
    });
    let events = [
        EVENT_SYSTEM_FOREGROUND,
        EVENT_SYSTEM_DIALOGSTART,
        EVENT_OBJECT_SHOW,
    ];
    let hooks = events
        .into_iter()
        .map(|e| unsafe {
            SetWinEventHook(
                e,
                e,
                None,
                Some(on_event),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        })
        .filter(|h| !h.is_invalid())
        .collect();
    Watch { hooks }
}

/// Aims the watch at a glide: the driven program's window, and the point,
/// none for a hold.
pub(super) fn aim(glide: u64, window: isize, point: Option<(i32, i32)>) {
    let mut pid = 0u32;
    if window != 0 {
        unsafe {
            GetWindowThreadProcessId(HWND(window as *mut c_void), Some(&mut pid));
        }
    }
    AIM.with(|a| {
        a.set(Aim {
            pid,
            point,
            glide,
            ..a.get()
        })
    });
    TRIPPED.with(|t| t.borrow_mut().take());
}

/// What tripped since the last aim, once.
pub(super) fn take() -> Option<Trip> {
    TRIPPED.with(|t| t.borrow_mut().take())
}

fn text_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
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
    if hwnd.is_invalid() || TRIPPED.with(|t| t.borrow().is_some()) {
        return;
    }
    let kind = match event {
        EVENT_SYSTEM_FOREGROUND => Kind::Front,
        EVENT_SYSTEM_DIALOGSTART => Kind::Dialog,
        _ => Kind::Shown,
    };
    // A window shown and hidden again at once, like a splash, is gone by
    // the time the click lands.
    if kind != Kind::Front && !IsWindowVisible(hwnd).as_bool() {
        return;
    }
    let aim = AIM.with(Cell::get);
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    let mut rect = RECT::default();
    let _ = GetWindowRect(hwnd, &mut rect);
    let covers = aim
        .point
        .is_some_and(|(x, y)| x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom);
    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
    let class = class_of(hwnd);
    let seen = Seen {
        kind,
        object,
        child,
        top: GetAncestor(hwnd, GA_ROOT) == hwnd,
        tool: ex & WS_EX_TOOLWINDOW.0 != 0,
        size: (rect.right - rect.left, rect.bottom - rect.top),
        class: &class,
        pid,
        own: std::process::id(),
        target: aim.pid,
        covers,
    };
    if !trips(&seen) {
        return;
    }
    TRIPPED.with(|t| {
        *t.borrow_mut() = Some(Trip {
            title: text_of(hwnd),
            kind,
            glide: aim.glide,
        })
    });
    let _ = PostMessageW(
        Some(HWND(aim.notify as *mut c_void)),
        aim.msg,
        WPARAM(0),
        LPARAM(0),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog() -> Seen<'static> {
        Seen {
            kind: Kind::Shown,
            object: OBJID_WINDOW.0,
            child: CHILDID_SELF as i32,
            top: true,
            tool: false,
            size: (400, 300),
            class: "#32770",
            pid: 7,
            own: 1,
            target: 7,
            covers: false,
        }
    }

    #[test]
    fn a_window_of_the_driven_program_trips_wherever_it_opens() {
        assert!(trips(&dialog()));
        assert!(trips(&Seen {
            kind: Kind::Dialog,
            ..dialog()
        }));
    }

    #[test]
    fn another_program_trips_only_over_the_point() {
        let other = Seen { pid: 9, ..dialog() };
        assert!(!trips(&other));
        assert!(trips(&Seen {
            covers: true,
            ..other
        }));
    }

    #[test]
    fn the_front_moving_to_another_program_trips() {
        let front = Seen {
            kind: Kind::Front,
            ..dialog()
        };
        assert!(!trips(&front));
        assert!(trips(&Seen {
            pid: 9,
            ..front.clone()
        }));
        // Not knowing the driven program, the front says nothing.
        assert!(!trips(&Seen {
            pid: 9,
            target: 0,
            ..front
        }));
    }

    #[test]
    fn horadric_and_passing_windows_never_trip() {
        let all = [
            Seen {
                pid: 1,
                covers: true,
                ..dialog()
            },
            Seen { pid: 0, ..dialog() },
            Seen {
                tool: true,
                ..dialog()
            },
            Seen {
                size: (0, 20),
                ..dialog()
            },
            Seen {
                class: "tooltips_class32",
                ..dialog()
            },
            Seen {
                class: "#32768",
                ..dialog()
            },
            Seen {
                top: false,
                ..dialog()
            },
            Seen {
                object: -4,
                ..dialog()
            },
            Seen {
                child: 3,
                ..dialog()
            },
        ];
        for s in all {
            assert!(!trips(&s), "{s:?}");
        }
    }
}
