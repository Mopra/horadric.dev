//! The desktop tools of `horadric mcp`: an agent can list the programs on
//! the user's desktop, read a window's controls through UI Automation, take
//! a picture of it, and click, type and press keys in it, the way the
//! browser tools give it the web.
//!
//! All of it runs here, in the MCP process on the user's desktop, so a slow
//! UI Automation call never holds up Horadric's own windows. The app is
//! only asked whether the human allows it: once per program per session,
//! and again for anything that looks risky.

use std::collections::HashMap;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use horadric_core::desktop_programs::{Act, Kind};
use serde_json::{json, Value};
use windows::core::BOOL;
use windows::core::{Interface, BSTR, HRESULT};
use windows::Win32::Foundation::{
    CloseHandle, GlobalFree, E_ACCESSDENIED, HANDLE, HGLOBAL, HWND, LPARAM, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGR,
    IWICBitmapSource, IWICImagingFactory, WICBitmapEncoderNoCache, WICBitmapInterpolationModeFant,
    WICRect,
};
use windows::Win32::Security::{
    GetAce, GetKernelObjectSecurity, GetSecurityDescriptorSacl, GetSidSubAuthority,
    GetSidSubAuthorityCount, GetTokenInformation, TokenIntegrityLevel, ACL,
    LABEL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, STREAM_SEEK_END,
    STREAM_SEEK_SET,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcess, GetCurrentThreadId, OpenProcess, OpenProcessToken,
    QueryFullProcessImageNameW, PROCESS_ACCESS_RIGHTS, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    IsWindowEnabled, MapVirtualKeyW, SendInput, VkKeyScanW, INPUT, INPUT_0, INPUT_KEYBOARD,
    INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT,
    MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetAncestor, GetClassNameW, GetCursorPos, GetForegroundWindow,
    GetLastActivePopup, GetSystemMetrics, GetWindow, GetWindowLongW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, SendMessageTimeoutW,
    SetCursorPos, SetForegroundWindow, ShowWindow, WindowFromPoint, GA_ROOT, GWL_EXSTYLE, GW_OWNER,
    SMTO_ABORTIFHUNG, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SW_RESTORE, WM_GETTEXT, WM_GETTEXTLENGTH, WS_EX_TOOLWINDOW,
};

use crate::desktop_helper::{
    as_administrator, elevating_body, held_text, hold_left, still_asking_text, Helper, HelperStart,
    Setback, Starting, ANNOUNCE_PAUSE, ASK_AGAIN_WAIT, ASK_WAIT, PEEK,
};
use crate::mcp::text;
use crate::watch::{self, changes_part, changes_text, digest, verdict, Until};

/// The most elements one snapshot holds, so a vast window can not take
/// minutes or fill the context.
const MOST_ELEMENTS: usize = 2000;

/// A dialog with fewer elements than this is read whole to judge whether
/// what is about to happen in it is risky.
const SMALL_DIALOG: usize = 40;

/// How long `desktop_wait` waits when not told, and at most.
const WAIT_DEFAULT_MS: u64 = 10_000;
/// Sets another default for a wait, in milliseconds, for a machine whose
/// programs are slow.
pub const WAIT_ENV: &str = "HORADRIC_DESKTOP_WAIT_MS";

/// The most characters `desktop_text` gives back.
const TEXT_MOST: usize = 20_000;
/// How long a copy is given to reach the clipboard.
const COPY_WAIT: Duration = Duration::from_millis(1500);
const CF_UNICODETEXT: u32 = 13;
const WAIT_MOST_MS: u64 = 60_000;

/// Said when a change is asked for before any program is watched.
const NOTHING_WATCHED: &str = "Nothing is watched yet: Horadric watches a program from the first \
time this session looks at it or operates it (desktop_snapshot, desktop_screenshot, or naming its \
window to desktop_changes or desktop_wait).";

/// Said of a window that runs as administrator when the helper that
/// reaches it is not to be had.
const UNWATCHED: &str = "Changes in this window are only partly watched because it runs as \
administrator: its windows opening, closing and coming to the front, and that something in it \
changed, mostly without what the change was.";

/// How often an unnamed wait asks the helper what changed in its programs:
/// a call over a local pipe, cheap, but not on every turn of the loop.
const HELPER_POLL: Duration = Duration::from_millis(300);

/// What an unnamed wait says when the helper failed it.
pub fn helper_note(why: &str) -> String {
    format!(
        "\n(Programs that run as administrator were not watched in this wait: {why}. The helper \
         is started again on the next call to one.)"
    )
}

/// The widest picture, in pixels.
const WIDEST: u32 = 1600;

/// How long a UI Automation pattern may take before the answer says it is
/// still going. A program that opens a modal dialog from a pattern holds
/// the call until the dialog closes, which is why clicks are real ones.
const PATTERN_WAIT: Duration = Duration::from_secs(5);

/// Words in a control, title or dialog that mean an action may change the
/// controlled machine or the project, so the human confirms it.
pub const RISKY_WORDS: &[&str] = &[
    "transfer",
    "download",
    "synchroni",
    "run mode",
    "program mode",
    "operating mode",
    "force",
    "set/reset",
    "clear all memory",
    "restart",
    "reset",
    "delete",
    "overwrite",
    "write",
    "overfør",
    "slet",
    "nulstil",
];

/// A top level window, as the agent and the human see it.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub hwnd: isize,
    pub title: String,
    /// The program's file name, like `notepad.exe`.
    pub exe: String,
    /// The program's full path, which the app's list of allowed programs
    /// may name. Empty when Windows will not tell.
    pub path: String,
    pub pid: u32,
    /// The window that owns it, for a dialog.
    pub owner: Option<isize>,
    pub class: String,
}

/// Whether any of `texts` names something that could change a machine or
/// a project.
pub fn looks_risky(texts: &[&str]) -> bool {
    texts.iter().any(|t| {
        let t = t.to_lowercase();
        RISKY_WORDS.iter().any(|w| t.contains(w))
    })
}

/// The names Windows gives the file name box of its Open and Save
/// dialogs, in the languages the machines here run, without the colon or
/// the access key's mark.
const FILE_NAME_FIELDS: &[&str] = &["file name", "filnavn", "dateiname"];

/// Whether an element is the file name box of a standard Open or Save
/// dialog: an edit field or combo box named "File name:". Typing a path
/// there and pressing Enter picks a file on this PC, which changes nothing
/// by itself, whatever the dialog is for.
pub fn file_name_field(control: &str, name: &str) -> bool {
    if control != "Edit" && control != "ComboBox" {
        return false;
    }
    let name: String = name
        .chars()
        .filter(|c| *c != '&' && *c != '_')
        .collect::<String>()
        .trim()
        .trim_end_matches(':')
        .trim()
        .to_lowercase();
    FILE_NAME_FIELDS.contains(&name.as_str())
}

/// Whether Enter in a window confirms something that matters: in a dialog
/// it presses the default button, except in a file dialog, where it opens
/// the folder or picks the file typed.
pub fn enter_confirms(in_dialog: bool, file_dialog: bool) -> bool {
    in_dialog && !file_dialog
}

/// What an action's window says, read once for the risk words and for
/// the patterns the human pre-approved.
#[derive(Debug, Default, PartialEq)]
pub struct Said {
    /// A small dialog's lines of text, its question.
    pub texts: Vec<String>,
    /// It is a standard Open or Save dialog.
    pub file_dialog: bool,
}

/// The titles a wait's `appears` and `gone` look at: the named program's
/// windows, or every window when none was named. A window of the same name
/// in another program, a dialog the human has open elsewhere, must not
/// answer for the one the agent waits on.
pub fn wait_titles(windows: Vec<Window>, pid: Option<u32>) -> Vec<String> {
    windows
        .into_iter()
        .filter(|w| pid.is_none_or(|p| w.pid == p))
        .map(|w| w.title)
        .collect()
}

/// How long a wait may last: what the call asks, else what [`WAIT_ENV`]
/// says, else ten seconds, and never past a minute. Short by default, so an
/// agent waiting on something that will not come looks again soon instead
/// of waiting blind.
pub fn wait_limit(asked: Option<u64>, env: Option<&str>) -> Duration {
    let ms = asked
        .or_else(|| env.and_then(|e| e.trim().parse().ok()))
        .unwrap_or(WAIT_DEFAULT_MS);
    Duration::from_millis(ms.clamp(100, WAIT_MOST_MS))
}

/// What a wait waited for, in words.
pub fn waited_for(u: &Until) -> String {
    let mut said = Vec::new();
    if let Some(a) = &u.appears {
        said.push(format!("a window titled like {a:?}"));
    }
    if let Some(g) = &u.gone {
        said.push(format!("no window titled like {g:?}"));
    }
    if let Some(t) = &u.text {
        said.push(format!("the text {t:?}"));
    }
    if let Some(q) = u.quiet_ms {
        said.push(format!("{q} ms of quiet"));
    }
    said.join(" or ")
}

/// What a field holds after text was typed into it.
#[derive(Debug, PartialEq)]
pub enum Landed {
    /// It holds what was typed, maybe among more.
    Holds(String),
    /// It holds something else: the text went elsewhere, or not at all.
    Elsewhere(String),
    /// Its value can not be read.
    Unknown,
}

/// Whether `now`, the field's value read back, holds `typed`. Line breaks
/// and case are not held against it, since fields change both.
pub fn landed(typed: &str, now: Option<String>) -> Landed {
    let Some(now) = now else {
        return Landed::Unknown;
    };
    let flat = |s: &str| s.replace("\r\n", "\n").trim().to_lowercase();
    let want = flat(typed);
    if want.is_empty() || flat(&now).contains(&want) {
        Landed::Holds(now)
    } else {
        Landed::Elsewhere(now)
    }
}

/// What the agent is told of a field's value after typing.
pub fn landed_text(l: &Landed, entered: bool) -> String {
    let when = if entered {
        " when Enter was pressed"
    } else {
        ""
    };
    match l {
        Landed::Holds(v) => format!(" The field held {:?}{when}.", short(v, 300)),
        Landed::Elsewhere(v) => format!(
            " But the field held {:?}, not what was typed: the text may have gone to another \
             element, or the field changed it.",
            short(v, 300)
        ),
        Landed::Unknown => " Its value can not be read back through UI Automation; \
            desktop_snapshot or desktop_screenshot shows whether the text is there."
            .to_string(),
    }
}

/// What `desktop_text` answers with the text read `how`, cut to
/// [`TEXT_MOST`] characters.
pub fn text_answer(text: &str, how: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let n = text.chars().count();
    if n <= TEXT_MOST {
        return format!("Read {how}, {n} characters:\n{text}");
    }
    let kept: String = text.chars().take(TEXT_MOST).collect();
    format!("Read {how}, {n} characters, the first {TEXT_MOST} here:\n{kept}")
}

/// The window as the app's bodies name it.
fn window_json(window: &Window) -> Value {
    json!({
        "hwnd": window.hwnd,
        "title": window.title,
        "exe": window.exe,
        "path": window.path,
        "pid": window.pid,
    })
}

/// What the app is asked before the agent may see or operate `window`.
/// `point` is where on the screen the action lands, so the app can glide
/// its ghost cursor there while the human reads `reason`, the agent's own
/// words for why.
pub fn allow_body(
    window: &Window,
    action: &str,
    what: &str,
    risky: bool,
    point: Option<(i32, i32)>,
    reason: &str,
) -> Value {
    json!({
        "op": "desktop.allow",
        "window": window_json(window),
        "action": action,
        "what": what,
        "risky": risky,
        "point": point.map(|(x, y)| json!({ "x": x, "y": y })),
        "reason": reason,
    })
}

/// What the app is told of an operate action beside its sentence, so a
/// pattern the human pre-approved can be matched against it.
pub fn act_json(act: &Act) -> Value {
    json!({
        "kind": act.kind.map(Kind::word),
        "window": act.window,
        "control": act.control,
        "name": act.name,
        "key": act.key,
        "texts": act.texts,
    })
}

/// Asks the app whether the human allows an operate action, telling it
/// what the action is in parts as well as in words.
#[allow(clippy::too_many_arguments)]
pub fn allow_act<A: FnMut(Value) -> Result<Value, String>>(
    app: &mut A,
    window: &Window,
    what: &str,
    risky: bool,
    act: &Act,
    point: Option<(i32, i32)>,
    reason: &str,
) -> Result<(), String> {
    let mut body = allow_body(window, "operate", what, risky, point, reason);
    body["act"] = act_json(act);
    allowed(app(body)?, what)
}

fn allowed(answer: Value, what: &str) -> Result<(), String> {
    if answer["allowed"] == true {
        Ok(())
    } else {
        Err(format!("Horadric did not allow you to {what}"))
    }
}

/// Asks the app through `app` whether the human allows it.
pub fn allow<A: FnMut(Value) -> Result<Value, String>>(
    app: &mut A,
    window: &Window,
    action: &str,
    what: &str,
    risky: bool,
    point: Option<(i32, i32)>,
    reason: &str,
) -> Result<(), String> {
    let answer = app(allow_body(window, action, what, risky, point, reason))?;
    allowed(answer, what)
}

/// What the app is told after an allowed action, for the history the
/// human watches: whether it worked and, in one line, what came of it.
pub fn done_body(window: &Window, what: &str, ok: bool, outcome: &str) -> Value {
    json!({
        "op": "desktop.done",
        "window": window_json(window),
        "what": what,
        "ok": ok,
        "outcome": short(outcome, 160),
    })
}

/// Tells the app how an allowed action went and hands back its content.
/// `result` holds the content and the outcome line, or the error. The
/// app's answer does not matter: the action has happened either way.
pub fn report<A: FnMut(Value) -> Result<Value, String>>(
    app: &mut A,
    window: &Window,
    what: &str,
    result: Result<(Vec<Value>, String), String>,
) -> Result<Vec<Value>, String> {
    let body = match &result {
        Ok((_, outcome)) => done_body(window, what, true, outcome),
        Err(why) => done_body(window, what, false, why),
    };
    let _ = app(body);
    result.map(|(content, _)| content)
}

/// The middle of a rectangle given by its edges, none when it is empty.
pub fn centre(left: i32, top: i32, right: i32, bottom: i32) -> Option<(i32, i32)> {
    (right > left && bottom > top).then_some(((left + right) / 2, (top + bottom) / 2))
}

/// The window list as the agent reads it. `reach` says which windows are
/// out of this session's own reach, so the agent knows before its first
/// call there that Windows will ask the human.
pub fn windows_text(list: &[Window], reach: impl Fn(&Window) -> Reach) -> String {
    if list.is_empty() {
        return "No windows are open on the desktop.".into();
    }
    let mut out = String::from("Windows on the desktop, frontmost first:\n");
    let (mut above, mut unsure) = (false, false);
    for w in list {
        out.push_str(&format!(
            "- id={} {:?} {} pid {}",
            w.hwnd, w.title, w.exe, w.pid
        ));
        if let Some(o) = w.owner {
            out.push_str(&format!(" (dialog of id={o})"));
        }
        match reach(w) {
            Reach::Open => {}
            Reach::Above => {
                out.push_str(" (as administrator)");
                above = true;
            }
            Reach::Unsure => {
                out.push_str(" (out of reach)");
                unsure = true;
            }
            Reach::Beyond => {
                out.push_str(" (above administrator, beyond reach)");
            }
        }
        out.push('\n');
    }
    if unsure {
        out.push_str(
            "Out of reach means Windows would not tell this session how the program runs: as \
             administrator, or as another user. It is treated as running as administrator.\n",
        );
    }
    if above || unsure {
        out.push_str(
            "Such a window is reached through Horadric's helper. The first call there has \
             Windows ask the human to let the helper run as administrator.",
        );
    } else {
        out.pop();
    }
    out
}

/// A process's integrity level, as far as Windows tells: what decides
/// whether Windows lets one process's input and UI Automation into
/// another's windows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Integrity {
    /// The level's number: medium is 0x2000, high (administrator) 0x3000.
    Level(u32),
    /// Windows would not let this session read it, which happens for a
    /// process run by another user as well as for one above it.
    Unreadable,
    /// Windows did not say for another reason, the process gone.
    Gone,
}

/// How a window stands to this session.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reach {
    Open,
    /// Its program runs above this session: Windows drops input to it.
    Above,
    /// Its program would not say, and may run above.
    Unsure,
    /// Its program runs above administrator, a system process: beyond
    /// even the helper, so Windows is not asked for one.
    Beyond,
}

/// The integrity level of a process run as administrator. Above it runs
/// only what Windows itself runs.
pub const HIGH_LEVEL: u32 = 0x3000;

/// How a window of a process at `them` stands to this session at `me`.
/// Only a higher level walls a window off; one that can not be read may
/// be higher, and is kept from plain input to be safe.
pub fn reach(me: Integrity, them: Integrity) -> Reach {
    match (me, them) {
        (Integrity::Level(m), Integrity::Level(t)) if t > m && t > HIGH_LEVEL => Reach::Beyond,
        (Integrity::Level(m), Integrity::Level(t)) if t > m => Reach::Above,
        (Integrity::Level(_), Integrity::Unreadable) => Reach::Unsure,
        _ => Reach::Open,
    }
}

/// What an unnamed wait learned of the programs the helper watches.
#[derive(Debug, Default, PartialEq)]
pub struct Peeked {
    /// The helper's place in its own changes.
    pub newest: u64,
    /// The text waited for showed there, as said of the change.
    pub hit: Option<String>,
    /// Whether nothing changed there for the quiet waited for, when one
    /// was.
    pub quiet: Option<bool>,
    /// What changed there, a line each.
    pub lines: Vec<String>,
}

/// Reads the helper's answer to [`PEEK`].
pub fn peeked(v: &Value) -> Peeked {
    Peeked {
        newest: v["newest"].as_u64().unwrap_or(0),
        hit: v["hit"].as_str().map(String::from),
        quiet: v["quiet"].as_bool(),
        lines: v["lines"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// Which of a wait's ends holds when the helper's programs count too:
/// `local` is what holds here but quiet, `local_quiet` whether it has
/// been quiet here. Quiet needs both sides quiet, text either side.
pub fn joined_verdict(
    local: Option<String>,
    local_quiet: Option<String>,
    theirs: &Peeked,
) -> Option<String> {
    local
        .or_else(|| {
            theirs
                .hit
                .as_ref()
                .map(|h| format!("{h}, in a program that runs as administrator"))
        })
        .or_else(|| local_quiet.filter(|_| theirs.quiet != Some(false)))
}

/// The window `query` names: its id, else its title (whole, then a part),
/// else its program's file name, case ignored, frontmost first.
pub fn pick_window<'a>(list: &'a [Window], query: &str) -> Option<&'a Window> {
    let query = query.trim();
    if let Ok(id) = query.parse::<isize>() {
        return list.iter().find(|w| w.hwnd == id);
    }
    let q = query.to_lowercase();
    if q.is_empty() {
        return None;
    }
    list.iter()
        .find(|w| w.title.to_lowercase() == q)
        .or_else(|| list.iter().find(|w| w.title.to_lowercase().contains(&q)))
        .or_else(|| list.iter().find(|w| w.exe.to_lowercase().contains(&q)))
}

/// Which element `wanted` names, by its name or its automation id (a
/// program in another language changes the names but not the ids): a
/// whole match before a part of a name, a part of an id last, and at each
/// step one that `apt` marks as fit for the action first.
pub fn pick_name(names: &[String], ids: &[String], apt: &[bool], wanted: &str) -> Option<usize> {
    let w = wanted.trim().to_lowercase();
    if w.is_empty() {
        return None;
    }
    let low = |v: &[String]| -> Vec<String> { v.iter().map(|n| n.trim().to_lowercase()).collect() };
    let (names, ids) = (low(names), low(ids));
    let id = |i: usize| ids.get(i).map(String::as_str).unwrap_or("");
    let fit = |i: usize| apt.get(i).copied().unwrap_or(false);
    let whole = |i: usize| names[i] == w || id(i) == w;
    let part = |i: usize| names[i].contains(&w);
    let part_id = |i: usize| id(i).contains(&w);
    let steps: [&dyn Fn(usize) -> bool; 6] = [
        &|i| fit(i) && whole(i),
        &|i| fit(i) && part(i),
        &whole,
        &part,
        &|i| fit(i) && part_id(i),
        &part_id,
    ];
    steps
        .iter()
        .find_map(|step| (0..names.len()).find(|&i| step(i)))
}

/// A UI Automation control type's name.
pub fn control_name(id: i32) -> &'static str {
    const NAMES: [&str; 41] = [
        "Button",
        "Calendar",
        "CheckBox",
        "ComboBox",
        "Edit",
        "Hyperlink",
        "Image",
        "ListItem",
        "List",
        "Menu",
        "MenuBar",
        "MenuItem",
        "ProgressBar",
        "RadioButton",
        "ScrollBar",
        "Slider",
        "Spinner",
        "StatusBar",
        "Tab",
        "TabItem",
        "Text",
        "ToolBar",
        "ToolTip",
        "Tree",
        "TreeItem",
        "Custom",
        "Group",
        "Thumb",
        "DataGrid",
        "DataItem",
        "Document",
        "SplitButton",
        "Window",
        "Pane",
        "Header",
        "HeaderItem",
        "Table",
        "TitleBar",
        "Separator",
        "SemanticZoom",
        "AppBar",
    ];
    id.checked_sub(50000)
        .and_then(|i| NAMES.get(i as usize))
        .copied()
        .unwrap_or("Element")
}

/// One element of a window's tree, as a snapshot tells it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Node {
    pub depth: usize,
    pub control: &'static str,
    pub name: String,
    pub value: Option<String>,
    pub automation_id: String,
    pub enabled: bool,
    /// Left, top, width and height, from the window's top left corner.
    pub rect: [i32; 4],
    /// What it can do: invoke, toggle, select, expand, value.
    pub actions: Vec<&'static str>,
    /// What it is: checked, expanded, selected, focused, offscreen.
    pub state: Vec<&'static str>,
}

/// `s` on one line, cut to `most` characters.
pub fn short(s: &str, most: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= most {
        return s;
    }
    let mut cut: String = s.chars().take(most.saturating_sub(3)).collect();
    cut.push_str("...");
    cut
}

/// One element's line, with `r` its ref.
pub fn node_line(n: &Node, r: usize) -> String {
    let mut l = format!("{}- {}", "  ".repeat(n.depth), n.control);
    if !n.name.is_empty() {
        l.push_str(&format!(" {:?}", short(&n.name, 100)));
    }
    if let Some(v) = n.value.as_deref().filter(|v| !v.is_empty()) {
        l.push_str(&format!(" value={:?}", short(v, 80)));
    }
    if !n.automation_id.is_empty() {
        l.push_str(&format!(" id={}", short(&n.automation_id, 60)));
    }
    if !n.enabled {
        l.push_str(" disabled");
    }
    for s in &n.state {
        l.push(' ');
        l.push_str(s);
    }
    let [x, y, w, h] = n.rect;
    if w > 0 && h > 0 {
        l.push_str(&format!(" at {x},{y} {w}x{h}"));
    }
    if !n.actions.is_empty() {
        l.push_str(&format!(" ({})", n.actions.join(", ")));
    }
    l.push_str(&format!(" [ref=e{r}]"));
    l
}

/// A window's tree as text: `head`, then a line per node, refs counting
/// from e0, cut to `limit` bytes. `more` says the window had more elements
/// than were read.
pub fn tree_text(head: &str, nodes: &[Node], more: bool, limit: usize) -> String {
    let mut out = String::from(head);
    for (i, n) in nodes.iter().enumerate() {
        let line = node_line(n, i);
        if out.len() + line.len() + 1 > limit {
            out.push_str(&format!(
                "\n(cut short here at e{i} of {}: take a desktop_screenshot, or act on what is \
                 above and snapshot again)",
                nodes.len()
            ));
            return out;
        }
        out.push('\n');
        out.push_str(&line);
    }
    if nodes.is_empty() {
        out.push_str(
            "\n(UI Automation shows nothing inside this window: take a desktop_screenshot)",
        );
    }
    if more {
        out.push_str(&format!(
            "\n(the window has more than {MOST_ELEMENTS} elements; these are the first)"
        ));
    }
    out
}

/// The key a name stands for: a virtual key, or a character to be found
/// on the keyboard layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    Vk(u16),
    Char(char),
}

/// Modifier keys held while a key is pressed.
#[derive(Clone, Debug, PartialEq)]
pub struct Chord {
    pub mods: Vec<u16>,
    pub key: Key,
}

const VK_SHIFT: u16 = 0x10;
const VK_CONTROL: u16 = 0x11;
const VK_MENU: u16 = 0x12;
const VK_LWIN: u16 = 0x5B;
const VK_RETURN: u16 = 0x0D;
const VK_DELETE: u16 = 0x2E;
const VK_F4: u16 = 0x73;
const VK_S: u16 = b'S' as u16;
const VK_TAB: u16 = 0x09;

/// `spec` as keys: a key's name (`Enter`, `F5`, `a`) after any modifiers
/// joined by `+` (`Control+Shift+S`).
pub fn parse_key(spec: &str) -> Result<Chord, String> {
    let (mods, key) = match spec {
        "+" => ("", "+"),
        s if s.ends_with("++") => (&s[..s.len() - 2], "+"),
        s => s.rsplit_once('+').unwrap_or(("", s)),
    };
    if key.is_empty() {
        return Err(format!("no key in {spec:?}"));
    }
    let mut held = Vec::new();
    for m in mods.split('+').filter(|m| !m.is_empty()) {
        let vk = match m.trim().to_ascii_lowercase().as_str() {
            "control" | "ctrl" => VK_CONTROL,
            "shift" => VK_SHIFT,
            "alt" => VK_MENU,
            "win" | "meta" | "cmd" | "windows" => VK_LWIN,
            _ => return Err(format!("{m} is not a modifier: Control, Shift, Alt or Win")),
        };
        if !held.contains(&vk) {
            held.push(vk);
        }
    }
    let lower = key.to_ascii_lowercase();
    let named = match lower.as_str() {
        "enter" | "return" => Some(VK_RETURN),
        "tab" => Some(VK_TAB),
        "escape" | "esc" => Some(0x1B),
        "backspace" => Some(0x08),
        "delete" | "del" => Some(VK_DELETE),
        "insert" | "ins" => Some(0x2D),
        "space" | " " => Some(0x20),
        "up" | "arrowup" => Some(0x26),
        "down" | "arrowdown" => Some(0x28),
        "left" | "arrowleft" => Some(0x25),
        "right" | "arrowright" => Some(0x27),
        "home" => Some(0x24),
        "end" => Some(0x23),
        "pageup" | "pgup" => Some(0x21),
        "pagedown" | "pgdn" => Some(0x22),
        "menu" | "apps" | "contextmenu" => Some(0x5D),
        "printscreen" => Some(0x2C),
        "pause" => Some(0x13),
        "capslock" => Some(0x14),
        _ => None,
    };
    let function = lower
        .strip_prefix('f')
        .and_then(|n| n.parse::<u16>().ok())
        .filter(|n| (1..=24).contains(n))
        .map(|n| 0x70 + n - 1);
    let key = match (named.or(function), single(key)) {
        (Some(vk), _) => Key::Vk(vk),
        (None, Some(c)) if c.is_ascii_alphanumeric() => Key::Vk(c.to_ascii_uppercase() as u16),
        (None, Some(c)) => Key::Char(c),
        (None, None) => {
            return Err(format!(
                "{key:?} is not a key: a character, or a name like Enter, Tab, Escape, F5 or Down"
            ))
        }
    };
    Ok(Chord { mods: held, key })
}

fn single(s: &str) -> Option<char> {
    let mut c = s.chars();
    let first = c.next()?;
    c.next().is_none().then_some(first)
}

/// One key going down or up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stroke {
    Vk {
        vk: u16,
        up: bool,
    },
    /// A UTF-16 unit typed as itself, whatever the keyboard layout.
    Unit {
        unit: u16,
        up: bool,
    },
}

/// The strokes that press `key` with `mods` held, let go in reverse.
pub fn chord_strokes(mods: &[u16], key: u16) -> Vec<Stroke> {
    let mut out: Vec<Stroke> = mods
        .iter()
        .map(|&vk| Stroke::Vk { vk, up: false })
        .collect();
    out.push(Stroke::Vk { vk: key, up: false });
    out.push(Stroke::Vk { vk: key, up: true });
    out.extend(mods.iter().rev().map(|&vk| Stroke::Vk { vk, up: true }));
    out
}

/// The strokes that type `text`: line breaks and tabs as their keys, so a
/// field sees Enter and Tab, everything else as its own characters.
pub fn text_strokes(text: &str) -> Vec<Stroke> {
    let mut out = Vec::new();
    for c in text.chars() {
        match c {
            '\r' => {}
            '\n' => out.extend(chord_strokes(&[], VK_RETURN)),
            '\t' => out.extend(chord_strokes(&[], VK_TAB)),
            c => {
                let mut units = [0u16; 2];
                for &unit in c.encode_utf16(&mut units).iter() {
                    out.push(Stroke::Unit { unit, up: false });
                    out.push(Stroke::Unit { unit, up: true });
                }
            }
        }
    }
    out
}

/// Whether a virtual key is one of the extended keys, which Windows tells
/// apart from their numeric keypad twins by a flag.
pub fn is_extended(vk: u16) -> bool {
    matches!(
        vk,
        0x21..=0x28 | 0x2D | 0x2E | 0x5B | 0x5C | 0x5D | 0x6F | 0x90 | 0xA3 | 0xA5
    )
}

/// `w` by `h` shrunk to at most `most` wide, keeping its shape.
pub fn fit(w: u32, h: u32, most: u32) -> (u32, u32) {
    if w <= most || w == 0 {
        return (w, h);
    }
    let nh = (h as u64 * most as u64 / w as u64).max(1) as u32;
    (most, nh)
}

/// `[x, y, w, h]` cut to what lies inside a `width` by `height` picture,
/// none when nothing does.
pub fn clip(r: [i32; 4], width: i32, height: i32) -> Option<[i32; 4]> {
    let left = r[0].max(0);
    let top = r[1].max(0);
    let right = (r[0] + r[2]).min(width);
    let bottom = (r[1] + r[3]).min(height);
    (right > left && bottom > top).then_some([left, top, right - left, bottom - top])
}

/// A screen point as SendInput's absolute coordinates over the virtual
/// desktop at `left`, `top`, `width` by `height`: 0 to 65535 each way.
pub fn absolute(x: i32, y: i32, desk: [i32; 4]) -> (i32, i32) {
    let [left, top, width, height] = desk;
    let scale = |v: i32, origin: i32, size: i32| {
        if size <= 1 {
            return 0;
        }
        ((v - origin) as i64 * 65535 / (size - 1) as i64).clamp(0, 65535) as i32
    };
    (scale(x, left, width), scale(y, top, height))
}

/// The ref number in `e12`.
pub fn ref_number(r: &str) -> Option<usize> {
    r.trim().strip_prefix('e')?.parse().ok()
}

/// Why a chord is not pressed at all: the Windows key acts on the shell,
/// outside the program the human approved.
pub fn press_refusal(chord: &Chord) -> Option<String> {
    let win = chord.mods.contains(&VK_LWIN) || chord.key == Key::Vk(VK_LWIN);
    win.then(|| {
        "chords with the Windows key act on Windows itself, not on the approved program, so \
         they are not pressed"
            .to_string()
    })
}

/// Whether a key commits or destroys whatever it meets: Delete, Control+S
/// (saving over), Alt+F4 (closing, maybe through a prompt), and Enter in a
/// dialog, which confirms it.
pub fn risky_key(chord: &Chord, in_dialog: bool) -> bool {
    let held = |vk| chord.mods.contains(&vk);
    match chord.key {
        Key::Vk(VK_DELETE) => true,
        Key::Vk(VK_RETURN) => in_dialog,
        Key::Vk(VK_S) => held(VK_CONTROL),
        Key::Vk(VK_F4) => held(VK_MENU),
        _ => false,
    }
}

/// Horadric's overlay windows: the ghost cursor, the Thinking plate and
/// the frame. They never take the keyboard, so the keyboard is never given
/// back to one.
pub const OVERLAY_CLASSES: &[&str] = &["HoradricGhost"];

/// Whether, after an action on the program with process id `target`, the
/// keyboard goes back to `before`, the window that was in front before
/// it, given whether that window is `alive` still and which window (and
/// its process) is in front `now`.
///
/// It does, so the human's own typing does not land in the program the
/// agent drives, unless the human was in that program already, the
/// window is gone or an overlay, it is in front already, or something
/// else than the program took the front meanwhile (the human moved on,
/// and is not pulled back). A dialog the action opened gives no reason
/// to stay: it waits behind, and the next action brings it forward.
pub fn give_back(
    before: Option<(isize, u32, &str)>,
    alive: bool,
    target: u32,
    now: Option<(isize, u32)>,
) -> bool {
    let Some((hwnd, pid, class)) = before else {
        return false;
    };
    if hwnd == 0 || !alive || pid == target || OVERLAY_CLASSES.contains(&class) {
        return false;
    }
    match now {
        None => true,
        Some((h, _)) if h == hwnd => false,
        Some((_, p)) => p == target,
    }
}

/// What an action's answer adds when the keyboard went back, or would not.
pub fn given_back_text(title: &str, done: bool) -> String {
    if done {
        format!(" Focus was given back to {title:?}.")
    } else {
        format!(
            " Windows would not give the focus back to {title:?}, so the program keeps \
             the keyboard."
        )
    }
}

/// Whether `control` (a UI Automation control type) or `class` (its
/// window's class) is a plain edit field, whose text Windows itself keeps.
/// WinForms names its TextBox `WindowsForms10.EDIT...`, rich edits are
/// `RichEdit20W` and the like.
pub fn is_edit(control: &str, class: &str) -> bool {
    control == "Edit" || class.to_ascii_lowercase().contains("edit")
}

/// Whether a value read off an element answers `desktop_text`. An empty
/// one does for an edit field, which may well be empty; elsewhere it more
/// likely means the text is not kept in its value, so copying is tried.
pub fn read_counts(value: &str, edit: bool) -> bool {
    edit || !value.trim().is_empty()
}

/// How a click on an element is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickWay {
    /// A real click of the mouse at the element's middle.
    Real,
    /// Expanded or collapsed through UI Automation.
    Expand,
    /// It has no place on screen to click.
    Nowhere,
}

/// How to click an element: a `plain` left click or not, whether
/// `only_expands` (its one action is expanding, no invoke, toggle or
/// select) and whether it `has_place` on screen.
///
/// A click is a real one at the element. Its own action through UI
/// Automation (invoke, toggle, select) waits for the program to finish
/// with it, and a program that opens a modal dialog there never does, so
/// the call stood waiting and held every later call of this process to
/// that program. Only an element whose sole action is expanding is left to
/// UI Automation: a click at its middle may only put a caret in it (an
/// editable combo box) or miss the glyph (a tree node), and expanding shows
/// its children rather than running a command of the program's.
pub fn click_way(plain: bool, only_expands: bool, has_place: bool) -> ClickWay {
    match (plain && only_expands, has_place) {
        (true, _) => ClickWay::Expand,
        (false, true) => ClickWay::Real,
        (false, false) => ClickWay::Nowhere,
    }
}

/// Whether a rectangle on screen has room for a click.
pub fn has_place(r: RECT) -> bool {
    r.right > r.left && r.bottom > r.top
}

/// Whether a click would land on something other than the program with
/// process id `target`: nothing found at the point, or a top window of
/// another process (`under`). A dialog of the program is the program.
pub fn covered(found: bool, under: u32, target: u32) -> bool {
    !found || under != target
}

/// What a failed Windows call says: its message, or for the codes that
/// have none, what the code means, and the code itself.
pub fn error_reason(code: u32, message: &str) -> String {
    let message = message.trim();
    if !message.is_empty() {
        return message.to_string();
    }
    let known = match code {
        0x8013_1505 => "UI Automation timed out: the program did not answer",
        0x8004_0200 => "the element is not enabled",
        0x8004_0201 => "the element is gone",
        0x8004_0202 => "the element has no point to click",
        0x8013_1509 => "the program does not allow that now",
        _ => "Windows gave no reason",
    };
    format!("{known} (0x{code:08X})")
}

// The rest touches Windows, and is verified on screen rather than tested.

/// A COM object moved to another thread. Sound here because COM is set up
/// multithreaded, where UI Automation's objects may be called from any
/// thread of the apartment.
struct Sent<T>(T);
unsafe impl<T> Send for Sent<T> {}
impl<T> Sent<T> {
    fn get(&self) -> &T {
        &self.0
    }
}

struct Com {
    uia: IUIAutomation,
    /// Everything a snapshot shows, for a window's whole tree.
    tree: IUIAutomationCacheRequest,
    /// The same for one element, or each element a search finds.
    one: IUIAutomationCacheRequest,
}

/// The elements of the last snapshot, by ref.
struct Snap {
    hwnd: isize,
    elements: Vec<IUIAutomationElement>,
}

/// The desktop side of the MCP server, set up on first use.
#[derive(Default)]
pub struct Desktop {
    com: Option<Com>,
    snap: Option<Snap>,
    /// The helper that reaches programs running as administrator, once
    /// one has been needed.
    helper: Option<Helper>,
    /// Windows asking the human to let the helper run, still unanswered
    /// when the call that asked gave up waiting.
    starting: Option<Starting>,
    /// When Windows last asked the human for the helper.
    asked: Option<Instant>,
    /// What last kept the helper away, why, and when, so Windows does not
    /// ask the human again on every call.
    setback: Option<(Setback, String, Instant)>,
    /// The last change `desktop_changes` told, per program (none for
    /// all), so the next call starts after it.
    seen: HashMap<Option<u32>, u64>,
    /// This is the helper, whose changes are a part of the session's
    /// answer: its ring numbers apart from the session's, so it keeps its
    /// own place and says no numbers.
    part: bool,
}

/// Why a call for an elevated window did not go to the helper.
enum Unreached {
    /// There is no helper for now: the call goes on here as far as Windows
    /// lets it, looking at the title bar and the screen, and says why.
    Walled(String),
    /// The call ends with this: Windows still asks the human, or the
    /// helper was lost in the middle of it.
    Stop(String),
}

/// What is known about the element an action aims at.
struct Aim {
    el: IUIAutomationElement,
    control: &'static str,
    name: String,
    automation_id: String,
    /// On screen.
    rect: RECT,
    invoke: bool,
    toggle: bool,
    select: bool,
    expand: bool,
    expanded: bool,
    writable: bool,
}

impl Aim {
    fn what(&self) -> String {
        if self.name.is_empty() {
            self.control.to_string()
        } else {
            format!("{} {:?}", self.control, short(&self.name, 60))
        }
    }
}

const PROPERTIES: [UIA_PROPERTY_ID; 17] = [
    UIA_NamePropertyId,
    UIA_ControlTypePropertyId,
    UIA_AutomationIdPropertyId,
    UIA_IsEnabledPropertyId,
    UIA_BoundingRectanglePropertyId,
    UIA_IsOffscreenPropertyId,
    UIA_HasKeyboardFocusPropertyId,
    UIA_IsInvokePatternAvailablePropertyId,
    UIA_IsTogglePatternAvailablePropertyId,
    UIA_IsSelectionItemPatternAvailablePropertyId,
    UIA_IsExpandCollapsePatternAvailablePropertyId,
    UIA_IsValuePatternAvailablePropertyId,
    UIA_ValueValuePropertyId,
    UIA_ValueIsReadOnlyPropertyId,
    UIA_ToggleToggleStatePropertyId,
    UIA_ExpandCollapseExpandCollapseStatePropertyId,
    UIA_SelectionItemIsSelectedPropertyId,
];

fn win_err(e: windows::core::Error) -> String {
    if e.code() == E_ACCESSDENIED {
        return "Windows refused (the program may run as administrator, which an agent that \
                does not can not reach)"
            .into();
    }
    error_reason(e.code().0 as u32, &e.message())
}

fn cached_bool(el: &IUIAutomationElement, p: UIA_PROPERTY_ID) -> bool {
    unsafe { el.GetCachedPropertyValue(p) }
        .ok()
        .and_then(|v: VARIANT| bool::try_from(&v).ok())
        .unwrap_or(false)
}

fn cached_i32(el: &IUIAutomationElement, p: UIA_PROPERTY_ID) -> Option<i32> {
    unsafe { el.GetCachedPropertyValue(p) }
        .ok()
        .and_then(|v: VARIANT| i32::try_from(&v).ok())
}

fn cached_string(el: &IUIAutomationElement, p: UIA_PROPERTY_ID) -> Option<String> {
    unsafe { el.GetCachedPropertyValue(p) }
        .ok()
        .and_then(|v: VARIANT| BSTR::try_from(&v).ok())
        .map(|b| b.to_string())
}

fn hwnd(id: isize) -> HWND {
    HWND(id as *mut _)
}

fn window_rect(id: isize) -> RECT {
    let mut r = RECT::default();
    let _ = unsafe { GetWindowRect(hwnd(id), &mut r) };
    r
}

/// What [`exe_of`] says when Windows will not tell.
const UNKNOWN_EXE: &str = "?";

/// The program's file name and its full path, which is empty when Windows
/// will not tell.
fn exe_of(pid: u32) -> (String, String) {
    let unknown = || (UNKNOWN_EXE.to_string(), String::new());
    unsafe {
        let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return unknown();
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            p,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(p);
        if ok.is_err() {
            return unknown();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let exe = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string();
        (exe, path)
    }
}

/// The access right to read an object's security descriptor, its label
/// among it.
const READ_CONTROL: PROCESS_ACCESS_RIGHTS = PROCESS_ACCESS_RIGHTS(0x0002_0000);
/// The type of the ACE that holds an object's integrity label.
const MANDATORY_LABEL_ACE: u8 = 0x11;

/// The last part of `sid`, which for an integrity label is its level.
unsafe fn last_rid(sid: PSID) -> Option<u32> {
    let count = GetSidSubAuthorityCount(sid);
    if count.is_null() || *count == 0 {
        return None;
    }
    let rid = GetSidSubAuthority(sid, u32::from(*count) - 1);
    (!rid.is_null()).then(|| *rid)
}

/// The integrity level of the process whose token is `token`.
unsafe fn token_level(token: HANDLE) -> Option<u32> {
    let mut len = 0;
    let _ = GetTokenInformation(token, TokenIntegrityLevel, None, 0, &mut len);
    if len == 0 {
        return None;
    }
    // In u64s, so the label's pointer inside is aligned.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    GetTokenInformation(
        token,
        TokenIntegrityLevel,
        Some(buf.as_mut_ptr().cast()),
        len,
        &mut len,
    )
    .ok()?;
    let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
    last_rid(label.Label.Sid)
}

/// The integrity level on the process object itself, which another user's
/// process often lets be read when its token is not. No label means
/// medium, as Windows reads it.
unsafe fn object_level(process: HANDLE) -> Option<u32> {
    let mut len = 0;
    let _ = GetKernelObjectSecurity(process, LABEL_SECURITY_INFORMATION.0, None, 0, &mut len);
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    let sd = PSECURITY_DESCRIPTOR(buf.as_mut_ptr().cast());
    GetKernelObjectSecurity(
        process,
        LABEL_SECURITY_INFORMATION.0,
        Some(sd),
        len,
        &mut len,
    )
    .ok()?;
    let (mut present, mut defaulted) = (BOOL(0), BOOL(0));
    let mut sacl: *mut ACL = std::ptr::null_mut();
    GetSecurityDescriptorSacl(sd, &mut present, &mut sacl, &mut defaulted).ok()?;
    if !present.as_bool() || sacl.is_null() {
        return Some(MEDIUM_LEVEL);
    }
    for i in 0..u32::from((*sacl).AceCount) {
        let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
        if GetAce(sacl, i, &mut ace).is_err() || ace.is_null() {
            continue;
        }
        // Header (4 bytes) and access mask (4 bytes), then the label.
        let ace = ace as *const u8;
        if *ace == MANDATORY_LABEL_ACE {
            return last_rid(PSID(ace.add(8) as *mut _));
        }
    }
    Some(MEDIUM_LEVEL)
}

/// The level Windows gives a process without a label.
const MEDIUM_LEVEL: u32 = 0x2000;

/// The integrity level of process `pid`, or of this one when none.
fn integrity(pid: Option<u32>) -> Integrity {
    unsafe {
        let process = match pid {
            Some(id) => match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, id) {
                Ok(p) => Some(p),
                Err(e) if e.code() == E_ACCESSDENIED => None,
                Err(_) => return Integrity::Gone,
            },
            None => Some(GetCurrentProcess()),
        };
        let mut level = None;
        if let Some(p) = process {
            let mut token = HANDLE::default();
            if OpenProcessToken(p, TOKEN_QUERY, &mut token).is_ok() {
                level = token_level(token);
                let _ = CloseHandle(token);
            }
            if pid.is_some() {
                let _ = CloseHandle(p);
            }
        }
        if let (None, Some(id)) = (level, pid) {
            if let Ok(p) = OpenProcess(READ_CONTROL, false, id) {
                level = object_level(p);
                let _ = CloseHandle(p);
            }
        }
        level.map_or(Integrity::Unreadable, Integrity::Level)
    }
}

/// How `w` stands to this session.
fn reach_of(w: &Window) -> Reach {
    reach(integrity(None), integrity(Some(w.pid)))
}

/// Why `w` can not be read or operated, when its program runs above this
/// session, or may: Windows then shows UI Automation only its title bar
/// and drops input sent to it. Also whether the helper may reach it, so
/// Windows is never asked for a helper that could not. `helper` says this
/// is the helper, which runs as administrator itself and has nothing
/// further to hand a call to.
fn out_of_reach(w: &Window, helper: bool) -> Option<(String, bool)> {
    let program = program_of(w);
    let reason = match (reach_of(w), helper) {
        (Reach::Open, _) => return None,
        (Reach::Beyond, _) | (Reach::Above, true) => {
            let beyond = format!(
                "{program} runs above administrator, as part of Windows, beyond even \
                 Horadric's helper, so Windows shows the agent only its title bar and lets it \
                 neither click nor type there"
            );
            return Some((beyond, false));
        }
        (Reach::Unsure, true) => {
            let unsure = format!(
                "Windows would not tell even Horadric's helper how {program} runs (another \
                 user's program, or part of Windows), so it is kept from clicks and keys that \
                 Windows might drop without a word"
            );
            return Some((unsure, false));
        }
        (Reach::Above, false) => format!(
            "{program} runs as administrator and this session does not, so Windows shows the \
             agent only its title bar and lets it neither click nor type there (pictures come \
             off the screen). It can only be reached through a helper that runs as \
             administrator"
        ),
        (Reach::Unsure, false) => format!(
            "Windows would not tell this session how {program} runs, as administrator or as \
             another user, so it is kept from plain clicks and keys, which Windows would drop \
             without a word if it runs as administrator. It is reached through a helper that \
             runs as administrator"
        ),
    };
    Some((reason, true))
}

/// The program as the agent reads it named, its title when Windows will
/// not tell its file.
fn program_of(w: &Window) -> String {
    if w.exe == UNKNOWN_EXE {
        format!("the program of {:?}", w.title)
    } else {
        w.exe.clone()
    }
}

/// The window at the end of `h`'s chain of owners, the program's main
/// window for a dialog. Windows' own GA_ROOTOWNER follows owners only for
/// popup windows, which a WinForms or WPF dialog is not.
fn root_owner(h: HWND) -> HWND {
    let top = unsafe { GetAncestor(h, GA_ROOT) };
    let mut at = if top.is_invalid() { h } else { top };
    for _ in 0..32 {
        match unsafe { GetWindow(at, GW_OWNER) } {
            Ok(o) if !o.is_invalid() => at = o,
            _ => break,
        }
    }
    at
}

/// Whether `w` is a dialog: owned by another window, or of the standard
/// dialog class.
fn is_dialog(w: &Window) -> bool {
    w.owner.is_some() || w.class == "#32770"
}

pub(crate) fn info(id: isize) -> Window {
    unsafe {
        let h = hwnd(id);
        let mut buf = [0u16; 512];
        let n = GetWindowTextW(h, &mut buf);
        let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        let n = GetClassNameW(h, &mut buf);
        let class = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        let mut pid = 0;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let owner = GetWindow(h, GW_OWNER)
            .ok()
            .filter(|o| !o.is_invalid())
            .map(|o| o.0 as isize);
        let (exe, path) = exe_of(pid);
        Window {
            hwnd: id,
            title,
            exe,
            path,
            pid,
            owner,
            class,
        }
    }
}

unsafe extern "system" fn each(h: HWND, list: LPARAM) -> windows::core::BOOL {
    (*(list.0 as *mut Vec<isize>)).push(h.0 as isize);
    true.into()
}

/// The windows a person could see on the desktop, frontmost first,
/// Horadric's own left out.
pub fn list_windows() -> Vec<Window> {
    let mut all: Vec<isize> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut all as *mut _ as isize));
    }
    all.into_iter()
        .filter(|&id| unsafe {
            let h = hwnd(id);
            if !IsWindowVisible(h).as_bool() {
                return false;
            }
            if GetWindowLongW(h, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
                return false;
            }
            let mut cloaked = 0u32;
            let _ = DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4);
            if cloaked != 0 {
                return false;
            }
            let r = window_rect(id);
            r.right > r.left && r.bottom > r.top
        })
        .map(info)
        .filter(|w| !w.title.is_empty() && !w.exe.to_lowercase().starts_with("horadric"))
        .collect()
}

/// Finds the window `query` names, and the top level window the human
/// approves for it: its root owner, the program's main window.
fn find(query: Option<&Value>) -> Result<(Window, Window), String> {
    let query = match query {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => {
            return Err("say which window: an id from desktop_windows, or part of its title".into())
        }
    };
    let list = list_windows();
    let target = pick_window(&list, &query).cloned().ok_or(format!(
        "no window matches {query:?}; desktop_windows lists them"
    ))?;
    // A program that will not say its file is refused by the caller, once
    // it knows the helper can not tell it either.
    let root_id = root_owner(hwnd(target.hwnd)).0 as isize;
    let mut root = if root_id == 0 || root_id == target.hwnd {
        target.clone()
    } else {
        info(root_id)
    };
    // The human approves a program: a window owned by another process is
    // approved as its own process, never under its owner's name.
    if root.pid != target.pid || root.exe == UNKNOWN_EXE {
        root = target.clone();
    }
    if root.title.is_empty() {
        root.title = target.title.clone();
    }
    Ok((target, root))
}

/// Brings `w` to the front so keys and clicks reach it. Anything else in
/// front, even a dialog of the same program, is refused: input would go
/// to a window the human did not see named, and a dialog there may be the
/// one that confirms a transfer.
fn bring_forward(w: &Window) -> Result<(), String> {
    unsafe {
        let h = hwnd(w.hwnd);
        // A window that is switched off has a modal dialog up, and Windows
        // may pass what reaches it on to that dialog.
        if !IsWindowEnabled(h).as_bool() {
            let popup = GetLastActivePopup(h);
            let named = if popup.is_invalid() || popup == h {
                "a modal dialog".to_string()
            } else {
                let p = info(popup.0 as isize);
                format!("{:?} (id={})", p.title, p.hwnd)
            };
            return Err(format!(
                "{:?} is waiting on {named}, so input would not reach it and nothing was sent; \
                 read the dialog and aim at it by its id",
                w.title
            ));
        }
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        if GetForegroundWindow() != h {
            ask_front(h);
            thread::sleep(Duration::from_millis(80));
        }
        let front = GetForegroundWindow();
        if front == h {
            return Ok(());
        }
        let ours = root_owner(h);
        if !front.is_invalid() && root_owner(front) == ours {
            let f = info(front.0 as isize);
            return Err(format!(
                "{:?} (id={}) of the same program is in front of {:?}, so input would go there \
                 and nothing was sent; read it and aim at it by its id",
                f.title, f.hwnd, w.title
            ));
        }
        Err(format!(
            "Windows would not bring {:?} to the front, so keys and clicks would go elsewhere; \
             ask the user to click it once",
            w.title
        ))
    }
}

/// Asks Windows to put `h` in front.
unsafe fn ask_front(h: HWND) {
    // Windows lets a process bring a window forward when it sent the last
    // input, and a mouse move by nothing is input enough.
    send(&[mouse(0, 0, MOUSEEVENTF_MOVE)]);
    if !SetForegroundWindow(h).as_bool() {
        let front = GetWindowThreadProcessId(GetForegroundWindow(), None);
        let me = GetCurrentThreadId();
        let _ = AttachThreadInput(me, front, true);
        let _ = SetForegroundWindow(h);
        let _ = BringWindowToTop(h);
        let _ = AttachThreadInput(me, front, false);
    }
}

/// The window in front, and its process.
fn foreground() -> Option<(isize, u32)> {
    let h = unsafe { GetForegroundWindow() };
    if h.is_invalid() {
        return None;
    }
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    Some((h.0 as isize, pid))
}

/// The window in front before an action, to give the keyboard back to.
fn front_before() -> Option<Window> {
    foreground().map(|(id, _)| info(id))
}

/// How long the window given the keyboard back is given to come to the
/// front.
const FRONT_WAIT: Duration = Duration::from_millis(150);

/// Gives the keyboard back to `before` after an action on `w`, when
/// [`give_back`] says so, and what the answer adds about it.
fn hand_back(before: Option<&Window>, w: &Window) -> String {
    let Some(b) = before else {
        return String::new();
    };
    let h = hwnd(b.hwnd);
    let alive = unsafe { IsWindow(Some(h)).as_bool() && IsWindowVisible(h).as_bool() };
    if !give_back(Some((b.hwnd, b.pid, &b.class)), alive, w.pid, foreground()) {
        return String::new();
    }
    // Keys sent a moment ago may still be on their way to the program, and
    // would land in the human's window if it were in front first.
    thread::sleep(Duration::from_millis(50));
    unsafe { ask_front(h) };
    let begun = Instant::now();
    while foreground().map(|(id, _)| id) != Some(b.hwnd) && begun.elapsed() < FRONT_WAIT {
        thread::sleep(Duration::from_millis(10));
    }
    given_back_text(&b.title, foreground().map(|(id, _)| id) == Some(b.hwnd))
}

/// Gives the keyboard back after an action, at once so the program holds
/// it as briefly as can be, and again after `settle` when the action went
/// through, since a dialog it opened may have taken the front meanwhile.
fn after_action(before: Option<&Window>, w: &Window, settle: Duration, ok: bool) -> String {
    let mut said = hand_back(before, w);
    if ok {
        thread::sleep(settle);
        let again = hand_back(before, w);
        if !again.is_empty() {
            said = again;
        }
    }
    said
}

fn mouse(dx: i32, dy: i32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key_input(s: Stroke) -> INPUT {
    let (vk, scan, flags) = match s {
        Stroke::Vk { vk, up } => {
            let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
            let mut f = KEYBD_EVENT_FLAGS(0);
            if is_extended(vk) {
                f |= KEYEVENTF_EXTENDEDKEY;
            }
            if up {
                f |= KEYEVENTF_KEYUP;
            }
            (vk, scan, f)
        }
        Stroke::Unit { unit, up } => {
            let f = if up {
                KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
            } else {
                KEYEVENTF_UNICODE
            };
            (0, unit, f)
        }
    };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Sends `inputs`, whether Windows took them all.
fn send(inputs: &[INPUT]) -> bool {
    if inputs.is_empty() {
        return true;
    }
    let n = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    n as usize == inputs.len()
}

fn send_strokes(strokes: &[Stroke]) -> Result<(), String> {
    let inputs: Vec<INPUT> = strokes.iter().map(|&s| key_input(s)).collect();
    // A long text goes in pieces, so a program reading its queue keeps up.
    for piece in inputs.chunks(40) {
        if !send(piece) {
            return Err(BLOCKED.into());
        }
        thread::sleep(Duration::from_millis(15));
    }
    Ok(())
}

const BLOCKED: &str = "Windows blocked the input (the program may run as administrator, or \
                       the screen is locked)";

/// The strokes for a chord, its character looked up on the keyboard layout.
fn chord_input(chord: &Chord) -> Vec<Stroke> {
    match chord.key {
        Key::Vk(vk) => chord_strokes(&chord.mods, vk),
        Key::Char(c) => {
            let mut units = [0u16; 2];
            let units = c.encode_utf16(&mut units);
            let scan = if units.len() == 1 {
                unsafe { VkKeyScanW(units[0]) }
            } else {
                -1
            };
            if scan == -1 {
                return text_strokes(&c.to_string());
            }
            let vk = (scan as u16) & 0xFF;
            let shifts = (scan as u16 >> 8) & 0xFF;
            let mut mods = chord.mods.clone();
            for (bit, m) in [(1, VK_SHIFT), (2, VK_CONTROL), (4, VK_MENU)] {
                if shifts & bit != 0 && !mods.contains(&m) {
                    mods.push(m);
                }
            }
            chord_strokes(&mods, vk)
        }
    }
}

/// Runs a UI Automation pattern on its own thread, so one the program
/// holds (a button whose dialog is modal) does not hold this server.
/// Whether it finished in time.
fn on_the_side(
    el: &IUIAutomationElement,
    act: impl FnOnce(&IUIAutomationElement) -> windows::core::Result<()> + Send + 'static,
) -> Result<bool, String> {
    let el = Sent(el.clone());
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let r = act(el.get()).map_err(win_err);
        let _ = tx.send(r);
    });
    match rx.recv_timeout(PATTERN_WAIT) {
        Ok(r) => r.map(|()| true),
        Err(_) => Ok(false),
    }
}

/// Reads something through UI Automation on its own thread, as
/// [`on_the_side`] acts, so a program that holds the call does not hold
/// this server. None when it fails or takes too long.
fn read_on_the_side<T: Send + 'static>(
    el: &IUIAutomationElement,
    read: impl FnOnce(&IUIAutomationElement) -> windows::core::Result<T> + Send + 'static,
) -> Option<T> {
    let el = Sent(el.clone());
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let _ = tx.send(read(el.get()).ok());
    });
    rx.recv_timeout(PATTERN_WAIT).ok().flatten()
}

/// Scrolls an element into view through UI Automation where it can be,
/// and where it is then.
fn scroll_into_view(el: &IUIAutomationElement) -> Option<RECT> {
    let scrolled = on_the_side(el, |el| unsafe {
        el.GetCurrentPatternAs::<IUIAutomationScrollItemPattern>(UIA_ScrollItemPatternId)?
            .ScrollIntoView()
    });
    if scrolled != Ok(true) {
        return None;
    }
    read_on_the_side(el, |el| unsafe { el.CurrentBoundingRectangle() }).filter(|r| has_place(*r))
}

/// What a field holds now, read live.
fn value_now(el: &IUIAutomationElement) -> Option<String> {
    read_on_the_side(el, |el| unsafe {
        let p = el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)?;
        Ok(p.CurrentValue()?.to_string())
    })
}

/// Another program can hold the clipboard open for a moment.
fn open_clipboard() -> bool {
    for _ in 0..10 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

/// The clipboard's text, if it holds any.
fn clip_text() -> Option<String> {
    if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) }.is_err() || !open_clipboard() {
        return None;
    }
    let text = unsafe {
        GetClipboardData(CF_UNICODETEXT).ok().and_then(|h| {
            let mem = HGLOBAL(h.0);
            let ptr = GlobalLock(mem) as *const u16;
            if ptr.is_null() {
                return None;
            }
            let mut len = 0;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            let _ = GlobalUnlock(mem);
            Some(s)
        })
    };
    unsafe {
        let _ = CloseClipboard();
    }
    text
}

/// Empties the clipboard.
fn clear_clip() {
    if open_clipboard() {
        unsafe {
            let _ = EmptyClipboard();
            let _ = CloseClipboard();
        }
    }
}

/// The class of window `h`.
fn class_of(h: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// The text Windows keeps for window `h`, an edit field's contents, asked
/// with a time limit so a hung program does not hold this server.
fn window_text(h: HWND) -> Option<String> {
    let ask = |msg, wparam: usize, lparam: isize| {
        let mut out = 0usize;
        let ok = unsafe {
            SendMessageTimeoutW(
                h,
                msg,
                WPARAM(wparam),
                LPARAM(lparam),
                SMTO_ABORTIFHUNG,
                2000,
                Some(&mut out),
            )
        };
        (ok.0 != 0).then_some(out)
    };
    let len = ask(WM_GETTEXTLENGTH, 0, 0)?;
    let mut buf = vec![0u16; len + 1];
    let got = ask(WM_GETTEXT, buf.len(), buf.as_mut_ptr() as isize)?;
    Some(String::from_utf16_lossy(&buf[..got.min(len)]))
}

/// Puts `text` on the clipboard, in place of what it held.
fn set_clip_text(text: &str) {
    if !open_clipboard() {
        return;
    }
    unsafe {
        let _ = EmptyClipboard();
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        if let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) {
            let ptr = GlobalLock(mem) as *mut u16;
            if !ptr.is_null() {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
                let _ = GlobalUnlock(mem);
                // The clipboard owns the memory once it takes it.
                if SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0))).is_err() {
                    let _ = GlobalFree(Some(mem));
                }
            }
        }
        let _ = CloseClipboard();
    }
}

impl Desktop {
    /// The desktop as the helper serves it.
    pub fn for_helper() -> Desktop {
        Desktop {
            part: true,
            ..Desktop::default()
        }
    }

    fn com(&mut self) -> Result<&Com, String> {
        if self.com.is_none() {
            let com = unsafe {
                let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                let uia: IUIAutomation =
                    CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                        .map_err(|e| format!("UI Automation is not available: {}", win_err(e)))?;
                let request = |scope| -> windows::core::Result<IUIAutomationCacheRequest> {
                    let r = uia.CreateCacheRequest()?;
                    for p in PROPERTIES {
                        r.AddProperty(p)?;
                    }
                    r.SetTreeScope(scope)?;
                    Ok(r)
                };
                Com {
                    tree: request(TreeScope_Subtree).map_err(win_err)?,
                    one: request(TreeScope_Element).map_err(win_err)?,
                    uia,
                }
            };
            self.com = Some(com);
        }
        Ok(self.com.as_ref().expect("set just above"))
    }

    /// Hands a call for an elevated window `w` to the helper, starting it
    /// when there is none. The outer error says the call can not go to the
    /// helper, and whether the call may still go ahead here, as far as
    /// Windows lets it.
    fn through_helper<A: FnMut(Value) -> Result<Value, String>>(
        &mut self,
        name: &str,
        args: &Value,
        app: &mut A,
        settle: Duration,
        w: &Window,
    ) -> Result<Result<Vec<Value>, String>, Unreached> {
        self.helper_for(w, app)?;
        let helper = self.helper.as_mut().expect("started just above");
        // A grant for a program does not cover the same program running as
        // administrator: the human is asked about that one by itself.
        let mut marked = |body: Value| app(as_administrator(body));
        let answer = helper.call(name, args, &mut marked, settle);
        answer.map_err(|why| {
            self.lose_helper(&why);
            Unreached::Stop(format!(
                "{why}. It may already have acted: look before you try again"
            ))
        })
    }

    /// Makes sure the helper runs, asking Windows for it when nothing
    /// holds that back, and waiting only so long for the human's answer.
    fn helper_for<A: FnMut(Value) -> Result<Value, String>>(
        &mut self,
        w: &Window,
        app: &mut A,
    ) -> Result<(), Unreached> {
        if self.helper.is_some() {
            return Ok(());
        }
        let most = if self.starting.is_some() {
            ASK_AGAIN_WAIT
        } else {
            if let Some((setback, why, at)) = &self.setback {
                let since_asked = self.asked.map_or(Duration::MAX, |a| a.elapsed());
                if let Some(left) = hold_left(*setback, at.elapsed(), since_asked) {
                    return Err(Unreached::Walled(held_text(*setback, why, left)));
                }
            }
            // The human sees why Windows asks before it does; an app from
            // before this word ignores it.
            let _ = app(elevating_body(window_json(w)));
            thread::sleep(ANNOUNCE_PAUSE);
            self.asked = Some(Instant::now());
            self.starting = Some(Starting::begin());
            ASK_WAIT
        };
        let starting = self.starting.as_ref().expect("set just above");
        let Some(started) = starting.wait(most) else {
            return Err(Unreached::Stop(still_asking_text(
                &program_of(w),
                starting.asked.elapsed(),
            )));
        };
        self.starting = None;
        if self.started(started) {
            return Ok(());
        }
        let (setback, why, at) = self.setback.clone().expect("set by a start that failed");
        let since_asked = self.asked.map_or(Duration::MAX, |a| a.elapsed());
        let left = hold_left(setback, at.elapsed(), since_asked).unwrap_or_default();
        Err(Unreached::Walled(held_text(setback, &why, left)))
    }

    /// Keeps how a start of the helper ended: the helper, or the setback
    /// timed from when the start thread saw it end, not from when a call
    /// came by to look. Says whether the helper runs.
    fn started(&mut self, (started, at): (Result<Helper, HelperStart>, Instant)) -> bool {
        let (setback, why) = match started {
            Ok(h) => {
                self.helper = Some(h);
                self.setback = None;
                return true;
            }
            Err(HelperStart::Declined(why)) => (Setback::Declined, why),
            Err(HelperStart::Failed(why)) => (Setback::Failed, why),
        };
        self.setback = Some((setback, why, at));
        false
    }

    /// Picks up a start that ended since the call that asked gave up
    /// waiting, without waiting for one still going.
    fn collect_start(&mut self) {
        if let Some(ended) = self.starting.as_ref().and_then(|s| s.wait(Duration::ZERO)) {
            self.starting = None;
            self.started(ended);
        }
    }

    /// Forgets a helper that is gone, so the next call may start another
    /// once the hold is over.
    fn lose_helper(&mut self, why: &str) {
        self.helper = None;
        self.setback = Some((Setback::Lost, why.to_string(), Instant::now()));
    }

    /// Calls the helper for something of its own, asking the app nothing.
    /// An error that is not the helper's answer loses it.
    fn ask_helper(&mut self, name: &str, args: &Value) -> Result<Vec<Value>, String> {
        let helper = self.helper.as_mut().ok_or("the helper is not running")?;
        let mut nobody =
            |_: Value| -> Result<Value, String> { Err("this call asks Horadric nothing".into()) };
        match helper.call(name, args, &mut nobody, Duration::ZERO) {
            Ok(answer) => answer,
            Err(why) => {
                self.lose_helper(&why);
                Err(why)
            }
        }
    }

    /// Answers a `desktop_` tool, asking `app` for the human's leave first.
    pub fn tool<A: FnMut(Value) -> Result<Value, String>>(
        &mut self,
        name: &str,
        args: &Value,
        app: &mut A,
        settle: Duration,
    ) -> Result<Vec<Value>, String> {
        let flag = |k: &str| args.get(k) == Some(&json!(true));
        let s = |k: &str| args.get(k).and_then(Value::as_str);
        if name == "desktop_windows" {
            return Ok(vec![text(windows_text(&list_windows(), reach_of))]);
        }
        if name == PEEK && self.part {
            return Ok(vec![self.peek(args)]);
        }
        // Without a window these read what is watched already, every
        // program in it approved earlier, so there is nothing to ask.
        let unnamed = args.get("window").is_none_or(Value::is_null);
        if unnamed {
            // A helper that connected after its call gave up waiting is
            // picked up here too, and a no is timed from when it came.
            self.collect_start();
        }
        match name {
            "desktop_changes" if unnamed => return Ok(vec![text(self.changes(args, None, app))]),
            "desktop_wait" if unnamed => return Ok(vec![text(self.wait(args, None)?)]),
            _ => {}
        }
        let (w, root) = find(args.get("window"))?;
        let (mut walled, by_helper) = out_of_reach(&w, self.part).unzip();
        if let (Some(why), Some(true)) = (&walled, by_helper) {
            match self.through_helper(name, args, app, settle, &w) {
                Ok(answer) => return answer,
                Err(Unreached::Stop(no)) => return Err(no),
                Err(Unreached::Walled(no)) => walled = Some(format!("{why}, and {no}")),
            }
        }
        // Windows drops input sent to an elevated window without a word,
        // so nothing that types, clicks or copies is sent there at all.
        if let (Some(why), "desktop_click" | "desktop_type" | "desktop_press" | "desktop_text") =
            (&walled, name)
        {
            return Err(why.clone());
        }
        if w.exe == UNKNOWN_EXE {
            let mut no = format!(
                "can not tell which program {:?} is, so it can not be approved",
                w.title
            );
            if let Some(why) = &walled {
                no.push_str(&format!(" ({why})"));
            }
            return Err(no);
        }
        // The agent's words for the human, kept to one line.
        let reason = short(s("reason").unwrap_or_default(), 300);
        match name {
            "desktop_snapshot" => {
                let what = format!("read the controls of {:?}", w.title);
                allow(app, &root, "see", &what, false, None, &reason)?;
                watch::follow(w.pid);
                let result = self.snapshot(&w).map(|mut tree| {
                    if let Some(why) = walled {
                        tree.push_str(&format!("\n(Note: {why}.)"));
                    }
                    (vec![text(tree)], "Read the window's controls.".to_string())
                });
                report(app, &root, &what, result)
            }
            "desktop_screenshot" => {
                let crop = match s("ref") {
                    Some(r) => Some(self.by_ref(&w, r)?),
                    None => None,
                };
                let what = format!("take a picture of {:?}", w.title);
                allow(app, &root, "see", &what, false, None, &reason)?;
                watch::follow(w.pid);
                let result =
                    self.picture(&w, crop.as_ref().map(|a| a.rect))
                        .map(|(png, copied)| {
                            let mut content = vec![json!({
                                "type": "image",
                                "data": horadric_core::release::base64_encode(&png),
                                "mimeType": "image/png",
                            })];
                            if copied {
                                content.push(text(
                                    "Windows would not draw this window for the agent, so this is \
                                 copied off the screen: whatever covers the window shows too.",
                                ));
                            }
                            let outcome = if copied {
                                "Took a picture, copied off the screen."
                            } else {
                                "Took a picture."
                            };
                            (content, outcome.to_string())
                        });
                report(app, &root, &what, result)
            }
            "desktop_click" => {
                let button = s("button").unwrap_or("left");
                if !matches!(button, "left" | "right" | "middle") {
                    return Err("button is left, right or middle".into());
                }
                let double = flag("double");
                let aim = self.aim(&w, args, false)?;
                let how = match (button, double) {
                    ("left", false) => "click".to_string(),
                    ("left", true) => "double click".to_string(),
                    (b, false) => format!("{b} click"),
                    (b, true) => format!("{b} double click"),
                };
                let what = format!("{how} {}", aim.what());
                let said = self.said(&w);
                let risky = flag("risky") || looks_risky_in(&w, &said, &[&aim.name]);
                let act = act_of(Kind::Click, &w, &said, aim.control, &aim.name, "");
                let point = aim_point(&w, &aim);
                allow_act(app, &root, &what, risky, &act, point, &reason)?;
                watch::follow(w.pid);
                let (before, mark, front) = (list_windows(), watch::mark(), front_before());
                let acted = self.click(&w, &aim, button, double);
                let back = after_action(front.as_ref(), &w, settle, acted.is_ok());
                let result = acted.map(|mut said| {
                    said.push_str(&came_of(&before, &w, mark));
                    said.push_str(&back);
                    (vec![text(said.clone())], said)
                });
                report(app, &root, &what, result)
            }
            "desktop_type" => {
                let typed = s("text").ok_or("say what to type: text")?;
                let submit = flag("submit");
                let aim = self.aim(&w, args, true)?;
                let mut what = format!("type {:?} into {}", short(typed, 60), aim.what());
                if submit {
                    what.push_str(" and press Enter");
                }
                // Enter in a dialog confirms it, as pressing it alone would,
                // but a path typed into a file dialog's name box only picks
                // the file.
                let said = self.said(&w);
                let picks_file = said.file_dialog && file_name_field(aim.control, &aim.name);
                let risky = flag("risky")
                    || (submit && enter_confirms(is_dialog(&w), picks_file))
                    || looks_risky_in(&w, &said, &[&aim.name]);
                let act = act_of(Kind::Type, &w, &said, aim.control, &aim.name, "");
                let point = aim_point(&w, &aim);
                allow_act(app, &root, &what, risky, &act, point, &reason)?;
                watch::follow(w.pid);
                let (before, mark, front) = (list_windows(), watch::mark(), front_before());
                let acted = self.type_in(&w, &aim, typed, submit);
                let back = after_action(front.as_ref(), &w, settle, acted.is_ok());
                let result = acted.map(|mut said| {
                    said.push_str(&came_of(&before, &w, mark));
                    said.push_str(&back);
                    (vec![text(said.clone())], said)
                });
                report(app, &root, &what, result)
            }
            "desktop_press" => {
                let key = s("key").ok_or("say which key")?;
                let chord = parse_key(key)?;
                if let Some(why) = press_refusal(&chord) {
                    return Err(why);
                }
                let what = format!("press {key} in {:?}", w.title);
                let (focused, at) = self.focused(&w);
                let said = self.said(&w);
                let risky = flag("risky")
                    || risky_key(&chord, enter_confirms(is_dialog(&w), said.file_dialog))
                    || looks_risky_in(&w, &said, &[&focused, key]);
                let act = act_of(Kind::Press, &w, &said, "", &focused, key);
                let point = at.or_else(|| window_point(&w));
                allow_act(app, &root, &what, risky, &act, point, &reason)?;
                watch::follow(w.pid);
                let (before, mark, front) = (list_windows(), watch::mark(), front_before());
                let pressed = bring_forward(&w).and_then(|()| send_strokes(&chord_input(&chord)));
                let back = after_action(front.as_ref(), &w, settle, pressed.is_ok());
                let result = pressed.map(|()| {
                    let mut said = format!("Pressed {key} in {:?}.", w.title);
                    said.push_str(&came_of(&before, &w, mark));
                    said.push_str(&back);
                    (vec![text(said.clone())], said)
                });
                report(app, &root, &what, result)
            }
            "desktop_text" => {
                let aim = if args.get("ref").is_some() || args.get("name").is_some() {
                    Some(self.aim(&w, args, false)?)
                } else {
                    None
                };
                let thing = aim
                    .as_ref()
                    .map_or_else(|| format!("{:?}", w.title), Aim::what);
                let what = format!("read the text of {thing}");
                allow(app, &root, "see", &what, false, None, &reason)?;
                watch::follow(w.pid);
                let read = self
                    .text_pattern(&w, aim.as_ref())
                    .map(|t| (t, "through UI Automation"))
                    .or_else(|| self.value_text(&w, aim.as_ref()));
                if let Some((t, how)) = read {
                    let said = text_answer(&t, how);
                    return report(
                        app,
                        &root,
                        &what,
                        Ok((vec![text(said)], "Read its text.".into())),
                    );
                }
                // Copying takes keys and the clipboard, so it is operating.
                let what = format!("select all and copy in {thing}, to read its text");
                let point = aim
                    .as_ref()
                    .map_or_else(|| window_point(&w), |a| aim_point(&w, a));
                allow(app, &root, "operate", &what, flag("risky"), point, &reason)?;
                let front = front_before();
                let copied = self.copy_text(&w, aim.as_ref());
                let back = after_action(front.as_ref(), &w, Duration::ZERO, false);
                let result = copied.map(|t| {
                    let mut said = text_answer(&t, "by selecting all and copying it");
                    said.push_str(&back);
                    (vec![text(said)], "Copied its text.".to_string())
                });
                report(app, &root, &what, result)
            }
            "desktop_changes" | "desktop_wait" => {
                let what = if name == "desktop_wait" {
                    format!("wait for a change in {:?}", w.title)
                } else {
                    format!("read what changed in {:?}", w.title)
                };
                allow(app, &root, "see", &what, false, None, &reason)?;
                watch::follow(w.pid);
                let answer = if name == "desktop_wait" {
                    self.wait(args, Some(&w))
                } else {
                    Ok(self.changes(args, Some(&w), app))
                };
                let result = answer.map(|mut said| {
                    if let Some(why) = walled {
                        said.push_str(&format!("\n(Note: {why}. {UNWATCHED})"));
                    }
                    let outcome = if name == "desktop_wait" {
                        "Waited for a change."
                    } else {
                        "Read what changed."
                    };
                    (vec![text(said)], outcome.to_string())
                });
                report(app, &root, &what, result)
            }
            _ => Err(format!("there is no tool {name}")),
        }
    }

    /// What changed since the last call, in `w`'s program or in every
    /// program watched, the helper's too.
    fn changes<A: FnMut(Value) -> Result<Value, String>>(
        &mut self,
        args: &Value,
        w: Option<&Window>,
        app: &mut A,
    ) -> String {
        let pid = w.map(|w| w.pid);
        let mut out = if pid.is_none() && !watch::any_followed() {
            NOTHING_WATCHED.to_string()
        } else {
            watch::flush();
            let asked = args
                .get("since")
                .and_then(Value::as_u64)
                .filter(|_| !self.part);
            let since = asked.unwrap_or_else(|| self.seen.get(&pid).copied().unwrap_or(0));
            let (list, lost, newest) = watch::since(since, pid);
            self.seen.insert(pid, newest);
            let scope = match w {
                Some(w) => format!(" in {:?}", w.title),
                None => " in the watched programs".to_string(),
            };
            if self.part {
                changes_part(&list, lost, &scope)
            } else {
                changes_text(&list, since, newest, lost, &scope)
            }
        };
        if pid.is_none() {
            if let Some(helper) = self.helper.as_mut() {
                // The agent's `since` counts in this ring; the helper keeps
                // its own place in its own.
                let mut theirs_args = args.clone();
                if let Some(o) = theirs_args.as_object_mut() {
                    o.remove("since");
                }
                let mut marked = |body: Value| app(as_administrator(body));
                let theirs =
                    helper.call("desktop_changes", &theirs_args, &mut marked, Duration::ZERO);
                match theirs {
                    Ok(Ok(content)) => {
                        let said = content
                            .first()
                            .and_then(|c| c["text"].as_str())
                            .unwrap_or_default()
                            .to_string();
                        out.push_str(&format!(
                            "\nIn programs that run as administrator, counted apart: {said}"
                        ));
                    }
                    Ok(Err(why)) => out.push_str(&format!(
                        "\nWhat changed in programs that run as administrator could not be \
                         read: {why}."
                    )),
                    Err(why) => {
                        self.lose_helper(&why);
                        out.push_str(&format!(
                            "\nWhat changed in programs that run as administrator could not be \
                             read: {why}. The helper is started again on the next call to one."
                        ));
                    }
                }
            }
        }
        out
    }

    /// The helper's side of an unnamed wait, see [`PEEK`]: without `after`
    /// it marks where the wait begins, with it it says what changed since
    /// and whether the text or quiet waited for holds here. With `commit`
    /// the changes count as told, as `desktop_changes` counts them.
    fn peek(&mut self, args: &Value) -> Value {
        watch::flush();
        let Some(after) = args["after"].as_u64() else {
            return json!({ "newest": watch::newest(), "started_ms": watch::now_ms() });
        };
        let started = args["started_ms"].as_u64().unwrap_or(0);
        let (seen, _, newest) = if args["commit"] == true {
            let read = watch::since(after, None);
            self.seen.insert(None, read.2);
            read
        } else {
            watch::peek(after, None)
        };
        let now = watch::now_ms();
        let text = Until {
            text: args["text"].as_str().map(String::from),
            ..Until::default()
        };
        let hit = text
            .text
            .is_some()
            .then(|| verdict(&text, &[], &seen, started, now))
            .flatten();
        let quiet = args["quiet_ms"].as_u64().map(|q| {
            let quiet = Until {
                quiet_ms: Some(q),
                ..Until::default()
            };
            verdict(&quiet, &[], &seen, started, now).is_some()
        });
        let skip = seen.len().saturating_sub(watch::MOST_LINES);
        let lines: Vec<String> = seen[skip..].iter().map(watch::line).collect();
        json!({ "newest": newest, "hit": hit, "quiet": quiet, "lines": lines })
    }

    /// Waits until what `args` asks for holds, in `w`'s program or in any
    /// watched, and says which held and what changed meanwhile.
    fn wait(&mut self, args: &Value, w: Option<&Window>) -> Result<String, String> {
        let s = |k: &str| {
            args.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(String::from)
        };
        let until = Until {
            appears: s("appears"),
            text: s("text"),
            gone: s("gone"),
            quiet_ms: args.get("quiet_ms").and_then(Value::as_u64),
        };
        if until.is_empty() {
            return Err("say what to wait for: appears, gone, text or quiet_ms".into());
        }
        let pid = w.map(|w| w.pid);
        let needs_watch = until.text.is_some() || until.quiet_ms.is_some();
        // Windows' titles reach this session from any program, but what
        // changes inside one that runs as administrator only the helper
        // sees, so an unnamed wait asks it too while it runs.
        let mut note = String::new();
        let mut theirs: Option<(Value, Peeked)> = None;
        if needs_watch && pid.is_none() && self.helper.is_some() {
            match self.ask_helper(PEEK, &json!({})) {
                Ok(content) => {
                    let mark = content.first().cloned().unwrap_or_default();
                    let ask = json!({
                        "after": mark["newest"],
                        "started_ms": mark["started_ms"],
                        "text": until.text,
                        "quiet_ms": until.quiet_ms,
                    });
                    // Not quiet there until the helper says so.
                    let unheard = Peeked {
                        quiet: until.quiet_ms.map(|_| false),
                        ..Peeked::default()
                    };
                    theirs = Some((ask, unheard));
                }
                Err(why) => note = helper_note(&why),
            }
        }
        if needs_watch && pid.is_none() && !watch::any_followed() && theirs.is_none() {
            return Err(format!(
                "{NOTHING_WATCHED} Name the window to wait in.{note}"
            ));
        }
        // Split, so quiet can be asked of both sides.
        let here = Until {
            quiet_ms: None,
            appears: until.appears.clone(),
            text: until.text.clone(),
            gone: until.gone.clone(),
        };
        let here_quiet = Until {
            quiet_ms: until.quiet_ms,
            ..Until::default()
        };
        let mut polled = Instant::now();
        let most = wait_limit(
            args.get("timeout_ms").and_then(Value::as_u64),
            std::env::var(WAIT_ENV).ok().as_deref(),
        );
        let titled = until.appears.is_some() || until.gone.is_some();
        watch::flush();
        let (mark, started, begun) = (watch::newest(), watch::now_ms(), Instant::now());
        let met = loop {
            let titles: Vec<String> = if titled {
                wait_titles(list_windows(), pid)
            } else {
                Vec::new()
            };
            // A look only: what the agent is told comes after the loop.
            let (seen, _, newest) = watch::peek(mark, pid);
            let now = watch::now_ms();
            if theirs.is_some() && polled.elapsed() >= HELPER_POLL {
                polled = Instant::now();
                let ask = theirs.as_ref().map(|(a, _)| a.clone()).unwrap_or_default();
                match self.ask_helper(PEEK, &ask) {
                    Ok(content) => {
                        if let (Some((_, p)), Some(c)) = (theirs.as_mut(), content.first()) {
                            *p = peeked(c);
                        }
                    }
                    Err(why) => {
                        note = helper_note(&why);
                        theirs = None;
                    }
                }
            }
            let holds = match &theirs {
                None => verdict(&until, &titles, &seen, started, now),
                Some((_, p)) => joined_verdict(
                    verdict(&here, &titles, &seen, started, now),
                    here_quiet
                        .quiet_ms
                        .and_then(|_| verdict(&here_quiet, &[], &seen, started, now)),
                    p,
                ),
            };
            if let Some(v) = holds {
                break Some(v);
            }
            let left = most.saturating_sub(begun.elapsed());
            if left.is_zero() {
                break None;
            }
            watch::wait_for_change(newest, left.min(Duration::from_millis(150)));
        };
        let (seen, _, newest) = watch::since(mark, pid);
        self.seen.insert(pid, newest);
        let took = begun.elapsed().as_secs_f32();
        let mut meanwhile = String::new();
        if seen.is_empty() {
            meanwhile.push_str(" Nothing changed meanwhile.");
        } else {
            meanwhile.push_str(" Meanwhile:");
            let skip = seen.len().saturating_sub(watch::MOST_LINES);
            for c in &seen[skip..] {
                meanwhile.push('\n');
                meanwhile.push_str(&watch::line(c));
            }
        }
        if let Some((mut ask, _)) = theirs {
            ask["commit"] = json!(true);
            match self.ask_helper(PEEK, &ask) {
                Ok(content) => {
                    let lines = content.first().map(peeked).unwrap_or_default().lines;
                    if !lines.is_empty() {
                        meanwhile.push_str(
                            "\nMeanwhile in programs that run as administrator, counted apart:",
                        );
                        for l in lines {
                            meanwhile.push('\n');
                            meanwhile.push_str(&l);
                        }
                    }
                }
                Err(why) => note = helper_note(&why),
            }
        }
        meanwhile.push_str(&note);
        match met {
            Some(v) => Ok(format!("Done after {took:.1} s: {v}.{meanwhile}")),
            // An error, so the agent does not take the silence for a yes
            // and wait again in the same blind way.
            None => Err(format!(
                "Timed out after {took:.1} s waiting for {}: it did not come. Do not wait \
                 again for the same thing: look first, with desktop_snapshot or \
                 desktop_screenshot, whether the last action landed where you meant.{meanwhile}",
                waited_for(&until)
            )),
        }
    }

    fn snapshot(&mut self, w: &Window) -> Result<String, String> {
        let com = self.com()?;
        let root = unsafe { com.uia.ElementFromHandleBuildCache(hwnd(w.hwnd), &com.tree) }
            .map_err(|e| format!("UI Automation could not read {:?}: {}", w.title, win_err(e)))?;
        let origin = window_rect(w.hwnd);
        let mut nodes = Vec::new();
        let mut elements = Vec::new();
        let mut more = false;
        walk(&root, 0, origin, &mut nodes, &mut elements, &mut more);
        let head = format!(
            "Window {:?} id={}, {} pid {}, {} x {} pixels. Places are x,y from the window's \
             top left corner, then width x height.",
            w.title,
            w.hwnd,
            w.exe,
            w.pid,
            origin.right - origin.left,
            origin.bottom - origin.top
        );
        self.snap = Some(Snap {
            hwnd: w.hwnd,
            elements,
        });
        Ok(tree_text(&head, &nodes, more, crate::mcp::MOST_TEXT))
    }

    fn by_ref(&mut self, w: &Window, r: &str) -> Result<Aim, String> {
        let n = ref_number(r).ok_or(format!("{r:?} is not a ref; refs look like e12"))?;
        let el = match &self.snap {
            Some(s) if s.hwnd != w.hwnd => {
                return Err(format!(
                    "{r} is from a snapshot of another window; take a desktop_snapshot of this one"
                ))
            }
            Some(s) => s.elements.get(n).cloned(),
            None => None,
        }
        .ok_or(format!(
            "there is no {r} in the last desktop_snapshot of this window; take a new one"
        ))?;
        let com = self.com()?;
        let fresh = unsafe { el.BuildUpdatedCache(&com.one) }
            .map_err(|_| format!("{r} is gone from the window; take a new desktop_snapshot"))?;
        Ok(aim_of(fresh))
    }

    /// The element `args` names by `ref`, or by `name` from a fresh search,
    /// one that takes text first when `typing`.
    fn aim(&mut self, w: &Window, args: &Value, typing: bool) -> Result<Aim, String> {
        if let Some(r) = args.get("ref").and_then(Value::as_str) {
            return self.by_ref(w, r);
        }
        let wanted = args
            .get("name")
            .and_then(Value::as_str)
            .ok_or("say which element: a ref from desktop_snapshot, or its name")?;
        let found = self.all(w)?;
        let names: Vec<String> = found.iter().map(|a| a.name.clone()).collect();
        let apt: Vec<bool> = found
            .iter()
            .map(|a| {
                if typing {
                    a.writable || matches!(a.control, "Edit" | "Document" | "ComboBox")
                } else {
                    a.invoke || a.toggle || a.select || a.expand
                }
            })
            .collect();
        let ids: Vec<String> = found.iter().map(|a| a.automation_id.clone()).collect();
        let i = pick_name(&names, &ids, &apt, wanted).ok_or(format!(
            "nothing in {:?} has a name or automation id like {wanted:?}; take a desktop_snapshot",
            w.title
        ))?;
        Ok(found.into_iter().nth(i).expect("picked from these"))
    }

    /// Every element in `w`, each with what an action needs to know.
    fn all(&mut self, w: &Window) -> Result<Vec<Aim>, String> {
        let com = self.com()?;
        unsafe {
            let root = com.uia.ElementFromHandle(hwnd(w.hwnd)).map_err(|e| {
                format!("UI Automation could not read {:?}: {}", w.title, win_err(e))
            })?;
            let every = com.uia.CreateTrueCondition().map_err(win_err)?;
            let found = root
                .FindAllBuildCache(TreeScope_Descendants, &every, &com.one)
                .map_err(win_err)?;
            let n = found.Length().unwrap_or(0);
            Ok((0..n)
                .filter_map(|i| found.GetElement(i).ok())
                .map(aim_of)
                .collect())
        }
    }

    /// What `w` says, when it is a dialog: a small one's text, and whether
    /// it is a standard file dialog. A window that is not a dialog says
    /// nothing here; its title speaks for it.
    fn said(&mut self, w: &Window) -> Said {
        if !is_dialog(w) {
            return Said::default();
        }
        let Ok(all) = self.all(w) else {
            return Said::default();
        };
        let file_dialog = all.iter().any(|a| file_name_field(a.control, &a.name));
        let texts = if all.len() < SMALL_DIALOG {
            all.into_iter()
                .filter(|a| a.control == "Text" && !a.name.trim().is_empty())
                .map(|a| a.name)
                .collect()
        } else {
            Vec::new()
        };
        Said { texts, file_dialog }
    }

    /// The name of what has the keyboard, when it is in `w`, and its
    /// middle on the screen, where a key pressed now lands.
    fn focused(&mut self, w: &Window) -> (String, Option<(i32, i32)>) {
        let Ok(com) = self.com() else {
            return (String::new(), None);
        };
        let el = unsafe { com.uia.GetFocusedElementBuildCache(&com.one) }
            .ok()
            // The process id is not among the cached properties, so it is
            // read live; a cached read of it always fails.
            .filter(|el| unsafe { el.CurrentProcessId() }.ok() == Some(w.pid as i32));
        let Some(el) = el else {
            return (String::new(), None);
        };
        unsafe {
            let name = el.CachedName().map(|b| b.to_string()).unwrap_or_default();
            let at = el
                .CachedBoundingRectangle()
                .ok()
                .and_then(|r| centre(r.left, r.top, r.right, r.bottom));
            (name, at)
        }
    }

    fn click(
        &mut self,
        w: &Window,
        aim: &Aim,
        button: &str,
        double: bool,
    ) -> Result<String, String> {
        let thing = aim.what();
        let plain = button == "left" && !double;
        let mut rect = aim.rect;
        let only_expands = aim.expand && !aim.invoke && !aim.toggle && !aim.select;
        if click_way(plain, only_expands, has_place(rect)) == ClickWay::Nowhere {
            // Brought into view where the program can, so the click still
            // lands on the element and not on a guess.
            if let Some(r) = scroll_into_view(&aim.el) {
                rect = r;
            }
        }
        let point = match click_way(plain, only_expands, has_place(rect)) {
            ClickWay::Expand => {
                let expanded = aim.expanded;
                let verb = if expanded { "collapsed" } else { "expanded" };
                let done = on_the_side(&aim.el, move |el| unsafe {
                    let p = el.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                        UIA_ExpandCollapsePatternId,
                    )?;
                    if expanded {
                        p.Collapse()
                    } else {
                        p.Expand()
                    }
                });
                return match done {
                    Ok(true) => Ok(format!("Clicked {thing}: {verb} it through UI Automation.")),
                    Ok(false) => Ok(format!(
                        "Clicked {thing}: {verb} it through UI Automation, and the program has \
                         not finished with it after {} seconds.",
                        PATTERN_WAIT.as_secs()
                    )),
                    // A pattern that fails may have done half its work, so
                    // it is not followed by a click that could do it again.
                    Err(e) => Err(format!(
                        "{thing} was not {verb}: UI Automation said {e}; nothing else was tried"
                    )),
                };
            }
            ClickWay::Nowhere => {
                return Err(format!(
                    "{thing} has no place on screen (scrolled out of view or hidden) and could \
                     not be scrolled into view, so nothing was clicked; scroll to it first"
                ))
            }
            // The ghost's point, so the click goes where the human saw it
            // aimed.
            ClickWay::Real => centre(rect.left, rect.top, rect.right, rect.bottom),
        };
        let Some((x, y)) = point else {
            return Err(format!(
                "{thing} has no place on screen, so nothing was clicked"
            ));
        };
        bring_forward(w)?;
        let desk = unsafe {
            [
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            ]
        };
        // The click lands on whatever is on top at that point, which must
        // be the approved program and not something covering it. Horadric's
        // overlay lets the mouse through, so Windows does not name it here.
        unsafe {
            let under = WindowFromPoint(POINT { x, y });
            let top = GetAncestor(under, GA_ROOT);
            let mut pid = 0;
            GetWindowThreadProcessId(top, Some(&mut pid));
            if covered(!under.is_invalid(), pid, w.pid) {
                let c = info(top.0 as isize);
                return Err(format!(
                    "{thing} is covered there by {:?} ({}), so a click would land on that; \
                     nothing was clicked",
                    c.title, c.exe
                ));
            }
        }
        // Taken now: a dialog the click closes has no place after it.
        let o = window_rect(w.hwnd);
        let (ax, ay) = absolute(x, y, desk);
        let at = MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK | MOUSEEVENTF_MOVE;
        let (down, up) = match button {
            "right" => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
            "middle" => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
            _ => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
        };
        let mut inputs = vec![mouse(ax, ay, at)];
        for _ in 0..if double { 2 } else { 1 } {
            inputs.push(mouse(ax, ay, at | down));
            inputs.push(mouse(ax, ay, at | up));
        }
        let mut was = POINT::default();
        let had = unsafe { GetCursorPos(&mut was) }.is_ok();
        let sent = send(&inputs);
        thread::sleep(Duration::from_millis(60));
        if had {
            let _ = unsafe { SetCursorPos(was.x, was.y) };
        }
        if !sent {
            return Err(BLOCKED.into());
        }
        Ok(format!(
            "Clicked {thing} with a real {}{} click at {},{} in the window.",
            if double { "double " } else { "" },
            button,
            x - o.left,
            y - o.top
        ))
    }

    fn type_in(
        &mut self,
        w: &Window,
        aim: &Aim,
        typed: &str,
        submit: bool,
    ) -> Result<String, String> {
        let thing = aim.what();
        let mut said =
            if aim.writable {
                // A Windows field breaks lines on CR LF.
                let value = typed.replace("\r\n", "\n").replace('\n', "\r\n");
                let set = on_the_side(&aim.el, move |el| unsafe {
                    el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)?
                        .SetValue(&BSTR::from(value))
                });
                // A value that was sent but not confirmed may still land, so it
                // is never typed a second time, nor followed by Enter.
                match set {
                    Ok(true) => format!("Set {thing} to {:?}", short(typed, 60)),
                    Ok(false) => {
                        return Ok(format!(
                        "Sent {thing} the value {:?} through UI Automation, and the program has \
                         not confirmed it after {} seconds (a dialog may have opened){}.",
                        short(typed, 60),
                        PATTERN_WAIT.as_secs(),
                        if submit { "; Enter was not pressed" } else { "" }
                    ))
                    }
                    Err(e) => {
                        return Err(format!(
                        "{thing} did not take the value: UI Automation said {e}; nothing was typed"
                    ))
                    }
                }
            } else {
                self.focus(w, aim)?;
                send_strokes(&text_strokes(typed))?;
                // The program takes typed keys in its own time.
                thread::sleep(Duration::from_millis(120));
                format!(
                    "Typed {} characters into {thing} at its caret",
                    typed.chars().count()
                )
            };
        // Read back before Enter, which may close the field's dialog, so
        // the agent knows whether the text is where it meant.
        let held = landed(typed, value_now(&aim.el));
        if submit && matches!(held, Landed::Elsewhere(_)) {
            return Err(format!(
                "{said}.{} So Enter was not pressed. Find where the keyboard is with \
                 desktop_snapshot before typing again.",
                landed_text(&held, false)
            ));
        }
        if submit {
            if aim.writable {
                self.focus(w, aim)
                    .map_err(|e| format!("{said}, but Enter was not pressed: {e}"))?;
            }
            send_strokes(&chord_strokes(&[], VK_RETURN))?;
            said.push_str(" and pressed Enter");
        }
        said.push('.');
        said.push_str(&landed_text(&held, submit));
        Ok(said)
    }

    /// Puts the keyboard in `aim`, its window in front, or says why not.
    fn focus(&mut self, w: &Window, aim: &Aim) -> Result<(), String> {
        bring_forward(w)?;
        unsafe { aim.el.SetFocus() }.map_err(|e| {
            format!(
                "{} would not take the keyboard ({}), so nothing was typed",
                aim.what(),
                win_err(e)
            )
        })
    }

    /// The text of `aim`, or of the window, through UI Automation's text
    /// pattern, which editors and many output panes have. None when it has
    /// none or it is empty.
    fn text_pattern(&mut self, w: &Window, aim: Option<&Aim>) -> Option<String> {
        let el = match aim {
            Some(a) => a.el.clone(),
            None => unsafe { self.com().ok()?.uia.ElementFromHandle(hwnd(w.hwnd)) }.ok()?,
        };
        read_on_the_side(&el, |el| unsafe {
            let p = el.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)?;
            Ok(p.DocumentRange()?.GetText(-1)?.to_string())
        })
        .filter(|t| !t.trim().is_empty())
    }

    /// The text of `aim`, or of what has the keyboard in `w`, from the
    /// value it reports, and how it was read: UI Automation's value, then
    /// the older accessibility value, then for an edit field the text
    /// Windows keeps in its window. A plain WinForms TextBox has no text
    /// pattern and ignores Control+A, but answers these.
    fn value_text(&mut self, w: &Window, aim: Option<&Aim>) -> Option<(String, &'static str)> {
        let (el, control) = match aim {
            Some(a) => (a.el.clone(), a.control),
            None => {
                let com = self.com().ok()?;
                let el = unsafe { com.uia.GetFocusedElementBuildCache(&com.one) }
                    .ok()
                    .filter(|el| unsafe { el.CurrentProcessId() }.ok() == Some(w.pid as i32))?;
                let control = control_name(unsafe { el.CachedControlType() }.map_or(0, |c| c.0));
                (el, control)
            }
        };
        let native = unsafe { el.CurrentNativeWindowHandle() }
            .ok()
            .filter(|h| !h.is_invalid());
        let class = native.map(class_of).unwrap_or_default();
        let edit = is_edit(control, &class);
        if let Some(v) = value_now(&el).filter(|v| read_counts(v, edit)) {
            return Some((v, "from its value through UI Automation"));
        }
        let legacy = read_on_the_side(&el, |el| unsafe {
            let p = el.GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
                UIA_LegacyIAccessiblePatternId,
            )?;
            Ok(p.CurrentValue()?.to_string())
        });
        if let Some(v) = legacy.filter(|v| read_counts(v, edit)) {
            return Some((v, "from its accessibility value"));
        }
        native
            .filter(|_| edit)
            .and_then(window_text)
            .map(|v| (v, "from the field's window text"))
    }

    /// The text of `aim`, or of what has the keyboard in `w`, by selecting
    /// all and copying it, for lists, grids and panes that UI Automation
    /// can not read. The clipboard's text is put back after.
    fn copy_text(&mut self, w: &Window, aim: Option<&Aim>) -> Result<String, String> {
        match aim {
            // An output pane may not take the keyboard through UI
            // Automation, but does from a click into it.
            Some(a) => {
                if self.focus(w, a).is_err() {
                    self.click(w, a, "left", false)?;
                }
            }
            None => bring_forward(w)?,
        }
        let kept = clip_text();
        let before = unsafe { GetClipboardSequenceNumber() };
        send_strokes(&chord_strokes(&[VK_CONTROL], b'A' as u16))?;
        thread::sleep(Duration::from_millis(80));
        send_strokes(&chord_strokes(&[VK_CONTROL], b'C' as u16))?;
        let begun = Instant::now();
        while unsafe { GetClipboardSequenceNumber() } == before && begun.elapsed() < COPY_WAIT {
            thread::sleep(Duration::from_millis(30));
        }
        if unsafe { GetClipboardSequenceNumber() } == before {
            return Err(format!(
                "nothing was copied from {}: it may not take Control+A and Control+C; \
                 desktop_screenshot of it is what is left",
                aim.map_or_else(|| format!("{:?}", w.title), Aim::what)
            ));
        }
        let copied = clip_text();
        // The read leaves the clipboard as it found it, or empty when it
        // held no text, rather than holding what was copied.
        match kept {
            Some(t) => set_clip_text(&t),
            None => clear_clip(),
        }
        copied
            .filter(|t| !t.trim().is_empty())
            .ok_or_else(|| "the copy held no text".to_string())
    }

    /// The window as a PNG, cut to `crop` (on screen) when given, and
    /// whether it was copied off the screen.
    fn picture(&mut self, w: &Window, crop: Option<RECT>) -> Result<(Vec<u8>, bool), String> {
        self.com()?;
        let h = hwnd(w.hwnd);
        if unsafe { IsIconic(h) }.as_bool() {
            return Err(format!(
                "{:?} is minimized, so there is nothing to see; desktop_snapshot still reads it",
                w.title
            ));
        }
        let o = window_rect(w.hwnd);
        let (width, height) = (o.right - o.left, o.bottom - o.top);
        if width <= 0 || height <= 0 {
            return Err("the window has no size".into());
        }
        let (pixels, copied) = unsafe { print(h, o) }?;
        let part = match crop {
            Some(r) => Some(
                clip(
                    [
                        r.left - o.left,
                        r.top - o.top,
                        r.right - r.left,
                        r.bottom - r.top,
                    ],
                    width,
                    height,
                )
                .ok_or("that element is outside the window's picture")?,
            ),
            None => None,
        };
        let png = unsafe { encode(&pixels, width as u32, height as u32, part) }.map_err(win_err)?;
        Ok((png, copied))
    }
}

/// What came of an action on `w`, told by the watcher from change `mark`
/// on, or by the windows that are new since `before` when it has nothing.
/// Whether acting on `names` (an element, a key) in `w` looks risky: by
/// them, the title, and for a small dialog, what its text says.
fn looks_risky_in(w: &Window, said: &Said, names: &[&str]) -> bool {
    let mut texts: Vec<&str> = names.to_vec();
    texts.push(&w.title);
    texts.extend(said.texts.iter().map(String::as_str));
    looks_risky(&texts)
}

/// An operate action in parts, for the patterns the human pre-approved.
fn act_of(kind: Kind, w: &Window, said: &Said, control: &str, name: &str, key: &str) -> Act {
    Act {
        kind: Some(kind),
        window: w.title.clone(),
        control: control.to_string(),
        name: name.to_string(),
        key: key.to_string(),
        texts: said.texts.clone(),
    }
}

fn came_of(before: &[Window], w: &Window, mark: u64) -> String {
    if watch::following(w.pid) {
        watch::flush();
        let (list, _, _) = watch::since(mark, Some(w.pid));
        let said = digest(&list, 2);
        if !said.is_empty() {
            return said;
        }
    }
    opened(before, w)
}

/// What a new window of the same program, a dialog most likely, says it
/// is, after an action.
fn opened(before: &[Window], w: &Window) -> String {
    let new: Vec<String> = list_windows()
        .into_iter()
        .filter(|n| n.pid == w.pid && !before.iter().any(|b| b.hwnd == n.hwnd))
        .map(|n| format!("{:?} id={}", n.title, n.hwnd))
        .collect();
    if new.is_empty() {
        String::new()
    } else {
        format!(" A window opened: {}.", new.join(", "))
    }
}

/// The middle of `w` on the screen, none when it is minimized and so
/// parked far off it.
fn window_point(w: &Window) -> Option<(i32, i32)> {
    if unsafe { IsIconic(hwnd(w.hwnd)) }.as_bool() {
        return None;
    }
    let r = window_rect(w.hwnd);
    centre(r.left, r.top, r.right, r.bottom)
}

/// Where an action on `aim` lands: its middle, or the window's when it
/// has no place on screen.
fn aim_point(w: &Window, aim: &Aim) -> Option<(i32, i32)> {
    let r = aim.rect;
    centre(r.left, r.top, r.right, r.bottom).or_else(|| window_point(w))
}

fn aim_of(el: IUIAutomationElement) -> Aim {
    unsafe {
        let control = control_name(el.CachedControlType().map(|c| c.0).unwrap_or(0));
        let name = el.CachedName().map(|b| b.to_string()).unwrap_or_default();
        let automation_id = el
            .CachedAutomationId()
            .map(|b| b.to_string())
            .unwrap_or_default();
        let rect = el.CachedBoundingRectangle().unwrap_or_default();
        let value = cached_bool(&el, UIA_IsValuePatternAvailablePropertyId);
        Aim {
            control,
            name,
            automation_id,
            rect,
            invoke: cached_bool(&el, UIA_IsInvokePatternAvailablePropertyId),
            toggle: cached_bool(&el, UIA_IsTogglePatternAvailablePropertyId),
            select: cached_bool(&el, UIA_IsSelectionItemPatternAvailablePropertyId),
            expand: cached_bool(&el, UIA_IsExpandCollapsePatternAvailablePropertyId),
            expanded: cached_i32(&el, UIA_ExpandCollapseExpandCollapseStatePropertyId)
                .is_some_and(|s| s == 1 || s == 2),
            writable: value && !cached_bool(&el, UIA_ValueIsReadOnlyPropertyId),
            el,
        }
    }
}

fn node_of(el: &IUIAutomationElement, depth: usize, origin: RECT) -> Node {
    unsafe {
        let r = el.CachedBoundingRectangle().unwrap_or_default();
        let mut actions = Vec::new();
        let has = |p| cached_bool(el, p);
        if has(UIA_IsInvokePatternAvailablePropertyId) {
            actions.push("invoke");
        }
        let toggle = has(UIA_IsTogglePatternAvailablePropertyId);
        if toggle {
            actions.push("toggle");
        }
        let select = has(UIA_IsSelectionItemPatternAvailablePropertyId);
        if select {
            actions.push("select");
        }
        let expand = has(UIA_IsExpandCollapsePatternAvailablePropertyId);
        if expand {
            actions.push("expand");
        }
        let value = has(UIA_IsValuePatternAvailablePropertyId);
        if value && !has(UIA_ValueIsReadOnlyPropertyId) {
            actions.push("value");
        }
        let mut state = Vec::new();
        if toggle {
            match cached_i32(el, UIA_ToggleToggleStatePropertyId) {
                Some(1) => state.push("checked"),
                Some(2) => state.push("mixed"),
                _ => state.push("unchecked"),
            }
        }
        if expand {
            match cached_i32(el, UIA_ExpandCollapseExpandCollapseStatePropertyId) {
                Some(0) => state.push("collapsed"),
                Some(1 | 2) => state.push("expanded"),
                _ => {}
            }
        }
        if select && has(UIA_SelectionItemIsSelectedPropertyId) {
            state.push("selected");
        }
        if has(UIA_HasKeyboardFocusPropertyId) {
            state.push("focused");
        }
        if has(UIA_IsOffscreenPropertyId) {
            state.push("offscreen");
        }
        Node {
            depth,
            control: control_name(el.CachedControlType().map(|c| c.0).unwrap_or(0)),
            name: el.CachedName().map(|b| b.to_string()).unwrap_or_default(),
            value: if value {
                cached_string(el, UIA_ValueValuePropertyId)
            } else {
                None
            },
            automation_id: el
                .CachedAutomationId()
                .map(|b| b.to_string())
                .unwrap_or_default(),
            enabled: el.CachedIsEnabled().map(|b| b.as_bool()).unwrap_or(true),
            rect: [
                r.left - origin.left,
                r.top - origin.top,
                r.right - r.left,
                r.bottom - r.top,
            ],
            actions,
            state,
        }
    }
}

/// The cached children of `el` into `nodes`, each kept in `elements` at
/// its ref.
fn walk(
    el: &IUIAutomationElement,
    depth: usize,
    origin: RECT,
    nodes: &mut Vec<Node>,
    elements: &mut Vec<IUIAutomationElement>,
    more: &mut bool,
) {
    let Ok(kids) = (unsafe { el.GetCachedChildren() }) else {
        return;
    };
    let n = unsafe { kids.Length() }.unwrap_or(0);
    for i in 0..n {
        if nodes.len() >= MOST_ELEMENTS {
            *more = true;
            return;
        }
        let Ok(kid) = (unsafe { kids.GetElement(i) }) else {
            continue;
        };
        nodes.push(node_of(&kid, depth, origin));
        elements.push(kid.clone());
        walk(&kid, depth + 1, origin, nodes, elements, more);
    }
}

/// The window's pixels, as Windows draws it even when covered: 32 bit BGR
/// rows, top first. A window Windows will not draw for this process (one
/// run as administrator) is copied off the screen at `at` instead, which
/// the second value says.
unsafe fn print(h: HWND, at: RECT) -> Result<(Vec<u8>, bool), String> {
    let (width, height) = (at.right - at.left, at.bottom - at.top);
    let screen = GetDC(None);
    let dc = CreateCompatibleDC(Some(screen));
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let result = match CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
        Ok(dib) => {
            let old = SelectObject(dc, dib.into());
            // 2 is PW_RENDERFULLCONTENT, which draws what DirectX and
            // composition put in the window too; the crate has no name for it.
            let drawn = PrintWindow(h, dc, PRINT_WINDOW_FLAGS(2)).as_bool();
            let copied = !drawn
                && BitBlt(
                    dc,
                    0,
                    0,
                    width,
                    height,
                    Some(screen),
                    at.left,
                    at.top,
                    SRCCOPY,
                )
                .is_ok();
            let len = width as usize * height as usize * 4;
            let pixels = std::slice::from_raw_parts(bits as *const u8, len).to_vec();
            SelectObject(dc, old);
            let _ = DeleteObject(dib.into());
            if drawn || copied {
                Ok((pixels, copied))
            } else {
                Err("Windows could not draw the window".to_string())
            }
        }
        Err(e) => Err(win_err(e)),
    };
    let _ = DeleteDC(dc);
    ReleaseDC(None, screen);
    result
}

/// `pixels` as a PNG, cut to `part` and no wider than [`WIDEST`].
unsafe fn encode(
    pixels: &[u8],
    width: u32,
    height: u32,
    part: Option<[i32; 4]>,
) -> windows::core::Result<Vec<u8>> {
    let wic: IWICImagingFactory =
        CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
    let bitmap = wic.CreateBitmapFromMemory(
        width,
        height,
        &GUID_WICPixelFormat32bppBGR,
        width * 4,
        pixels,
    )?;
    let mut source: IWICBitmapSource = bitmap.cast()?;
    let (mut w, mut h) = (width, height);
    if let Some([x, y, cw, ch]) = part {
        let clipper = wic.CreateBitmapClipper()?;
        let r = WICRect {
            X: x,
            Y: y,
            Width: cw,
            Height: ch,
        };
        clipper.Initialize(&source, &r)?;
        source = clipper.cast()?;
        (w, h) = (cw as u32, ch as u32);
    }
    let (fw, fh) = fit(w, h, WIDEST);
    if (fw, fh) != (w, h) {
        let scaler = wic.CreateBitmapScaler()?;
        scaler.Initialize(&source, fw, fh, WICBitmapInterpolationModeFant)?;
        source = scaler.cast()?;
    }
    let stream = SHCreateMemStream(None).ok_or_else(|| windows::core::Error::from(HRESULT(-1)))?;
    let encoder = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
    encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
    let mut frame = None;
    encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
    let frame = frame.ok_or_else(|| windows::core::Error::from(HRESULT(-1)))?;
    frame.Initialize(None)?;
    frame.WriteSource(&source, std::ptr::null())?;
    frame.Commit()?;
    encoder.Commit()?;
    let mut size = 0u64;
    stream.Seek(0, STREAM_SEEK_END, Some(&mut size))?;
    stream.Seek(0, STREAM_SEEK_SET, None)?;
    let mut out = vec![0u8; size as usize];
    let mut read = 0u32;
    stream
        .Read(out.as_mut_ptr() as *mut _, size as u32, Some(&mut read))
        .ok()?;
    out.truncate(read as usize);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(hwnd: isize, title: &str, exe: &str) -> Window {
        Window {
            hwnd,
            title: title.into(),
            exe: exe.into(),
            path: format!(r"C:\Program Files\{exe}"),
            pid: 7,
            owner: None,
            class: String::new(),
        }
    }

    #[test]
    fn the_keyboard_goes_back_only_to_the_human_s_own_window() {
        let stage = Some((10, 1, "HoradricTerminal"));
        // The action left the program in front: the keyboard goes back.
        assert!(give_back(stage, true, 7, Some((20, 7))));
        assert!(give_back(stage, true, 7, None));
        // A dialog of the program took the front: back all the same.
        assert!(give_back(stage, true, 7, Some((21, 7))));
        // Already back, or the human went elsewhere meanwhile.
        assert!(!give_back(stage, true, 7, Some((10, 1))));
        assert!(!give_back(stage, true, 7, Some((30, 4))));
        // The human was in the program, or nothing was in front.
        assert!(!give_back(Some((20, 7, "Notepad")), true, 7, Some((20, 7))));
        assert!(!give_back(None, true, 7, Some((20, 7))));
        assert!(!give_back(Some((0, 1, "")), true, 7, Some((20, 7))));
        // Gone, or an overlay that never takes the keyboard.
        assert!(!give_back(stage, false, 7, Some((20, 7))));
        assert!(!give_back(
            Some((10, 1, "HoradricGhost")),
            true,
            7,
            Some((20, 7))
        ));
    }

    #[test]
    fn the_answer_says_where_the_focus_went() {
        assert_eq!(
            given_back_text("Horadric", true),
            " Focus was given back to \"Horadric\"."
        );
        assert!(given_back_text("Horadric", false).contains("keeps the keyboard"));
    }

    #[test]
    fn edit_fields_are_told_by_control_or_class() {
        assert!(is_edit("Edit", ""));
        assert!(is_edit("Pane", "WindowsForms10.EDIT.app.0.141b42a_r9_ad1"));
        assert!(is_edit("Document", "RichEdit20W"));
        assert!(!is_edit("List", "SysListView32"));
    }

    #[test]
    fn an_empty_value_answers_only_for_an_edit_field() {
        assert!(read_counts("", true));
        assert!(read_counts("42", false));
        assert!(!read_counts("  ", false));
    }

    #[test]
    fn a_wait_in_a_program_looks_only_at_its_windows() {
        let mut other = win(2, "Synchronization", "SysmacStudio.exe");
        other.pid = 9;
        let all = vec![win(1, "Ghost target", "python.exe"), other];
        assert_eq!(wait_titles(all.clone(), Some(7)), vec!["Ghost target"]);
        assert_eq!(
            wait_titles(all.clone(), None),
            vec!["Ghost target", "Synchronization"]
        );
        assert!(wait_titles(all, Some(3)).is_empty());
    }

    #[test]
    fn a_wait_is_short_unless_asked_or_set_longer() {
        assert_eq!(wait_limit(None, None), Duration::from_secs(10));
        assert_eq!(wait_limit(None, Some(" 25000 ")), Duration::from_secs(25));
        assert_eq!(wait_limit(None, Some("soon")), Duration::from_secs(10));
        assert_eq!(
            wait_limit(Some(3000), Some("25000")),
            Duration::from_secs(3)
        );
        assert_eq!(wait_limit(Some(10), None), Duration::from_millis(100));
        assert_eq!(wait_limit(Some(600_000), None), Duration::from_secs(60));
    }

    #[test]
    fn a_wait_says_what_it_waited_for() {
        let u = Until {
            appears: Some("Open".into()),
            quiet_ms: Some(800),
            ..Until::default()
        };
        assert_eq!(
            waited_for(&u),
            "a window titled like \"Open\" or 800 ms of quiet"
        );
    }

    #[test]
    fn typed_text_is_checked_against_the_field() {
        let path = r"C:\Proj\PLC.smc2";
        assert_eq!(landed(path, Some(path.into())), Landed::Holds(path.into()));
        assert_eq!(
            landed("10.0.0.5", Some("IP: 10.0.0.5".into())),
            Landed::Holds("IP: 10.0.0.5".into())
        );
        assert_eq!(
            landed("a\nb", Some("A\r\nB".into())),
            Landed::Holds("A\r\nB".into())
        );
        assert_eq!(landed(path, Some("".into())), Landed::Elsewhere("".into()));
        assert_eq!(landed(path, None), Landed::Unknown);
        assert!(landed_text(&Landed::Holds("x".into()), true).contains("when Enter"));
        assert!(landed_text(&Landed::Elsewhere("".into()), false).contains("not what was typed"));
    }

    #[test]
    fn read_text_is_cut_and_says_how_much() {
        assert_eq!(
            text_answer("a\r\nb", "by copying"),
            "Read by copying, 3 characters:\na\nb"
        );
        let long = "x".repeat(TEXT_MOST + 5);
        let said = text_answer(&long, "by copying");
        assert!(said.starts_with(&format!(
            "Read by copying, {} characters, the first",
            TEXT_MOST + 5
        )));
        assert_eq!(said.chars().filter(|&c| c == 'x').count(), TEXT_MOST);
    }

    #[test]
    fn risky_words_are_found_in_any_case_and_language() {
        assert!(looks_risky(&["Transfer to Controller"]));
        assert!(looks_risky(&["OK", "Change to RUN mode?"]));
        assert!(looks_risky(&["Synchronization"]));
        assert!(looks_risky(&["Overfør til PLC"]));
        assert!(looks_risky(&["SLET"]));
        assert!(looks_risky(&["", "Set/Reset"]));
        assert!(!looks_risky(&["OK", "Cancel", "Watch Tab 1"]));
        assert!(!looks_risky(&[]));
    }

    #[test]
    fn the_allow_body_names_the_program_the_action_and_where_it_lands() {
        let w = win(66, "Proj - Sysmac Studio", "SysmacStudio.exe");
        assert_eq!(
            allow_body(
                &w,
                "operate",
                "click Button \"Transfer\"",
                true,
                Some((-1200, 340)),
                "Send the fix to the PLC"
            ),
            json!({
                "op": "desktop.allow",
                "window": {
                    "hwnd": 66,
                    "title": "Proj - Sysmac Studio",
                    "exe": "SysmacStudio.exe",
                    "path": r"C:\Program Files\SysmacStudio.exe",
                    "pid": 7,
                },
                "action": "operate",
                "what": "click Button \"Transfer\"",
                "risky": true,
                "point": { "x": -1200, "y": 340 },
                "reason": "Send the fix to the PLC",
            })
        );
        let seeing = allow_body(&w, "see", "look", false, None, "");
        assert_eq!(seeing["point"], Value::Null);
        assert_eq!(seeing["reason"], "");
    }

    #[test]
    fn a_file_dialog_s_name_box_is_told_by_its_name() {
        assert!(file_name_field("ComboBox", "File name:"));
        assert!(file_name_field("Edit", "File name:"));
        assert!(file_name_field("Edit", "File &name:"));
        assert!(file_name_field("Edit", " Filnavn: "));
        assert!(!file_name_field("Text", "File name:"));
        assert!(!file_name_field("Button", "File name:"));
        assert!(!file_name_field("Edit", "File name filter"));
        assert!(!file_name_field("Edit", "Controller name:"));
    }

    #[test]
    fn enter_confirms_a_dialog_but_not_a_file_dialog() {
        assert!(enter_confirms(true, false));
        assert!(!enter_confirms(true, true));
        assert!(!enter_confirms(false, false));
        // A file dialog still confirms nothing by Enter, but Delete in its
        // file list is risky all the same.
        let k = |s: &str| parse_key(s).unwrap();
        assert!(!risky_key(&k("Enter"), enter_confirms(true, true)));
        assert!(risky_key(&k("Enter"), enter_confirms(true, false)));
        assert!(risky_key(&k("Delete"), enter_confirms(true, true)));
    }

    #[test]
    fn typing_a_path_into_a_file_dialog_is_not_risky_by_itself() {
        let save = win(5, "Export", "SysmacStudio.exe");
        let said = Said {
            texts: Vec::new(),
            file_dialog: true,
        };
        assert!(!looks_risky_in(&save, &said, &["File name:"]));
        let picks_file = said.file_dialog && file_name_field("ComboBox", "File name:");
        assert!(!enter_confirms(true, picks_file));
        // In any other field of it, Enter confirms as in any dialog.
        let other = said.file_dialog && file_name_field("Edit", "Search");
        assert!(enter_confirms(true, other));
        // A dialog that asks about a transfer is risky whatever is clicked.
        let ask = win(6, "Sysmac Studio", "SysmacStudio.exe");
        let said = Said {
            texts: vec!["Transfer to the Controller?".into()],
            file_dialog: false,
        };
        assert!(looks_risky_in(&ask, &said, &["Yes"]));
    }

    #[test]
    fn an_act_is_sent_in_parts_beside_the_sentence() {
        let w = win(5, "Import solution", "SysmacStudio.exe");
        let said = Said {
            texts: vec!["Replace?".into()],
            file_dialog: true,
        };
        let act = act_of(Kind::Press, &w, &said, "", "File name:", "Enter");
        assert_eq!(
            act_json(&act),
            json!({
                "kind": "press",
                "window": "Import solution",
                "control": "",
                "name": "File name:",
                "key": "Enter",
                "texts": ["Replace?"],
            })
        );
        let mut sent = Value::Null;
        let mut app = |body: Value| {
            sent = body;
            Ok(json!({ "allowed": true }))
        };
        assert!(allow_act(&mut app, &w, "press Enter", false, &act, None, "").is_ok());
        assert_eq!(sent["action"], "operate");
        assert_eq!(sent["act"]["kind"], "press");
    }

    #[test]
    fn allow_needs_the_app_to_say_allowed() {
        let w = win(1, "A", "a.exe");
        let mut yes = |_: Value| Ok(json!({ "allowed": true }));
        assert!(allow(&mut yes, &w, "see", "look", false, None, "").is_ok());
        let mut silent = |_: Value| Ok(json!({}));
        assert!(allow(&mut silent, &w, "see", "look", false, None, "").is_err());
        let mut no = |_: Value| Err("the user said no".to_string());
        assert_eq!(
            allow(&mut no, &w, "see", "look", false, None, ""),
            Err("the user said no".to_string())
        );
    }

    #[test]
    fn the_done_body_says_how_it_went_in_one_line() {
        let w = win(66, "Proj", "SysmacStudio.exe");
        assert_eq!(
            done_body(
                &w,
                "press Enter in \"Proj\"",
                false,
                "the window\nwould not come forward"
            ),
            json!({
                "op": "desktop.done",
                "window": {
                    "hwnd": 66,
                    "title": "Proj",
                    "exe": "SysmacStudio.exe",
                    "path": r"C:\Program Files\SysmacStudio.exe",
                    "pid": 7,
                },
                "what": "press Enter in \"Proj\"",
                "ok": false,
                "outcome": "the window would not come forward",
            })
        );
        let long = "x".repeat(500);
        let cut = done_body(&w, "look", true, &long);
        assert_eq!(cut["outcome"].as_str().unwrap().chars().count(), 160);
    }

    #[test]
    fn a_report_tells_the_app_and_keeps_the_result() {
        let w = win(1, "A", "a.exe");
        let mut told = Vec::new();
        let mut app = |body: Value| {
            told.push(body);
            Err("an old Horadric knows no desktop.done".to_string())
        };
        let content = vec![json!({ "type": "text", "text": "hi" })];
        assert_eq!(
            report(
                &mut app,
                &w,
                "click",
                Ok((content.clone(), "Clicked.".into()))
            ),
            Ok(content)
        );
        assert_eq!(
            report(&mut app, &w, "click", Err("covered".into())),
            Err("covered".to_string())
        );
        assert_eq!(told.len(), 2);
        assert_eq!(told[0]["op"], "desktop.done");
        assert_eq!(told[0]["ok"], true);
        assert_eq!(told[0]["outcome"], "Clicked.");
        assert_eq!(told[1]["ok"], false);
        assert_eq!(told[1]["outcome"], "covered");
    }

    #[test]
    fn a_rectangle_has_a_middle_unless_it_is_empty() {
        assert_eq!(centre(0, 0, 100, 50), Some((50, 25)));
        assert_eq!(centre(-1920, -200, -1820, -100), Some((-1870, -150)));
        assert_eq!(centre(10, 10, 10, 40), None);
        assert_eq!(centre(10, 40, 30, 20), None);
    }

    #[test]
    fn windows_are_picked_by_id_title_then_program() {
        let list = [
            win(10, "Untitled - Notepad", "notepad.exe"),
            win(20, "Notepad", "other.exe"),
            win(30, "Proj", "SysmacStudio.exe"),
        ];
        assert_eq!(pick_window(&list, "30").unwrap().hwnd, 30);
        assert!(pick_window(&list, "31").is_none());
        assert_eq!(pick_window(&list, "notepad").unwrap().hwnd, 20);
        assert_eq!(pick_window(&list, "UNTITLED").unwrap().hwnd, 10);
        assert_eq!(pick_window(&list, "sysmac").unwrap().hwnd, 30);
        assert!(pick_window(&list, "  ").is_none());
    }

    #[test]
    fn a_click_is_real_unless_the_element_only_expands() {
        assert_eq!(click_way(true, false, true), ClickWay::Real);
        assert_eq!(click_way(true, true, true), ClickWay::Expand);
        assert_eq!(click_way(true, true, false), ClickWay::Expand);
        assert_eq!(click_way(true, false, false), ClickWay::Nowhere);
        // A right or double click is always a real one.
        assert_eq!(click_way(false, true, true), ClickWay::Real);
        assert_eq!(click_way(false, true, false), ClickWay::Nowhere);
    }

    #[test]
    fn a_rectangle_has_a_place_only_with_room() {
        let r = |left, top, right, bottom| RECT {
            left,
            top,
            right,
            bottom,
        };
        assert!(has_place(r(10, 10, 20, 30)));
        assert!(!has_place(r(10, 10, 10, 30)));
        assert!(!has_place(r(0, 0, 0, 0)));
        assert!(!has_place(r(20, 10, 10, 30)));
    }

    #[test]
    fn a_click_point_is_covered_by_any_other_program() {
        assert!(!covered(true, 7, 7));
        assert!(covered(true, 8, 7));
        assert!(covered(false, 7, 7));
    }

    #[test]
    fn an_error_without_a_message_says_its_code() {
        assert_eq!(
            error_reason(0x8000_4005, " Unspecified error "),
            "Unspecified error"
        );
        assert_eq!(
            error_reason(0x8013_1505, ""),
            "UI Automation timed out: the program did not answer (0x80131505)"
        );
        assert_eq!(
            error_reason(0x1234_5678, ""),
            "Windows gave no reason (0x12345678)"
        );
    }

    #[test]
    fn the_window_list_notes_dialogs() {
        let mut d = win(2, "Confirm", "a.exe");
        d.owner = Some(1);
        let open = |_: &Window| Reach::Open;
        let t = windows_text(&[win(1, "Main", "a.exe"), d.clone()], open);
        assert!(t.contains("- id=1 \"Main\" a.exe pid 7\n"), "{t}");
        assert!(
            t.ends_with("- id=2 \"Confirm\" a.exe pid 7 (dialog of id=1)"),
            "{t}"
        );
        assert!(windows_text(&[], open).contains("No windows"));
        let admin = windows_text(&[win(1, "Main", "a.exe"), d], |w| {
            if w.hwnd == 2 {
                Reach::Above
            } else {
                Reach::Open
            }
        });
        assert!(admin.contains("- id=1 \"Main\" a.exe pid 7\n"), "{admin}");
        assert!(
            admin.contains("(dialog of id=1) (as administrator)\n"),
            "{admin}"
        );
        assert!(!admin.contains("Out of reach means"), "{admin}");
        assert!(admin.ends_with("let the helper run as administrator."));
        let unsure = windows_text(&[win(3, "Service", "?")], |_| Reach::Unsure);
        assert!(
            unsure.contains("\"Service\" ? pid 7 (out of reach)\n"),
            "{unsure}"
        );
        assert!(!unsure.contains("(as administrator)"), "{unsure}");
        assert!(unsure.contains("Out of reach means"), "{unsure}");
        let system = windows_text(&[win(4, "Svc", "svc.exe")], |_| Reach::Beyond);
        assert!(
            system.ends_with("(above administrator, beyond reach)"),
            "{system}"
        );
        assert!(!system.contains("helper"), "{system}");
    }

    #[test]
    fn only_a_higher_level_walls_off_and_an_unread_one_is_unsure() {
        use Integrity::*;
        let (medium, high, system) = (Level(0x2000), Level(0x3000), Level(0x4000));
        assert_eq!(reach(medium, high), Reach::Above);
        assert_eq!(reach(high, system), Reach::Beyond);
        assert_eq!(reach(medium, system), Reach::Beyond);
        assert_eq!(reach(system, system), Reach::Open);
        assert_eq!(reach(medium, medium), Reach::Open);
        assert_eq!(reach(high, medium), Reach::Open);
        assert_eq!(reach(medium, Unreadable), Reach::Unsure);
        assert_eq!(reach(medium, Gone), Reach::Open);
        // Not knowing this session's own level walls off nothing.
        assert_eq!(reach(Unreadable, high), Reach::Open);
        assert_eq!(reach(Gone, Unreadable), Reach::Open);
    }

    #[test]
    fn a_wait_says_when_the_helper_failed_it() {
        let n = helper_note("the helper ended");
        assert!(
            n.starts_with("\n(Programs that run as administrator"),
            "{n}"
        );
        assert!(n.contains(": the helper ended."), "{n}");
    }

    #[test]
    fn a_peek_is_read_from_the_helper_s_answer() {
        let p = peeked(&json!({
            "newest": 9, "hit": "\"Done\" appeared", "quiet": false, "lines": ["a", "b"]
        }));
        assert_eq!(
            p,
            Peeked {
                newest: 9,
                hit: Some("\"Done\" appeared".into()),
                quiet: Some(false),
                lines: vec!["a".into(), "b".into()],
            }
        );
        assert_eq!(peeked(&json!({})), Peeked::default());
    }

    #[test]
    fn a_wait_with_the_helper_needs_quiet_on_both_sides_and_text_on_either() {
        let quiet = || Some("nothing changed for 500 ms".to_string());
        let calm = Peeked {
            quiet: Some(true),
            ..Peeked::default()
        };
        let busy = Peeked {
            quiet: Some(false),
            ..Peeked::default()
        };
        assert_eq!(joined_verdict(None, quiet(), &calm), quiet());
        assert_eq!(joined_verdict(None, quiet(), &busy), None);
        assert_eq!(joined_verdict(None, None, &calm), None);
        let hit = Peeked {
            hit: Some("\"Done\" appeared: x".into()),
            quiet: Some(false),
            ..Peeked::default()
        };
        assert_eq!(
            joined_verdict(None, None, &hit).as_deref(),
            Some("\"Done\" appeared: x, in a program that runs as administrator")
        );
        // What holds here is said first.
        assert_eq!(
            joined_verdict(Some("here".into()), None, &hit).as_deref(),
            Some("here")
        );
        // No quiet asked of the helper leaves quiet to this side.
        assert_eq!(joined_verdict(None, quiet(), &Peeked::default()), quiet());
    }

    #[test]
    fn names_prefer_whole_then_fit_elements() {
        let strings = |v: &[&str]| -> Vec<String> { v.iter().map(|s| s.to_string()).collect() };
        let names = strings(&["Name:", "Name", "Rename", "OK"]);
        let none = strings(&["", "", "", ""]);
        assert_eq!(pick_name(&names, &none, &[false; 4], "name"), Some(1));
        assert_eq!(
            pick_name(&names, &none, &[false, false, true, false], "name"),
            Some(2)
        );
        assert_eq!(pick_name(&names, &[], &[], "ok"), Some(3));
        assert_eq!(pick_name(&names, &[], &[], "na"), Some(0));
        assert_eq!(pick_name(&names, &[], &[], "zz"), None);
        assert_eq!(pick_name(&names, &[], &[], ""), None);

        // A Danish program: the id File beats the name "Åbn filen igen".
        let names = strings(&["Åbn filen igen", "Fil", "Gem ikke"]);
        let ids = strings(&["ReopenButton", "File", "SecondaryButton"]);
        let apt = [true, true, true];
        assert_eq!(pick_name(&names, &ids, &apt, "File"), Some(1));
        assert_eq!(pick_name(&names, &ids, &apt, "secondary"), Some(2));
        assert_eq!(pick_name(&names, &ids, &apt, "gem"), Some(2));
    }

    #[test]
    fn control_types_have_names() {
        assert_eq!(control_name(50000), "Button");
        assert_eq!(control_name(50004), "Edit");
        assert_eq!(control_name(50020), "Text");
        assert_eq!(control_name(50040), "AppBar");
        assert_eq!(control_name(50041), "Element");
        assert_eq!(control_name(0), "Element");
    }

    #[test]
    fn a_node_line_says_everything_known() {
        let n = Node {
            depth: 2,
            control: "Button",
            name: "Transfer\nto Controller".into(),
            value: None,
            automation_id: "btnTransfer".into(),
            enabled: false,
            rect: [10, 20, 80, 24],
            actions: vec!["invoke"],
            state: vec!["focused"],
        };
        assert_eq!(
            node_line(&n, 12),
            "    - Button \"Transfer to Controller\" id=btnTransfer disabled focused at 10,20 80x24 (invoke) [ref=e12]"
        );
        let e = Node {
            control: "Edit",
            value: Some("42".into()),
            enabled: true,
            actions: vec!["value"],
            ..Default::default()
        };
        assert_eq!(node_line(&e, 0), "- Edit value=\"42\" (value) [ref=e0]");
    }

    #[test]
    fn a_tree_is_cut_at_the_limit_and_says_so() {
        let n = Node {
            control: "Text",
            name: "x".repeat(50),
            enabled: true,
            ..Default::default()
        };
        let nodes = vec![n; 10];
        let full = tree_text("Head", &nodes, false, 100_000);
        assert_eq!(full.lines().count(), 11);
        assert!(full.contains("[ref=e9]"));
        let cut = tree_text("Head", &nodes, true, 200);
        assert!(cut.contains("cut short here at e"), "{cut}");
        assert!(cut.len() < 400);
        assert!(tree_text("Head", &nodes, true, 100_000).contains("more than 2000"));
        assert!(tree_text("Head", &[], false, 100).contains("shows nothing"));
    }

    #[test]
    fn keys_parse_to_virtual_keys() {
        assert_eq!(
            parse_key("Control+S").unwrap(),
            Chord {
                mods: vec![VK_CONTROL],
                key: Key::Vk(b'S' as u16)
            }
        );
        assert_eq!(
            parse_key("Alt+F4").unwrap(),
            Chord {
                mods: vec![VK_MENU],
                key: Key::Vk(0x73)
            }
        );
        assert_eq!(parse_key("Enter").unwrap().key, Key::Vk(VK_RETURN));
        assert_eq!(parse_key("F5").unwrap().key, Key::Vk(0x74));
        assert_eq!(parse_key("f24").unwrap().key, Key::Vk(0x87));
        assert_eq!(parse_key("Shift+Tab").unwrap().mods, vec![VK_SHIFT]);
        assert_eq!(parse_key("a").unwrap().key, Key::Vk(b'A' as u16));
        assert_eq!(parse_key("7").unwrap().key, Key::Vk(b'7' as u16));
        assert_eq!(parse_key("?").unwrap().key, Key::Char('?'));
        assert_eq!(parse_key("+").unwrap().key, Key::Char('+'));
        assert_eq!(
            parse_key("ctrl++").unwrap(),
            Chord {
                mods: vec![VK_CONTROL],
                key: Key::Char('+')
            }
        );
        assert_eq!(
            parse_key("Control+Control+Shift+down").unwrap().mods,
            vec![VK_CONTROL, VK_SHIFT]
        );
        assert!(parse_key("Hyper+a").is_err());
        assert!(parse_key("NotAKey").is_err());
        assert!(parse_key("F25").is_err());
        assert!(parse_key("Control+").is_err());
    }

    #[test]
    fn the_windows_key_is_never_pressed() {
        assert!(press_refusal(&parse_key("Win+R").unwrap()).is_some());
        assert!(press_refusal(&parse_key("Control+Meta+D").unwrap()).is_some());
        assert!(press_refusal(&parse_key("Control+S").unwrap()).is_none());
        assert!(press_refusal(&parse_key("Enter").unwrap()).is_none());
    }

    #[test]
    fn keys_that_commit_or_destroy_are_risky() {
        let k = |s| parse_key(s).unwrap();
        assert!(risky_key(&k("Delete"), false));
        assert!(risky_key(&k("Shift+Del"), false));
        assert!(risky_key(&k("Control+S"), false));
        assert!(risky_key(&k("Control+Shift+s"), false));
        assert!(risky_key(&k("Alt+F4"), false));
        assert!(risky_key(&k("Enter"), true));
        assert!(!risky_key(&k("Enter"), false));
        assert!(!risky_key(&k("s"), false));
        assert!(!risky_key(&k("F4"), false));
        assert!(!risky_key(&k("F5"), true));
        assert!(!risky_key(&k("Escape"), true));
        // The key's own name is among the texts judged too.
        assert!(looks_risky(&["", "Delete"]));
    }

    #[test]
    fn a_chord_lets_go_in_reverse() {
        let s = chord_strokes(&[VK_CONTROL, VK_SHIFT], 0x53);
        let order: Vec<(u16, bool)> = s
            .iter()
            .map(|s| match *s {
                Stroke::Vk { vk, up } => (vk, up),
                Stroke::Unit { .. } => unreachable!(),
            })
            .collect();
        assert_eq!(
            order,
            [
                (VK_CONTROL, false),
                (VK_SHIFT, false),
                (0x53, false),
                (0x53, true),
                (VK_SHIFT, true),
                (VK_CONTROL, true)
            ]
        );
    }

    #[test]
    fn text_is_typed_as_characters_with_enter_for_lines() {
        let s = text_strokes("aé\r\n😀\t");
        assert_eq!(
            s[0],
            Stroke::Unit {
                unit: 'a' as u16,
                up: false
            }
        );
        assert_eq!(
            s[1],
            Stroke::Unit {
                unit: 'a' as u16,
                up: true
            }
        );
        assert_eq!(
            s[2],
            Stroke::Unit {
                unit: 0xE9,
                up: false
            }
        );
        assert_eq!(
            s[4],
            Stroke::Vk {
                vk: VK_RETURN,
                up: false
            }
        );
        assert_eq!(
            s[6],
            Stroke::Unit {
                unit: 0xD83D,
                up: false
            }
        );
        assert_eq!(
            s[8],
            Stroke::Unit {
                unit: 0xDE00,
                up: false
            }
        );
        assert_eq!(
            s[10],
            Stroke::Vk {
                vk: VK_TAB,
                up: false
            }
        );
        assert_eq!(s.len(), 12);
    }

    #[test]
    fn arrows_and_editing_keys_are_extended() {
        assert!(is_extended(0x25));
        assert!(is_extended(0x2E));
        assert!(!is_extended(VK_RETURN));
        assert!(!is_extended(b'A' as u16));
    }

    #[test]
    fn pictures_shrink_to_fit_keeping_their_shape() {
        assert_eq!(fit(800, 600, 1600), (800, 600));
        assert_eq!(fit(3200, 1800, 1600), (1600, 900));
        assert_eq!(fit(4000, 1, 1600), (1600, 1));
        assert_eq!(fit(0, 0, 1600), (0, 0));
    }

    #[test]
    fn a_crop_is_cut_to_the_picture() {
        assert_eq!(clip([10, 10, 20, 20], 100, 100), Some([10, 10, 20, 20]));
        assert_eq!(clip([-5, 90, 20, 20], 100, 100), Some([0, 90, 15, 10]));
        assert_eq!(clip([200, 0, 20, 20], 100, 100), None);
    }

    #[test]
    fn screen_points_map_onto_the_virtual_desktop() {
        let desk = [0, 0, 1921, 1081];
        assert_eq!(absolute(0, 0, desk), (0, 0));
        assert_eq!(absolute(1920, 1080, desk), (65535, 65535));
        assert_eq!(absolute(960, 540, desk), (32767, 32767));
        // A monitor left of the primary one starts below zero.
        assert_eq!(absolute(-1920, 0, [-1920, 0, 3841, 1081]), (0, 0));
        assert_eq!(absolute(5000, -10, desk), (65535, 0));
    }

    #[test]
    fn refs_are_numbers_after_an_e() {
        assert_eq!(ref_number("e12"), Some(12));
        assert_eq!(ref_number(" e0 "), Some(0));
        assert_eq!(ref_number("12"), None);
        assert_eq!(ref_number("ex"), None);
    }

    /// Not a check but a run to watch: opens a file in Notepad, reads it,
    /// types, selects all, takes pictures, closes the tab without saving.
    /// `HORADRIC_DESKTOP_OUT=<folder> cargo test -p horadric notepad_by_hand`,
    /// with the ignored and nocapture flags handed on to the test binary,
    /// then look at the pictures in the folder.
    #[test]
    #[ignore]
    fn notepad_by_hand() {
        let out = std::path::PathBuf::from(
            std::env::var("HORADRIC_DESKTOP_OUT")
                .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned()),
        );
        let file = out.join("desk-test.txt");
        std::fs::write(&file, "").unwrap();
        let mut notepad = std::process::Command::new("notepad.exe")
            .arg(&file)
            .spawn()
            .unwrap();
        let mut w = None;
        for _ in 0..40 {
            thread::sleep(Duration::from_millis(250));
            w = list_windows()
                .into_iter()
                .find(|w| w.title.contains("desk-test"));
            if w.is_some() {
                break;
            }
        }
        let w = w.expect("Notepad came up");
        thread::sleep(Duration::from_millis(1000));
        let mut d = Desktop::default();
        let mut app = |body: Value| {
            println!("APP <- {body}");
            Ok(json!({ "allowed": true }))
        };
        let settle = Duration::from_millis(400);
        let id = json!(w.hwnd);
        let mut run = |name: &str, args: Value| {
            let r = d.tool(name, &args, &mut app, settle);
            match &r {
                Ok(c) => {
                    for part in c {
                        match part["type"].as_str() {
                            Some("text") => println!("{name}: {}", part["text"].as_str().unwrap()),
                            _ => println!("{name}: an image"),
                        }
                    }
                }
                Err(e) => println!("{name} ERROR: {e}"),
            }
            r
        };
        let snap = |c: &[Value]| c[0]["text"].as_str().unwrap_or("").to_string();
        run("desktop_windows", json!({})).unwrap();
        let tree = snap(&run("desktop_snapshot", json!({ "window": id })).unwrap());
        let doc = tree
            .lines()
            .find(|l| l.contains("- Document") || l.contains("- Edit"))
            .and_then(|l| l.rsplit("[ref=").next())
            .map(|r| r.trim_end_matches(']').to_string());
        let aim = match &doc {
            Some(r) => json!({ "window": id, "ref": r }),
            None => json!({ "window": id, "name": "Text editor" }),
        };
        let mut typing = aim.clone();
        typing["text"] = json!("Hello from Horadric\nA second line, typed for real.");
        run("desktop_type", typing).unwrap();
        run("desktop_press", json!({ "window": id, "key": "Control+A" })).unwrap();
        let shot = run("desktop_screenshot", json!({ "window": id })).unwrap();
        let png = horadric_core::release::base64_decode(shot[0]["data"].as_str().unwrap()).unwrap();
        std::fs::write(out.join("desk-window.png"), png).unwrap();
        if let Some(r) = &doc {
            run("desktop_snapshot", json!({ "window": id })).unwrap();
            let shot = run("desktop_screenshot", json!({ "window": id, "ref": r })).unwrap();
            std::fs::write(
                out.join("desk-element.png"),
                horadric_core::release::base64_decode(shot[0]["data"].as_str().unwrap()).unwrap(),
            )
            .unwrap();
        }
        let _ = run("desktop_click", json!({ "window": id, "name": "File" }));
        thread::sleep(Duration::from_millis(500));
        let shot = run("desktop_screenshot", json!({ "window": id })).unwrap();
        std::fs::write(
            out.join("desk-menu.png"),
            horadric_core::release::base64_decode(shot[0]["data"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        run("desktop_press", json!({ "window": id, "key": "Escape" })).unwrap();
        run("desktop_press", json!({ "window": id, "key": "Escape" })).unwrap();
        run("desktop_press", json!({ "window": id, "key": "Control+A" })).unwrap();
        run("desktop_press", json!({ "window": id, "key": "Delete" })).unwrap();
        let _ = run("desktop_press", json!({ "window": id, "key": "Control+W" }));
        thread::sleep(Duration::from_millis(800));
        let _ = run("desktop_windows", json!({}));
        let _ = run("desktop_snapshot", json!({ "window": id }));
        let _ = run(
            "desktop_click",
            json!({ "window": id, "name": "SecondaryButton" }),
        );
        thread::sleep(Duration::from_millis(800));
        let _ = run("desktop_press", json!({ "window": id, "key": "Alt+F4" }));
        thread::sleep(Duration::from_millis(1500));
        if list_windows().iter().any(|x| x.hwnd == w.hwnd) {
            println!("Notepad is still open, pid {}", w.pid);
        }
        let _ = notepad.kill();
        let _ = notepad.wait();
        let _ = std::fs::remove_file(&file);
    }

    /// One tool call by hand, for checking a program on screen:
    /// `TOOL=desktop_snapshot ARGS='{"window":"Notepad"}' cargo test -p
    /// horadric one_tool`, with the ignored and nocapture flags handed on to
    /// the test binary. A picture is written to
    /// `HORADRIC_DESKTOP_OUT`, or the temp folder.
    #[test]
    #[ignore]
    fn one_tool() {
        let name = std::env::var("TOOL").unwrap();
        let args: Value =
            serde_json::from_str(&std::env::var("ARGS").unwrap_or("{}".into())).unwrap();
        let mut d = Desktop::default();
        let mut app = |body: Value| {
            println!("APP <- {body}");
            Ok(json!({ "allowed": true }))
        };
        match d.tool(&name, &args, &mut app, Duration::from_millis(400)) {
            Ok(content) => {
                for part in content {
                    if let Some(t) = part["text"].as_str() {
                        println!("{t}");
                    } else {
                        let out = std::env::var("HORADRIC_DESKTOP_OUT")
                            .map(std::path::PathBuf::from)
                            .unwrap_or_else(|_| std::env::temp_dir());
                        let png =
                            horadric_core::release::base64_decode(part["data"].as_str().unwrap())
                                .unwrap();
                        std::fs::write(out.join("one-tool.png"), png).unwrap();
                        println!("picture in {}", out.join("one-tool.png").display());
                    }
                }
            }
            Err(e) => println!("ERROR: {e}"),
        }
    }

    /// A window of our own to operate: a button opens a modal dialog with
    /// a field, Escape closes it.
    const DIALOG_FORM: &str = r#"
Add-Type -AssemblyName System.Windows.Forms
$f = New-Object Windows.Forms.Form
$f.Text = 'Horadric operate test'
$b = New-Object Windows.Forms.Button; $b.Text = 'Settings'; $b.Top = 10
$b.Add_Click({
  $d = New-Object Windows.Forms.Form; $d.Text = 'Settings dialog'
  $t = New-Object Windows.Forms.TextBox; $t.AccessibleName = 'IP address'; $t.Text = '10.0.0.3'; $t.Top = 10
  $x = New-Object Windows.Forms.Button; $x.Text = 'Cancel'; $x.Top = 40
  $x.DialogResult = [Windows.Forms.DialogResult]::Cancel
  $d.Controls.AddRange(@($t, $x)); $d.CancelButton = $x
  [void]$d.ShowDialog($f)
})
$f.Controls.Add($b)
[Windows.Forms.Application]::Run($f)
"#;

    /// Not a check but a run to read: starts the form above, opens its
    /// dialog, types into its field, asks what changed, closes the dialog,
    /// waits for it to be gone and for quiet, and closes the form.
    /// `cargo test -p horadric watch_a_dialog_by_hand`, with the ignored and
    /// nocapture flags handed on to the test binary.
    #[test]
    #[ignore]
    fn watch_a_dialog_by_hand() {
        let script = std::env::temp_dir().join("horadric-operate-form.ps1");
        std::fs::write(&script, DIALOG_FORM).unwrap();
        let mut form = std::process::Command::new("powershell")
            .args(["-NoProfile", "-STA", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&script)
            .spawn()
            .unwrap();
        let mut w = None;
        for _ in 0..40 {
            thread::sleep(Duration::from_millis(250));
            w = list_windows()
                .into_iter()
                .find(|w| w.title == "Horadric operate test");
            if w.is_some() {
                break;
            }
        }
        let w = w.expect("the form came up");
        let mut d = Desktop::default();
        let mut app = |_: Value| Ok(json!({ "allowed": true }));
        let id = json!(w.hwnd);
        let mut run = |name: &str, args: Value| {
            let r = d.tool(name, &args, &mut app, Duration::from_millis(400));
            match &r {
                Ok(c) => println!("{name}: {}", c[0]["text"].as_str().unwrap_or("(an image)")),
                Err(e) => println!("{name} ERROR: {e}"),
            }
            r.map(|c| c[0]["text"].as_str().unwrap_or_default().to_string())
                .unwrap_or_else(|e| format!("ERROR: {e}"))
        };
        let first = run("desktop_changes", json!({ "window": id }));
        // A real key, not UI Automation's invoke: a WinForms button holds
        // that until its modal dialog closes, and UI Automation then waits
        // on the program for anything else.
        let clicked = run("desktop_press", json!({ "window": id, "key": "Enter" }));
        let waited_for = run(
            "desktop_wait",
            json!({ "appears": "Settings dialog", "timeout_ms": 5000 }),
        );
        let typed = run(
            "desktop_type",
            json!({ "window": "Settings dialog", "name": "IP address", "text": "10.0.0.5" }),
        );
        let changes = run("desktop_changes", json!({ "window": id }));
        let again = run("desktop_changes", json!({ "window": id }));
        let escaped = run(
            "desktop_press",
            json!({ "window": "Settings dialog", "key": "Escape" }),
        );
        let gone = run(
            "desktop_wait",
            json!({ "window": id, "gone": "Settings dialog", "timeout_ms": 5000 }),
        );
        let quiet = run(
            "desktop_wait",
            json!({ "quiet_ms": 800, "timeout_ms": 5000 }),
        );
        let _ = form.kill();
        let _ = form.wait();
        let _ = std::fs::remove_file(&script);
        assert!(first.starts_with("Nothing changed"), "{first}");
        assert!(clicked.contains("dialog shown"), "{clicked}");
        assert!(waited_for.starts_with("Done after"), "{waited_for}");
        assert!(typed.contains("10.0.0.3\" -> \"10.0.0.5"), "{typed}");
        assert!(
            changes.contains("dialog shown \"Settings dialog\""),
            "{changes}"
        );
        assert!(changes.contains("10.0.0.5"), "{changes}");
        assert!(again.starts_with("Nothing changed"), "{again}");
        assert!(escaped.contains("dialog closed"), "{escaped}");
        assert!(gone.starts_with("Done after"), "{gone}");
        assert!(quiet.contains("nothing changed for 800 ms"), "{quiet}");
    }

    /// The same for a window that runs as administrator, reached through
    /// the helper, on a machine that elevates without asking: starts a
    /// small form as administrator that changes by itself, reads it once,
    /// and asks what changed. `cargo build -p horadric` first, for the
    /// helper, then `cargo test -p horadric watch_elevated_by_hand`, with
    /// the ignored and nocapture flags handed on to the test binary.
    #[test]
    #[ignore]
    fn watch_elevated_by_hand() {
        let script = std::env::temp_dir().join("horadric-watch-elevated.ps1");
        std::fs::write(&script, crate::watch::tests::FORM).unwrap();
        let start = format!(
            "(Start-Process powershell -Verb RunAs -PassThru -ArgumentList '-NoProfile','-STA',\
             '-ExecutionPolicy','Bypass','-File','{}').Id",
            script.display()
        );
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &start])
            .output()
            .unwrap();
        let pid: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        let mut w = None;
        for _ in 0..40 {
            thread::sleep(Duration::from_millis(100));
            w = list_windows().into_iter().find(|w| w.pid == pid);
            if w.is_some() {
                break;
            }
        }
        let w = w.expect("the form came up");
        let mut d = Desktop::default();
        let mut app = |_: Value| Ok(json!({ "allowed": true }));
        let id = json!(w.hwnd);
        let mut run = |name: &str, args: Value| {
            let r = d.tool(name, &args, &mut app, Duration::from_millis(400));
            match &r {
                Ok(c) => println!("{name}: {}", c[0]["text"].as_str().unwrap_or("(an image)")),
                Err(e) => println!("{name} ERROR: {e}"),
            }
            r
        };
        let _ = run("desktop_snapshot", json!({ "window": id }));
        let waited = run(
            "desktop_wait",
            json!({ "window": id, "text": "Watch dialog", "timeout_ms": 8000 }),
        );
        thread::sleep(Duration::from_millis(2500));
        let changes = run("desktop_changes", json!({ "window": id }));
        let all = run("desktop_changes", json!({}));
        thread::sleep(Duration::from_secs(3));
        let _ = std::fs::remove_file(&script);
        let text =
            |r: Result<Vec<Value>, String>| r.unwrap()[0]["text"].as_str().unwrap().to_string();
        let waited = text(waited);
        assert!(waited.starts_with("Done after"), "{waited}");
        assert!(
            waited.contains("CheckBox \"Online\" now checked"),
            "{waited}"
        );
        let changes = text(changes);
        assert!(changes.contains("dialog closed"), "{changes}");
        assert!(text(all).contains("run as administrator"));
    }

    #[test]
    fn short_keeps_one_line_and_cuts() {
        assert_eq!(short("a\n b", 10), "a b");
        assert_eq!(short("abcdefghij", 6), "abc...");
    }
}
