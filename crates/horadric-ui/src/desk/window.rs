//! The window that shows the agent's desk: it hosts Windows' Remote
//! Desktop control, connected to a child session of this user.
//!
//! There is one, on the UI thread, and it keeps itself: the app only
//! tells it when it starts and stops, and the tray menu asks it for its
//! lines, and an agent's `desk_` call opens it through [`agent_call`]
//! when it is closed. The `desk_` tools work only while it is open, so
//! the file that names the session exists only while it is connected.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, COLORREF, ERROR_SUCCESS, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
    InvalidateRect, SelectObject, SetBkMode, SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DT_CALCRECT, DT_CENTER, DT_NOPREFIX, DT_WORDBREAK, OUT_DEFAULT_PRECIS,
    PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::Security::Credentials::{
    CredPackAuthenticationBufferW, CredUIPromptForWindowsCredentialsW,
    CredUnPackAuthenticationBufferW, CREDUIWIN_GENERIC, CREDUI_INFOW, CRED_PACK_FLAGS,
    CRED_PACK_GENERIC_CREDENTIALS,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteKeyValueW, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, RegSetKeyValueW,
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};
use windows::Win32::System::RemoteDesktop::{
    ProcessIdToSessionId, WTSEnableChildSessions, WTSFreeMemory, WTSGetActiveConsoleSessionId,
    WTSGetChildSessionId, WTSIsChildSessionsEnabled, WTSLogoffSession, WTSQuerySessionInformationW,
    WTSUserName,
};
use windows::Win32::System::Threading::{GetCurrentProcessId, WaitForSingleObject, INFINITE};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, IsIconic, KillTimer,
    LoadCursorW, PostMessageW, RegisterClassW, SetForegroundWindow, SetTimer, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, GWL_STYLE, IDC_ARROW, SIZE_MINIMIZED, SWP_FRAMECHANGED, SWP_NOZORDER,
    SW_HIDE, SW_RESTORE, SW_SHOW, SW_SHOWNOACTIVATE, WM_APP, WM_CLOSE, WM_DESTROY, WM_DPICHANGED,
    WM_ENTERSIZEMOVE, WM_ERASEBKGND, WM_EXITSIZEMOVE, WM_LBUTTONUP, WM_PAINT, WM_SETFOCUS, WM_SIZE,
    WM_TIMER, WNDCLASSW, WS_CLIPCHILDREN, WS_EX_APPWINDOW, WS_OVERLAPPEDWINDOW, WS_POPUP,
};

use horadric_core::desk;

use crate::credential;
use crate::menu::Item;
use crate::screens::{self, Screen};

#[path = "window_rdp.rs"]
mod rdp;

use rdp::{Rdp, SignIn};

const CLASS: PCWSTR = w!("HoradricDesk");
const TITLE: PCWSTR = w!("Agent's desk");

/// An event of the control's: its id in wParam's low word, the control's
/// mark in the word above, its number in lParam.
const WM_DESK_EVENT: u32 = WM_APP + 1;
/// The elevated `desk enable` is back: wParam is whether child sessions
/// are on now.
const WM_DESK_ENABLED: u32 = WM_APP + 2;
/// Start connecting. Posted, so the window is on screen before a prompt.
const WM_DESK_BEGIN: u32 = WM_APP + 3;
/// The sign-in's thread is back: what it brought is in `Desk::answer`.
const WM_DESK_READY: u32 = WM_APP + 4;
/// The hand was asked for: wParam is whether it answers, lParam the mark
/// of the control that was connected when it was asked.
const WM_DESK_HAND: u32 = WM_APP + 5;

/// Asks Windows for the session's id until it has one.
const T_SESSION: usize = 1;
/// The window waits this long for the control to let go, then closes.
const T_CLOSE: usize = 2;
/// A sign-in that neither works nor fails is given up on.
const T_PATIENCE: usize = 3;
/// The hand should answer by now: a RunOnce line nobody ran is taken
/// back, and a desk without a hand is told to the human.
const T_HAND: usize = 4;

/// The close is through: the desk that was asked for meanwhile opens.
const T_AGAIN: usize = 5;

/// RunOnce runs about half a minute after a fresh sign-in, a slow one
/// later.
const HAND_AFTER_SIGN_IN: u32 = 120_000;
/// How long the session is given to let an old connection go before a
/// new one is made.
const AGAIN_AFTER: u32 = 1000;
/// A session that was signed in already runs no RunOnce: its hand
/// answers at once or is gone.
const HAND_AFTER_RETURN: u32 = 10_000;

/// The file in the state folder that holds the chosen screen's name.
const SCREEN_FILE: &str = "desk-screen";

/// The tray menu's ids for the desk.
const TRAY: usize = 9900;
const TRAY_OPEN: usize = TRAY;
const TRAY_CLOSE: usize = TRAY + 1;
const TRAY_END: usize = TRAY + 2;
const TRAY_SCREEN: usize = TRAY + 10;
const TRAY_LAST: usize = TRAY + 99;

/// The disconnect reasons that are no failure: this side, the other
/// side, or the session itself ended it.
const ENDED: [i32; 3] = [1, 2, 3];
/// Windows refused the user name or the password.
const REFUSED: i32 = 2055;

/// Where the window goes.
#[derive(Debug, Clone, PartialEq)]
enum Place {
    /// All of this screen, without a frame: (left, top, right, bottom).
    Fill([i32; 4]),
    /// An ordinary window, its outer edge.
    Window([i32; 4]),
}

/// Where the desk's window goes. With two screens or more it fills the
/// chosen one, or else the first that is not the primary, so the human
/// keeps the screen they work on. With one screen it is a window among
/// the others, about 1280 by 800 at this `scale` (1.0 at 96 dpi).
fn place(screens: &[Screen], chosen: Option<&str>, scale: f32) -> Place {
    if screens.len() >= 2 {
        let screen = chosen
            .and_then(|name| screens.iter().find(|s| s.name == name))
            .or_else(|| screens.iter().find(|s| !s.primary));
        if let Some(s) = screen {
            return Place::Fill(s.bounds);
        }
    }
    let work = screens
        .iter()
        .find(|s| s.primary)
        .or(screens.first())
        .map_or([0, 0, 1920, 1080], |s| s.work);
    let fit = |want: f32, room: i32| ((want * scale) as i32).min(room).max(1);
    let w = fit(1280.0, work[2] - work[0]);
    let h = fit(800.0, work[3] - work[1]);
    let x = work[0] + (work[2] - work[0] - w) / 2;
    let y = work[1] + (work[3] - work[1] - h) / 2;
    Place::Window([x, y, x + w, y + h])
}

/// The name of the screen the desk fills, for the tray menu's tick.
fn filled<'a>(screens: &'a [Screen], chosen: Option<&str>) -> Option<&'a str> {
    match place(screens, chosen, 1.0) {
        Place::Fill(bounds) => screens
            .iter()
            .find(|s| s.bounds == bounds)
            .map(|s| s.name.as_str()),
        Place::Window(_) => None,
    }
}

/// What RunOnce runs at the desk's sign-in. RunOnce carries no
/// environment, so a dev instance says what it is on the command line.
fn hand_command(exe: &str, dev: bool) -> String {
    let dev = if dev { " --dev" } else { "" };
    format!("\"{exe}\" desk hand{dev}")
}

/// The name to sign in with: a Microsoft account's email as it is (with
/// `MicrosoftAccount\` before it the sign-in hangs), a local account as
/// `.\name`.
fn account(microsoft: Option<&str>, local: &str) -> String {
    match microsoft.map(str::trim).filter(|m| m.contains('@')) {
        Some(email) => email.to_string(),
        None => format!(".\\{local}"),
    }
}

/// Why the desk went away, for the human. `hello_only` is Windows'
/// "only allow Windows Hello sign-in" setting, under which every
/// password is refused; `said` is Windows' own sentence when it has one.
fn why(reason: i32, hello_only: bool, said: Option<&str>) -> String {
    let known = match reason {
        REFUSED if hello_only => Some(
            "Windows refused the sign-in. This PC only allows Windows Hello for Microsoft \
             accounts, and under that setting every password is refused. Turn it off in \
             Settings, Accounts, Sign-in options, then open the desk again.",
        ),
        REFUSED => Some(
            "Windows refused the user name or the password. The desk needs the account's \
             password, not the PIN. Open the desk again to type it anew.",
        ),
        2567 => Some("Windows knows no account by that user name."),
        2823 => Some("That account is switched off."),
        3335 => Some("That account is locked after too many wrong passwords."),
        3591 => Some("That account has expired."),
        3847 | 4615 => Some("That account's password must be changed first."),
        2311 => Some("That account may not sign in this way, for example with an empty password."),
        516 | 264 | 2308 => Some(
            "The desk's session did not answer. Remote Desktop Services may be stopped or \
             child sessions switched off.",
        ),
        _ => None,
    };
    match (known, said) {
        (Some(known), _) => known.to_string(),
        (None, Some(said)) => format!("{said} (reason {reason})"),
        (None, None) => format!("Windows closed the desk's session, reason {reason}."),
    }
}

/// Switches child sessions on for this machine. Needs admin rights: it
/// is what `horadric desk enable` runs, started elevated by the window.
pub fn enable_child_sessions() -> Result<(), String> {
    unsafe { WTSEnableChildSessions(true) }.ok().map_err(|e| {
        format!(
            "Windows did not switch child sessions on: {} This needs admin rights.",
            e.message()
        )
    })
}

fn child_sessions_on() -> bool {
    let mut on = windows::core::BOOL(0);
    unsafe { WTSIsChildSessionsEnabled(&mut on) }.as_bool() && on.as_bool()
}

/// The desk's Windows session, signed in or left behind disconnected.
fn child_session() -> Option<u32> {
    let mut id = 0u32;
    unsafe { WTSGetChildSessionId(&mut id) }.ok().ok()?;
    Some(id)
}

/// Whether a desk is signed in, shown or not: what "End the desk" ends.
pub fn signed_in() -> bool {
    child_session().is_some()
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    /// Nothing is going on: a click starts over.
    Idle,
    /// A thread is out: the human is asked for admin rights or for the
    /// password, or the hand is asked for.
    Asking,
    Connecting,
    Connected,
    /// The window waits for the control to disconnect, hidden unless it
    /// was asked for again meanwhile.
    Closing,
}

/// What the sign-in's thread brings back. The password is wiped when
/// this is dropped, whichever way that happens.
#[derive(Default)]
struct Ready {
    user: String,
    /// Empty when the human gave none.
    secret: Vec<u8>,
    /// The sign-in came from Credential Manager.
    stored: bool,
    /// A hand answers already.
    hand: bool,
    /// The desk's session was signed in before this connect.
    returning: bool,
}

impl Drop for Ready {
    fn drop(&mut self) {
        self.secret.fill(0);
    }
}

/// Where the sign-in's thread leaves its [`Ready`]. The window holds the
/// other end, so a window that went away meanwhile leaves no password
/// behind in a message nobody reads.
type Answer = Arc<Mutex<Option<Ready>>>;

struct Desk {
    hwnd: HWND,
    rdp: Option<Rc<Rdp>>,
    phase: Phase,
    /// What the window says while it has no picture.
    status: String,
    /// A sign-in the human typed, kept only once Windows took it.
    typed: Option<(String, Vec<u8>)>,
    /// The sign-in came from Credential Manager, so a refusal drops it.
    stored: bool,
    /// The RunOnce line is this window's to take back.
    armed: bool,
    /// Where the sign-in's thread answers, while one is out.
    answer: Option<Answer>,
    /// The mark of the control whose events count. Letting go of a
    /// control moves it on, so what that one still says is not heard.
    mark: u16,
    /// The desk was asked for while it was closing: it opens again once
    /// the control has let go.
    again: bool,
    /// The session was signed in before this connect, see [`Ready`].
    returning: bool,
    polls: u32,
    /// The human is dragging an edge: the session waits for the end.
    sizing: bool,
    /// The desk shows but its hand never answered, see [`hand_asked`].
    handless: bool,
}

thread_local! {
    static DESK: RefCell<Option<Desk>> = const { RefCell::new(None) };
    static CLASS_READY: Cell<bool> = const { Cell::new(false) };
    /// A script is trying the desk on a dev instance: an ordinary window
    /// that takes neither a whole screen nor the focus from the human.
    static TRIAL: Cell<bool> = const { Cell::new(false) };
    static MARKS: Cell<u16> = const { Cell::new(0) };
}

/// A mark no control before had, for the next one or for none.
fn next_mark() -> u16 {
    MARKS.with(|m| {
        m.set(m.get().wrapping_add(1));
        m.get()
    })
}

/// The desk's state, briefly. Never held across a call into the control
/// or a prompt: both pump messages, which come back in here.
fn with<R>(f: impl FnOnce(&mut Desk) -> R) -> Option<R> {
    DESK.with(|d| d.try_borrow_mut().ok()?.as_mut().map(f))
}

fn window() -> Option<HWND> {
    with(|d| d.hwnd)
}

fn the_rdp() -> Option<Rc<Rdp>> {
    with(|d| d.rdp.clone()).flatten()
}

fn debug(what: &str) {
    if std::env::var_os("HORADRIC_DEBUG").is_some() {
        eprintln!("horadric: desk: {what}");
    }
}

fn state_file(name: &str) -> Option<PathBuf> {
    horadric_hooks::state_dir().map(|d| d.join(name))
}

fn chosen_screen() -> Option<String> {
    let text = std::fs::read_to_string(state_file(SCREEN_FILE)?).ok()?;
    let name = text.trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn forget_session() {
    if let Some(file) = state_file(desk::SESSION_FILE) {
        let _ = std::fs::remove_file(file);
    }
}

/// Called once when the app starts. A session file left by a Horadric
/// that crashed would tell the tools a closed desk is open.
pub(crate) fn startup() {
    let mut mine = 0u32;
    let _ = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut mine) };
    let named = state_file(desk::SESSION_FILE)
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|t| desk::read_session(&t));
    // A Horadric that autostarts inside the desk must leave the file to
    // the hand beside it, which reads it to know where it is.
    if named.is_some() && named != Some(mine) {
        forget_session();
    }
    // A Horadric that died while its RunOnce line was set left it for
    // the human's own next sign-in, where the hand would refuse and
    // flash a console. No desk of this instance is connecting now, so a
    // line of its own is a leftover. Another instance's line is left
    // alone, and so is every line seen from inside the desk, where
    // Windows may be about to run it.
    let own_line = std::env::current_exe()
        .ok()
        .map(|exe| hand_command(&exe.to_string_lossy(), horadric_hooks::dev()));
    let in_console = mine == unsafe { WTSGetActiveConsoleSessionId() };
    if in_console && own_line.is_some() && run_once_line() == own_line {
        debug("a RunOnce line was left behind");
        disarm_hand();
    }
    // For trying it from a script, which has no hand to click the tray.
    if let Some(how) = std::env::var_os("HORADRIC_DESK_OPEN").filter(|_| horadric_hooks::dev()) {
        TRIAL.with(|t| t.set(how == "window"));
        open();
    }
}

/// Called when the app quits: the desk closes with it, and stays signed in.
pub(crate) fn shutdown() {
    if let Some(hwnd) = window() {
        if let Some(rdp) = the_rdp() {
            rdp.disconnect();
        }
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}

/// Opens the desk's window, or brings it to the front.
pub fn open() {
    open_as(true);
}

/// [`open`], for the human's pick or an agent's call. An agent's never
/// takes the keyboard from the human: the window shows where it always
/// does, but the human's window keeps the focus.
fn open_as(activate: bool) {
    if let Some(hwnd) = window() {
        unsafe {
            if activate {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                let _ = ShowWindow(hwnd, SW_SHOW);
                let _ = SetForegroundWindow(hwnd);
            } else {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
        }
        // A window that was closing is wanted again, but its control is
        // still letting go and can not connect anew: the disconnect it
        // is about to tell of, or the close timer, opens the desk again.
        let begin = with(|d| {
            if d.phase == Phase::Closing {
                d.again = true;
                d.status = "Opening the agent's desk again.".into();
            }
            let begin = d.phase == Phase::Idle && d.rdp.is_none();
            // Words left by a failure are no longer true, and an agent
            // asking how the desk is would read them as this try's.
            if begin {
                d.status.clear();
            }
            begin
        });
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, true);
            if begin == Some(true) {
                let _ = PostMessageW(Some(hwnd), WM_DESK_BEGIN, WPARAM(0), LPARAM(0));
            }
        }
        return;
    }
    if let Err(e) = create(activate) {
        tell("The agent's desk did not open", &e);
    }
}

/// How an agent's call finds the desk.
#[derive(Debug, PartialEq)]
pub(crate) enum Seen {
    /// No window, or one the human is closing.
    Closed,
    /// On its way: a prompt, the sign-in or the connection.
    Opening(String),
    /// Shown; its hand may still be starting.
    Open,
    /// It stopped and says why, waiting for a click to try again.
    Failed(String),
}

impl Seen {
    /// The answer to `horadric mcp`, which its `crate::desk` reads.
    pub(crate) fn json(&self) -> serde_json::Value {
        let (desk, status) = match self {
            Seen::Closed => ("closed", ""),
            Seen::Opening(s) => ("opening", s.as_str()),
            Seen::Open => ("open", ""),
            Seen::Failed(s) => ("failed", s.as_str()),
        };
        serde_json::json!({ "desk": desk, "status": status })
    }
}

/// The desk's window as an agent's call sees it.
fn seen() -> Seen {
    let Some((phase, status, again, handless)) =
        with(|d| (d.phase, d.status.clone(), d.again, d.handless))
    else {
        return Seen::Closed;
    };
    // The window's words end in what to click, which is the human's.
    let words = status
        .lines()
        .filter(|l| !l.starts_with("Click here"))
        .collect::<Vec<_>>()
        .join(" ");
    match phase {
        Phase::Connected if handless => Seen::Failed(HANDLESS.into()),
        Phase::Connected => Seen::Open,
        Phase::Closing if !again => Seen::Closed,
        // A fresh window, or one asked to begin again, has no words
        // until its first step says some.
        Phase::Idle if !words.is_empty() => Seen::Failed(words),
        _ => Seen::Opening(words),
    }
}

/// An agent's `desk.open` or `desk.state`: the first opens the desk as
/// the tray's Open does, the second only looks. Called on the UI thread
/// from the app's call routing; nothing here waits, the window's own
/// steps are posted to it.
pub(crate) fn agent_call(op: &str) -> serde_json::Value {
    if op == "desk.open" && !matches!(seen(), Seen::Open | Seen::Opening(_)) {
        open_as(false);
    }
    seen().json()
}

/// Signs the desk out, which closes every app on it.
pub fn end() {
    let id = child_session();
    if let Some(hwnd) = window() {
        close(hwnd);
    }
    if let Some(id) = id {
        if let Err(e) = unsafe { WTSLogoffSession(None, id, false) } {
            tell("The desk did not sign out", &e.message());
        }
    }
}

fn tell(title: &str, text: &str) {
    debug(&format!("{title}: {text}"));
    crate::app::forge_note(title, text);
}

/// The desk's line in the tray menu, with what can be done to it now.
pub(crate) fn tray_item() -> Item {
    let open = window().is_some();
    let mut lines = vec![Item::action(TRAY_OPEN, if open { "Show" } else { "Open" })];
    if open {
        lines.push(Item::action(TRAY_CLOSE, "Close the window"));
    }
    if signed_in() {
        lines.push(Item::action(TRAY_END, "End the desk"));
    }
    let all = crate::app::monitors();
    if all.len() >= 2 {
        let chosen = chosen_screen();
        let on = filled(&all, chosen.as_deref()).map(str::to_string);
        lines.push(Item::Separator);
        lines.extend(all.iter().enumerate().map(|(i, s)| Item::Action {
            id: TRAY_SCREEN + i,
            label: screens::label(i + 1, s),
            checked: on.as_deref() == Some(s.name.as_str()),
            bold: false,
        }));
    }
    Item::Submenu("Agent's desk".into(), lines)
}

/// Whether a pick from the tray menu is one of the desk's lines.
pub(crate) fn tray_owns(id: usize) -> bool {
    (TRAY..=TRAY_LAST).contains(&id)
}

/// Does what a line of [`tray_item`] says.
pub(crate) fn tray_pick(id: usize) {
    match id {
        TRAY_OPEN => open(),
        TRAY_CLOSE => {
            if let Some(hwnd) = window() {
                close(hwnd);
            }
        }
        TRAY_END => end(),
        i if (TRAY_SCREEN..=TRAY_LAST).contains(&i) => {
            let all = crate::app::monitors();
            let Some(screen) = all.get(i - TRAY_SCREEN) else {
                return;
            };
            if let Some(file) = state_file(SCREEN_FILE) {
                let _ = std::fs::write(file, &screen.name);
            }
            match window() {
                Some(hwnd) => stand(hwnd),
                None => open(),
            }
        }
        _ => {}
    }
}

fn register_class() {
    if CLASS_READY.with(|c| c.replace(true)) {
        return;
    }
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: GetModuleHandleW(None).map(Into::into).unwrap_or_default(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
    }
}

fn create(activate: bool) -> Result<(), String> {
    register_class();
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_APPWINDOW,
            CLASS,
            TITLE,
            WS_POPUP | WS_CLIPCHILDREN,
            0,
            0,
            10,
            10,
            None,
            None,
            GetModuleHandleW(None).ok().map(Into::into),
            None,
        )
    }
    .map_err(|e| e.message())?;
    DESK.with(|d| {
        *d.borrow_mut() = Some(Desk {
            hwnd,
            rdp: None,
            phase: Phase::Idle,
            status: String::new(),
            typed: None,
            stored: false,
            armed: false,
            answer: None,
            mark: 0,
            again: false,
            returning: false,
            polls: 0,
            sizing: false,
            handless: false,
        })
    });
    stand(hwnd);
    unsafe {
        if TRIAL.with(Cell::get) || !activate {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        } else {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
        let _ = PostMessageW(Some(hwnd), WM_DESK_BEGIN, WPARAM(0), LPARAM(0));
    }
    Ok(())
}

/// Puts the window where [`place`] says, with or without a frame.
fn stand(hwnd: HWND) {
    let mut all = crate::app::monitors();
    if TRIAL.with(Cell::get) {
        all.retain(|s| s.primary);
    }
    let scale = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0;
    let (style, at) = match place(&all, chosen_screen().as_deref(), scale) {
        Place::Fill(at) => (WS_POPUP, at),
        Place::Window(at) => (WS_OVERLAPPEDWINDOW, at),
    };
    unsafe {
        SetWindowLongPtrW(hwnd, GWL_STYLE, (style | WS_CLIPCHILDREN).0 as isize);
        let _ = SetWindowPos(
            hwnd,
            None,
            at[0],
            at[1],
            at[2] - at[0],
            at[3] - at[1],
            SWP_NOZORDER | SWP_FRAMECHANGED,
        );
    }
}

fn client(hwnd: HWND) -> (i32, i32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    ((r.right - r.left).max(1), (r.bottom - r.top).max(1))
}

/// The screen's scale in percent, as Remote Desktop takes it.
fn scale_percent(hwnd: HWND) -> u32 {
    (unsafe { GetDpiForWindow(hwnd) }.max(96) * 100 / 96).clamp(100, 500)
}

fn say(hwnd: HWND, phase: Phase, status: &str) {
    with(|d| {
        d.phase = phase;
        d.status = status.to_string();
    });
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

/// The first step: child sessions must be on, which takes admin rights
/// once per machine.
fn begin(hwnd: HWND) {
    if with(|d| d.phase != Phase::Idle || d.rdp.is_some()) != Some(false) {
        return;
    }
    if child_sessions_on() {
        return sign_in(hwnd);
    }
    say(
        hwnd,
        Phase::Asking,
        "The desk is a second Windows session, which has to be switched on once.\n\
         Windows asks for your permission now.",
    );
    let to = hwnd.0 as isize;
    // The wait is for the human's answer to Windows' prompt, which must
    // not freeze every other window of Horadric.
    std::thread::spawn(move || {
        let ran = run_enable();
        debug(&format!("desk enable ran: {ran:?}"));
        let on = child_sessions_on();
        unsafe {
            let _ = PostMessageW(
                Some(HWND(to as *mut c_void)),
                WM_DESK_ENABLED,
                WPARAM(on as usize),
                LPARAM(0),
            );
        }
    });
}

/// Runs `horadric desk enable` with admin rights and waits for it.
fn run_enable() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = HSTRING::from(exe.as_os_str());
    let mut run = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: w!("desk enable"),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut run).map_err(|e| e.message())?;
        if !run.hProcess.is_invalid() {
            WaitForSingleObject(run.hProcess, INFINITE);
            let _ = CloseHandle(run.hProcess);
        }
    }
    Ok(())
}

/// The second step: a sign-in, kept or asked for, and the connection.
fn sign_in(hwnd: HWND) {
    let target = desk::credential_target(horadric_hooks::state_name());
    let kept = credential::read(&target).filter(|(u, p)| !u.is_empty() && !p.is_empty());
    let answer = Answer::default();
    with(|d| d.answer = Some(answer.clone()));
    say(
        hwnd,
        Phase::Asking,
        if kept.is_some() {
            "Signing in to the agent's desk."
        } else {
            "Waiting for your Windows password."
        },
    );
    let to = hwnd.0 as isize;
    // The human takes their time over the prompt, and asking for the
    // hand waits on the network: neither may hold the UI thread.
    std::thread::spawn(move || {
        let owner = HWND(to as *mut c_void);
        let mut ready = Ready::default();
        ready.stored = kept.is_some();
        let typed = kept.or_else(|| {
            prompt(
                owner,
                &account(microsoft_account().as_deref(), &local_user()),
            )
        });
        if let Some((user, secret)) = typed {
            ready.user = user;
            ready.secret = secret;
            ready.hand = hand_answers(300);
            ready.returning = child_session().is_some_and(has_user);
        }
        if let Ok(mut slot) = answer.lock() {
            *slot = Some(ready);
        }
        unsafe {
            let _ = PostMessageW(Some(owner), WM_DESK_READY, WPARAM(0), LPARAM(0));
        }
    });
}

/// The sign-in's thread is back with a sign-in, or with none.
fn ready(hwnd: HWND) {
    let brought = with(|d| {
        if d.phase != Phase::Asking {
            return None;
        }
        // An answer that is not in yet belongs to a thread still out:
        // this message was an older one's.
        let ready = d.answer.as_ref()?.lock().ok()?.take()?;
        d.answer = None;
        Some(ready)
    })
    .flatten();
    let Some(ready) = brought else {
        return;
    };
    if ready.secret.is_empty() {
        return say(
            hwnd,
            Phase::Idle,
            "The desk needs your Windows password to sign in.\nClick here to try again.",
        );
    }
    connect(hwnd, ready);
}

/// The third step: the control, and the connection to the session.
fn connect(hwnd: HWND, mut ready: Ready) {
    say(hwnd, Phase::Connecting, "Signing in to the agent's desk.");
    // With a hand answering the session is still signed in and RunOnce
    // would not run there, but at the human's own next sign-in.
    let armed = !ready.hand && arm_hand();
    let mark = next_mark();
    with(|d| d.mark = mark);
    let stored = ready.stored;
    let user = std::mem::take(&mut ready.user);
    let connected = Rdp::create(hwnd, client(hwnd), WM_DESK_EVENT, mark).and_then(|rdp| {
        if !rdp.listening() {
            return Err("the Remote Desktop control does not tell what happens to it".into());
        }
        let mut password = String::from_utf8_lossy(&ready.secret).into_owned();
        let started = rdp.connect(&SignIn {
            user: &user,
            password: &password,
            size: client(hwnd),
            scale: scale_percent(hwnd),
        });
        // Safe: zeros are valid UTF-8, and the text is dropped right after.
        unsafe { password.as_bytes_mut() }.fill(0);
        started.map(|()| rdp)
    });
    match connected {
        Ok(rdp) => {
            debug(&format!(
                "connecting as a {} account",
                if user.contains('@') {
                    "Microsoft"
                } else {
                    "local"
                }
            ));
            with(|d| {
                d.rdp = Some(Rc::new(rdp));
                d.stored = stored;
                d.typed = (!stored).then(|| (user, std::mem::take(&mut ready.secret)));
                d.armed = armed;
                d.returning = ready.returning;
                d.polls = 0;
                d.handless = false;
            });
            unsafe {
                SetTimer(Some(hwnd), T_PATIENCE, 60_000, None);
            }
        }
        Err(e) => {
            if armed {
                disarm_hand();
            }
            say(
                hwnd,
                Phase::Idle,
                &format!("The desk did not open: {e}\nClick here to try again."),
            );
            tell("The agent's desk did not open", &e);
        }
    }
}

/// Windows' own prompt for a user name and a password, `user` filled in.
fn prompt(owner: HWND, user: &str) -> Option<(String, Vec<u8>)> {
    let caption = w!("Agent's desk");
    let message = w!(
        "The desk is a second Windows session of yours. Type your Windows password to sign \
         in to it (the password, not the PIN). Horadric keeps it in Credential Manager."
    );
    let info = CREDUI_INFOW {
        cbSize: std::mem::size_of::<CREDUI_INFOW>() as u32,
        hwndParent: owner,
        pszMessageText: message,
        pszCaptionText: caption,
        ..Default::default()
    };
    let user = HSTRING::from(user);
    unsafe {
        // Asked twice: once for the size, once for the bytes.
        let mut size = 0u32;
        let _ = CredPackAuthenticationBufferW(
            CRED_PACK_GENERIC_CREDENTIALS,
            PCWSTR(user.as_ptr()),
            w!(""),
            None,
            &mut size,
        );
        let mut filled = vec![0u8; size as usize];
        let prefilled = CredPackAuthenticationBufferW(
            CRED_PACK_GENERIC_CREDENTIALS,
            PCWSTR(user.as_ptr()),
            w!(""),
            Some(filled.as_mut_ptr()),
            &mut size,
        )
        .is_ok();
        let mut package = 0u32;
        let mut out: *mut c_void = std::ptr::null_mut();
        let mut out_size = 0u32;
        let answer = CredUIPromptForWindowsCredentialsW(
            Some(&info),
            0,
            &mut package,
            prefilled.then_some(filled.as_ptr() as *const c_void),
            if prefilled { size } else { 0 },
            &mut out,
            &mut out_size,
            None,
            CREDUIWIN_GENERIC,
        );
        if answer != ERROR_SUCCESS.0 || out.is_null() {
            return None;
        }
        let mut name = [0u16; 514];
        let mut domain = [0u16; 256];
        let mut password = [0u16; 257];
        let (mut name_len, mut domain_len, mut password_len) = (
            name.len() as u32,
            domain.len() as u32,
            password.len() as u32,
        );
        let unpacked = CredUnPackAuthenticationBufferW(
            CRED_PACK_FLAGS(0),
            out,
            out_size,
            Some(PWSTR(name.as_mut_ptr())),
            &mut name_len,
            Some(PWSTR(domain.as_mut_ptr())),
            Some(&mut domain_len),
            Some(PWSTR(password.as_mut_ptr())),
            &mut password_len,
        );
        std::slice::from_raw_parts_mut(out as *mut u8, out_size as usize).fill(0);
        CoTaskMemFree(Some(out));
        let text = |buf: &[u16]| {
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            String::from_utf16_lossy(&buf[..end])
        };
        let typed = (text(&name), text(&password).into_bytes());
        password.fill(0);
        unpacked.ok()?;
        (!typed.0.trim().is_empty() && !typed.1.is_empty()).then_some(typed)
    }
}

/// The email of the Microsoft account this Windows user signs in with.
fn microsoft_account() -> Option<String> {
    unsafe {
        let mut key = HKEY::default();
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\IdentityCRL\\UserExtendedProperties"),
            None,
            KEY_READ,
            &mut key,
        )
        .ok()
        .ok()?;
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        let found = RegEnumKeyExW(
            key,
            0,
            Some(PWSTR(name.as_mut_ptr())),
            &mut len,
            None,
            None,
            None,
            None,
        );
        let _ = RegCloseKey(key);
        (found == ERROR_SUCCESS).then(|| String::from_utf16_lossy(&name[..len as usize]))
    }
}

fn local_user() -> String {
    std::env::var("USERNAME").unwrap_or_default()
}

/// Windows' "only allow Windows Hello sign-in for Microsoft accounts".
fn hello_only() -> bool {
    let mut value = 0u32;
    let mut size = 4u32;
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\PasswordLess\\Device"),
            w!("DevicePasswordLessBuildVersion"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut c_void),
            Some(&mut size),
        ) == ERROR_SUCCESS
            && value == 2
    }
}

const RUN_ONCE_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\RunOnce");

/// Whether a hand answers on its port within `wait` milliseconds. It
/// waits, so never on the UI thread.
fn hand_answers(wait: u64) -> bool {
    let at = std::net::SocketAddr::from(([127, 0, 0, 1], desk::port(horadric_hooks::dev())));
    std::net::TcpStream::connect_timeout(&at, std::time::Duration::from_millis(wait)).is_ok()
}

/// Asks for the hand on a thread of its own and posts the answer back.
fn ask_hand(hwnd: HWND, mark: u16) {
    let to = hwnd.0 as isize;
    std::thread::spawn(move || {
        let answers = hand_answers(1000);
        unsafe {
            let _ = PostMessageW(
                Some(HWND(to as *mut c_void)),
                WM_DESK_HAND,
                WPARAM(answers as usize),
                LPARAM(mark as isize),
            );
        }
    });
}

/// Whether somebody is signed in to Windows session `id`.
fn has_user(id: u32) -> bool {
    let mut name = PWSTR::null();
    let mut bytes = 0u32;
    unsafe {
        if WTSQuerySessionInformationW(None, id, WTSUserName, &mut name, &mut bytes).is_err()
            || name.is_null()
        {
            return false;
        }
        let signed_in = *name.0 != 0;
        WTSFreeMemory(name.0 as *mut c_void);
        signed_in
    }
}

/// The RunOnce line as it stands, whoever wrote it.
fn run_once_line() -> Option<String> {
    let mut text = [0u16; 1024];
    let mut size = (text.len() * 2) as u32;
    let got = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_ONCE_KEY,
            &HSTRING::from(desk::RUN_ONCE),
            RRF_RT_REG_SZ,
            None,
            Some(text.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        )
    };
    (got == ERROR_SUCCESS).then(|| {
        let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
        String::from_utf16_lossy(&text[..end])
    })
}

/// Has the hand start at the desk's sign-in. True when the line was
/// written.
fn arm_hand() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let command = hand_command(&exe.to_string_lossy(), horadric_hooks::dev());
    let data: Vec<u16> = command.encode_utf16().chain([0]).collect();
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            RUN_ONCE_KEY,
            &HSTRING::from(desk::RUN_ONCE),
            REG_SZ.0,
            Some(data.as_ptr() as *const c_void),
            (data.len() * 2) as u32,
        ) == ERROR_SUCCESS
    }
}

/// Takes the RunOnce line back. Windows removes it when it runs it, so
/// one still there was never run, and would start a hand at the human's
/// own next sign-in.
fn disarm_hand() {
    unsafe {
        let _ = RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            RUN_ONCE_KEY,
            &HSTRING::from(desk::RUN_ONCE),
        );
    }
}

fn on_event(hwnd: HWND, id: usize, mark: u16, number: i32) {
    debug(&format!("event {id} ({number})"));
    // A control that was let go of may have said something on its way
    // out. Heard now, its disconnect would close a window that tells why
    // or has started over.
    if with(|d| d.mark) != Some(mark) {
        return debug("an event of a control that is gone");
    }
    match id {
        rdp::ON_CONNECTED | rdp::ON_LOGIN_COMPLETE => {
            // Both events come, in turn. The first one shows the desk.
            let first = with(|d| {
                if d.phase != Phase::Connecting {
                    return false;
                }
                d.phase = Phase::Connected;
                d.status.clear();
                d.polls = 0;
                true
            });
            if first == Some(true) {
                unsafe {
                    let _ = KillTimer(Some(hwnd), T_PATIENCE);
                    SetTimer(Some(hwnd), T_SESSION, 500, None);
                }
            }
            if id != rdp::ON_LOGIN_COMPLETE {
                return;
            }
            // Kept only now: being connected is not yet being signed in,
            // and a wrong password must never be stored.
            let typed = with(|d| (d.phase == Phase::Connected).then(|| d.typed.take()))
                .flatten()
                .flatten();
            if let Some((user, mut secret)) = typed {
                let target = desk::credential_target(horadric_hooks::state_name());
                if let Err(e) = credential::write(&target, &user, &secret) {
                    tell("The desk's sign-in was not kept", &e);
                }
                secret.fill(0);
            }
        }
        rdp::ON_DISCONNECTED => disconnected(hwnd, number),
        rdp::ON_FATAL_ERROR | rdp::ON_LOGON_ERROR => {}
        _ => {}
    }
}

fn disconnected(hwnd: HWND, reason: i32) {
    forget_session();
    let Some((rdp, phase, stored, armed, again)) = with(|d| {
        if let Some((_, secret)) = d.typed.as_mut() {
            secret.fill(0);
        }
        d.typed = None;
        (
            d.rdp.take(),
            d.phase,
            d.stored,
            std::mem::take(&mut d.armed),
            std::mem::take(&mut d.again),
        )
    }) else {
        return;
    };
    unsafe {
        let _ = KillTimer(Some(hwnd), T_SESSION);
        let _ = KillTimer(Some(hwnd), T_PATIENCE);
        let _ = KillTimer(Some(hwnd), T_HAND);
    }
    if armed {
        disarm_hand();
    }
    let said = rdp.as_ref().and_then(|r| r.describe(reason));
    drop(rdp);
    debug(&format!("disconnected, reason {reason}"));
    if phase == Phase::Closing && again {
        // The close is through, and the desk was asked for meanwhile.
        // Not at once: the session is still letting the old connection
        // go, and ends a new one that comes in the same moment.
        unsafe {
            let _ = KillTimer(Some(hwnd), T_CLOSE);
            SetTimer(Some(hwnd), T_AGAIN, AGAIN_AFTER, None);
            let _ = InvalidateRect(Some(hwnd), None, true);
        }
        return;
    }
    // An end that came before the desk was ever shown is a failure
    // whatever its reason, and gets its words below.
    if phase == Phase::Closing || (ENDED.contains(&reason) && phase != Phase::Connecting) {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        return;
    }
    if reason == REFUSED && stored {
        credential::delete(&desk::credential_target(horadric_hooks::state_name()));
    }
    let text = why(reason, hello_only(), said.as_deref());
    say(
        hwnd,
        Phase::Idle,
        &format!("{text}\nClick here to try again."),
    );
    tell("The agent's desk closed", &text);
}

/// Closing leaves the session signed in, so the agent's apps stay open.
fn close(hwnd: HWND) {
    forget_session();
    let live = the_rdp().filter(|r| r.connected() != 0);
    match live {
        Some(rdp) => {
            with(|d| {
                d.phase = Phase::Closing;
                d.again = false;
            });
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
                SetTimer(Some(hwnd), T_CLOSE, 3000, None);
            }
            rdp.disconnect();
        }
        None => unsafe {
            let _ = DestroyWindow(hwnd);
        },
    }
}

fn on_timer(hwnd: HWND, timer: usize) {
    match timer {
        T_SESSION => {
            let polls = with(|d| {
                d.polls += 1;
                d.polls
            })
            .unwrap_or(u32::MAX);
            let connected = with(|d| d.phase == Phase::Connected) == Some(true);
            let id = child_session().filter(|_| connected);
            if let Some(id) = id {
                if let Some(file) = state_file(desk::SESSION_FILE) {
                    if let Err(e) = std::fs::write(&file, desk::session_line(id)) {
                        tell("The desk's tools can not find it", &e.to_string());
                    }
                }
                debug(&format!("session {id}"));
            }
            if id.is_some() || polls > 60 {
                let wait = match with(|d| d.returning) {
                    Some(true) => HAND_AFTER_RETURN,
                    _ => HAND_AFTER_SIGN_IN,
                };
                unsafe {
                    let _ = KillTimer(Some(hwnd), T_SESSION);
                    SetTimer(Some(hwnd), T_HAND, wait, None);
                }
            }
        }
        T_CLOSE => {
            unsafe {
                let _ = KillTimer(Some(hwnd), T_CLOSE);
            }
            match with(|d| (d.phase == Phase::Closing).then(|| std::mem::take(&mut d.again))) {
                // Asked for again, and the control never said it let go.
                Some(Some(true)) => {
                    drop(let_go(hwnd));
                    unsafe {
                        SetTimer(Some(hwnd), T_AGAIN, AGAIN_AFTER, None);
                        let _ = InvalidateRect(Some(hwnd), None, true);
                    }
                }
                Some(Some(false)) => unsafe {
                    let _ = DestroyWindow(hwnd);
                },
                _ => {}
            }
        }
        T_AGAIN => {
            unsafe {
                let _ = KillTimer(Some(hwnd), T_AGAIN);
            }
            // Still closing means nobody closed it for good meanwhile.
            if with(|d| d.phase == Phase::Closing && d.rdp.is_none()) == Some(true) {
                with(|d| {
                    d.phase = Phase::Idle;
                    d.again = false;
                });
                begin(hwnd);
            }
        }
        T_PATIENCE => {
            unsafe {
                let _ = KillTimer(Some(hwnd), T_PATIENCE);
            }
            if with(|d| d.phase == Phase::Connecting) == Some(true) {
                let rdp = let_go(hwnd);
                if let Some(rdp) = &rdp {
                    rdp.disconnect();
                }
                drop(rdp);
                if with(|d| std::mem::take(&mut d.armed)) == Some(true) {
                    disarm_hand();
                }
                let text = "Windows did not answer the sign-in within a minute.";
                say(
                    hwnd,
                    Phase::Idle,
                    &format!("{text}\nClick here to try again."),
                );
                tell("The agent's desk did not open", text);
            }
        }
        T_HAND => {
            unsafe {
                let _ = KillTimer(Some(hwnd), T_HAND);
            }
            if with(|d| std::mem::take(&mut d.armed)) == Some(true) {
                disarm_hand();
            }
            if let Some(mark) = with(|d| d.mark) {
                ask_hand(hwnd, mark);
            }
        }
        _ => {}
    }
}

/// Takes the control out of the window without waiting for its
/// disconnect, and moves the mark on so that one is not heard later.
fn let_go(hwnd: HWND) -> Option<Rc<Rdp>> {
    unsafe {
        let _ = KillTimer(Some(hwnd), T_SESSION);
        let _ = KillTimer(Some(hwnd), T_HAND);
    }
    let mark = next_mark();
    with(|d| {
        d.mark = mark;
        d.rdp.take()
    })
    .flatten()
}

/// The hand was asked for when it should be up. A desk that shows but
/// has no hand is closed to every `desk_` tool, and only a new sign-in
/// starts one. That is the human's to choose: signing out closes the
/// agent's apps.
fn hand_asked(answers: bool, mark: u16) {
    let shown = with(|d| d.mark == mark && d.phase == Phase::Connected) == Some(true);
    if answers || !shown {
        return;
    }
    with(|d| d.handless = true);
    tell(
        "The desk's helper is not running",
        "The agent can not see or use the desk without it. \"End the desk\" in the tray \
         menu and opening the desk again starts it. Ending the desk closes the apps open \
         on it.",
    );
}

/// What an agent hears of a desk that shows without its hand.
const HANDLESS: &str = "The desk is open but its helper is not running, and only a new \
     sign-in starts one. The human can end the desk from Horadric's tray menu (Agent's desk, \
     End the desk), which closes the apps open on it; the next desk_ call opens it again.";

fn paint(hwnd: HWND) {
    let status = with(|d| d.status.clone()).unwrap_or_default();
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut ps);
        let mut all = RECT::default();
        let _ = GetClientRect(hwnd, &mut all);
        let back = CreateSolidBrush(COLORREF(0x0014_1210));
        FillRect(dc, &all, back);
        let _ = DeleteObject(back.into());
        if !status.is_empty() {
            let dpi = GetDpiForWindow(hwnd).max(96) as i32;
            let font = CreateFontW(
                -(15 * dpi / 96),
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                w!("Segoe UI"),
            );
            let old = SelectObject(dc, font.into());
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0x00C8_CCD0));
            let mut text: Vec<u16> = status.encode_utf16().collect();
            let format = DT_CENTER | DT_WORDBREAK | DT_NOPREFIX;
            let margin = (all.right / 8).max(8);
            let mut need = RECT {
                left: margin,
                top: 0,
                right: all.right - margin,
                bottom: 0,
            };
            let height = DrawTextW(dc, &mut text, &mut need, format | DT_CALCRECT);
            let top = ((all.bottom - height) / 2).max(0);
            let mut at = RECT {
                left: margin,
                top,
                right: all.right - margin,
                bottom: all.bottom,
            };
            DrawTextW(dc, &mut text, &mut at, format);
            SelectObject(dc, old);
            let _ = DeleteObject(font.into());
        }
        let _ = EndPaint(hwnd, &ps);
    }
}

fn resized(hwnd: HWND, settled: bool) {
    if let Some(rdp) = the_rdp() {
        rdp.resize(client(hwnd), settled, scale_percent(hwnd));
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_DESK_BEGIN => {
            begin(hwnd);
            LRESULT(0)
        }
        WM_DESK_ENABLED => {
            if with(|d| d.phase == Phase::Asking) == Some(true) {
                if wp.0 != 0 {
                    with(|d| d.phase = Phase::Idle);
                    sign_in(hwnd);
                } else {
                    say(
                        hwnd,
                        Phase::Idle,
                        "Child sessions are still off, so there is no desk. Switching them on \
                         needs an administrator's yes.\nClick here to try again.",
                    );
                }
            }
            LRESULT(0)
        }
        WM_DESK_EVENT => {
            on_event(hwnd, wp.0 & 0xFFFF, (wp.0 >> 16) as u16, lp.0 as i32);
            LRESULT(0)
        }
        WM_DESK_READY => {
            ready(hwnd);
            LRESULT(0)
        }
        WM_DESK_HAND => {
            hand_asked(wp.0 != 0, lp.0 as u16);
            LRESULT(0)
        }
        WM_TIMER => {
            on_timer(hwnd, wp.0);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            // Only reached while there is no control: it takes the clicks.
            begin(hwnd);
            LRESULT(0)
        }
        WM_ENTERSIZEMOVE => {
            with(|d| d.sizing = true);
            LRESULT(0)
        }
        WM_EXITSIZEMOVE => {
            with(|d| d.sizing = false);
            resized(hwnd, true);
            LRESULT(0)
        }
        WM_SIZE => {
            if wp.0 as u32 != SIZE_MINIMIZED {
                let sizing = with(|d| d.sizing).unwrap_or(false);
                resized(hwnd, !sizing);
                let _ = InvalidateRect(Some(hwnd), None, true);
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            resized(hwnd, true);
            LRESULT(0)
        }
        WM_SETFOCUS => {
            // The keyboard belongs to the desk while its window has it.
            if let Some(rdp) = the_rdp() {
                rdp.focus();
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_CLOSE => {
            close(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            forget_session();
            // Taken out first: dropping the control pumps messages.
            let gone = DESK.with(|d| d.try_borrow_mut().ok().and_then(|mut d| d.take()));
            if let Some(mut gone) = gone {
                if gone.armed {
                    disarm_hand();
                }
                if let Some((_, secret)) = gone.typed.as_mut() {
                    secret.fill(0);
                }
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(name: &str, left: i32, primary: bool) -> Screen {
        Screen {
            name: name.into(),
            bounds: [left, 0, left + 1920, 1080],
            work: [left, 0, left + 1920, 1032],
            primary,
        }
    }

    #[test]
    fn the_desk_fills_the_screen_the_human_is_not_on() {
        let two = [screen("A", 0, true), screen("B", 1920, false)];
        assert_eq!(place(&two, None, 1.0), Place::Fill([1920, 0, 3840, 1080]));
        assert_eq!(filled(&two, None), Some("B"));
    }

    #[test]
    fn a_chosen_screen_wins_while_it_is_plugged_in() {
        let two = [screen("A", 0, true), screen("B", 1920, false)];
        assert_eq!(place(&two, Some("A"), 1.0), Place::Fill([0, 0, 1920, 1080]));
        assert_eq!(
            place(&two, Some("gone"), 1.0),
            Place::Fill([1920, 0, 3840, 1080])
        );
    }

    #[test]
    fn one_screen_gets_an_ordinary_window_inside_the_work_area() {
        let one = [screen("A", 0, true)];
        assert_eq!(
            place(&one, Some("A"), 1.0),
            Place::Window([320, 116, 1600, 916])
        );
        // Scaled up it would not fit, so it stops at the work area.
        assert_eq!(place(&one, None, 2.0), Place::Window([0, 0, 1920, 1032]));
        assert_eq!(filled(&one, None), None);
    }

    #[test]
    fn a_dev_instance_says_so_to_its_hand() {
        assert_eq!(
            hand_command(r"C:\Program Files\Horadric\horadric.exe", false),
            r#""C:\Program Files\Horadric\horadric.exe" desk hand"#
        );
        assert!(hand_command(r"C:\h.exe", true).ends_with("desk hand --dev"));
    }

    #[test]
    fn a_microsoft_account_signs_in_by_its_email_alone() {
        assert_eq!(account(Some("ada@example.com"), "ada"), "ada@example.com");
        assert_eq!(account(None, "ada"), r".\ada");
        assert_eq!(account(Some(""), "ada"), r".\ada");
    }

    #[test]
    fn a_refused_sign_in_names_windows_hello_when_it_is_the_cause() {
        assert!(why(2055, true, None).contains("Windows Hello"));
        assert!(why(2055, false, Some("ignored")).contains("not the PIN"));
        assert!(!why(2055, false, None).contains("Windows Hello"));
    }

    #[test]
    fn an_unknown_reason_keeps_what_windows_said_and_its_number() {
        assert_eq!(
            why(9999, false, Some("It broke.")),
            "It broke. (reason 9999)"
        );
        assert!(why(9999, false, None).contains("9999"));
    }
}
