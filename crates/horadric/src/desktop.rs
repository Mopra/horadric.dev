//! The desktop tools of `horadric mcp`: an agent can list the programs on
//! the user's desktop, read a window's controls through UI Automation, take
//! a picture of it, and click, type and press keys in it, the way the
//! browser tools give it the web.
//!
//! All of it runs here, in the MCP process on the user's desktop, so a slow
//! UI Automation call never holds up Horadric's own windows. The app is
//! only asked whether the human allows it: once per program per session,
//! and again for anything that looks risky.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};
use windows::core::{Interface, BSTR, HRESULT};
use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, HANDLE, HWND, LPARAM, POINT, RECT};
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
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, STREAM_SEEK_END,
    STREAM_SEEK_SET,
};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcess, GetCurrentThreadId, OpenProcess, OpenProcessToken,
    QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
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
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, SetCursorPos, SetForegroundWindow,
    ShowWindow, WindowFromPoint, GA_ROOT, GWL_EXSTYLE, GW_OWNER, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_RESTORE, WS_EX_TOOLWINDOW,
};

use crate::desktop_helper::{as_administrator, Helper, HelperStart};
use crate::mcp::text;

/// The most elements one snapshot holds, so a vast window can not take
/// minutes or fill the context.
const MOST_ELEMENTS: usize = 2000;

/// A dialog with fewer elements than this is read whole to judge whether
/// what is about to happen in it is risky.
const SMALL_DIALOG: usize = 40;

/// The widest picture, in pixels.
const WIDEST: u32 = 1600;

/// How long a UI Automation pattern may take before the answer says it is
/// still going. A button that opens a modal dialog can hold its Invoke
/// until the dialog closes.
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

/// The window as the app's bodies name it.
fn window_json(window: &Window) -> Value {
    json!({
        "hwnd": window.hwnd,
        "title": window.title,
        "exe": window.exe,
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
    if answer["allowed"] == true {
        Ok(())
    } else {
        Err(format!("Horadric did not allow you to {what}"))
    }
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

/// The window list as the agent reads it.
pub fn windows_text(list: &[Window]) -> String {
    if list.is_empty() {
        return "No windows are open on the desktop.".into();
    }
    let mut out = String::from("Windows on the desktop, frontmost first:\n");
    for w in list {
        out.push_str(&format!(
            "- id={} {:?} {} pid {}",
            w.hwnd, w.title, w.exe, w.pid
        ));
        if let Some(o) = w.owner {
            out.push_str(&format!(" (dialog of id={o})"));
        }
        out.push('\n');
    }
    out.pop();
    out
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
    /// Why the helper can not be had: the user declined it, or it died.
    /// Kept so Windows does not ask the user again on every call.
    no_helper: Option<String>,
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
    e.message().to_string()
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

fn exe_of(pid: u32) -> String {
    unsafe {
        let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return UNKNOWN_EXE.into();
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
            return UNKNOWN_EXE.into();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string()
    }
}

/// Whether process `pid`, or this one when none, runs as administrator,
/// when Windows says.
fn elevated(pid: Option<u32>) -> Option<bool> {
    unsafe {
        let process = match pid {
            Some(id) => OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, id).ok()?,
            None => GetCurrentProcess(),
        };
        let mut token = HANDLE::default();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token);
        if pid.is_some() {
            let _ = CloseHandle(process);
        }
        opened.ok()?;
        let mut e = TOKEN_ELEVATION::default();
        let mut len = 0;
        let read = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut e as *mut TOKEN_ELEVATION as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        let _ = CloseHandle(token);
        read.ok()?;
        Some(e.TokenIsElevated != 0)
    }
}

/// Why `w` can not be read or operated, when it runs as administrator and
/// this session does not: Windows then shows UI Automation only its title
/// bar and drops input sent to it.
fn out_of_reach(w: &Window) -> Option<String> {
    (elevated(Some(w.pid)) == Some(true) && elevated(None) == Some(false)).then(|| {
        format!(
            "{} runs as administrator and this session does not, so Windows shows the agent \
             only its title bar and lets it neither click nor type there (pictures come off the screen). \
             It can only be reached through a helper that runs as administrator",
            w.exe
        )
    })
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

fn info(id: isize) -> Window {
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
        Window {
            hwnd: id,
            title,
            exe: exe_of(pid),
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
    if target.exe == UNKNOWN_EXE {
        return Err(format!(
            "can not tell which program {:?} is (it may run as administrator), so it can not be \
             approved",
            target.title
        ));
    }
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
            // Windows lets a process bring a window forward when it sent
            // the last input, and a mouse move by nothing is input enough.
            send(&[mouse(0, 0, MOUSEEVENTF_MOVE)]);
            if !SetForegroundWindow(h).as_bool() {
                let front = GetWindowThreadProcessId(GetForegroundWindow(), None);
                let me = GetCurrentThreadId();
                let _ = AttachThreadInput(me, front, true);
                let _ = SetForegroundWindow(h);
                let _ = BringWindowToTop(h);
                let _ = AttachThreadInput(me, front, false);
            }
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

impl Desktop {
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

    /// Hands a call for an elevated window to the helper, starting it the
    /// first time. The outer error says the helper is out of reach.
    fn through_helper<A: FnMut(Value) -> Result<Value, String>>(
        &mut self,
        name: &str,
        args: &Value,
        app: &mut A,
        settle: Duration,
    ) -> Result<Result<Vec<Value>, String>, String> {
        if let Some(no) = &self.no_helper {
            return Err(no.clone());
        }
        if self.helper.is_none() {
            match Helper::start() {
                Ok(h) => self.helper = Some(h),
                Err(HelperStart::Declined(no)) => {
                    self.no_helper = Some(no.clone());
                    return Err(no);
                }
                Err(HelperStart::Failed(no)) => return Err(no),
            }
        }
        let helper = self.helper.as_mut().expect("started just above");
        // A grant for a program does not cover the same program running as
        // administrator: the human is asked about that one by itself.
        let mut marked = |body: Value| app(as_administrator(body));
        let answer = helper.call(name, args, &mut marked, settle);
        if let Err(why) = &answer {
            self.helper = None;
            self.no_helper = Some(format!(
                "{why} (it may already have acted; it is not started again this session)"
            ));
        }
        answer
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
            return Ok(vec![text(windows_text(&list_windows()))]);
        }
        let (w, root) = find(args.get("window"))?;
        let mut walled = out_of_reach(&w);
        if let Some(why) = &walled {
            match self.through_helper(name, args, app, settle) {
                Ok(answer) => return answer,
                Err(no) => walled = Some(format!("{why}, and {no}")),
            }
        }
        if let (Some(why), "desktop_click" | "desktop_type" | "desktop_press") = (&walled, name) {
            return Err(why.clone());
        }
        // The agent's words for the human, kept to one line.
        let reason = short(s("reason").unwrap_or_default(), 300);
        match name {
            "desktop_snapshot" => {
                let what = format!("read the controls of {:?}", w.title);
                allow(app, &root, "see", &what, false, None, &reason)?;
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
                let risky = flag("risky") || self.looks_risky(&w, &[&aim.name]);
                let point = aim_point(&w, &aim);
                allow(app, &root, "operate", &what, risky, point, &reason)?;
                let before = list_windows();
                let result = self.click(&w, &aim, button, double).map(|mut said| {
                    thread::sleep(settle);
                    said.push_str(&opened(&before, &w));
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
                // Enter in a dialog confirms it, as pressing it alone would.
                let risky = flag("risky")
                    || (submit && is_dialog(&w))
                    || self.looks_risky(&w, &[&aim.name]);
                let point = aim_point(&w, &aim);
                allow(app, &root, "operate", &what, risky, point, &reason)?;
                let before = list_windows();
                let result = self.type_in(&w, &aim, typed, submit).map(|mut said| {
                    thread::sleep(settle);
                    said.push_str(&opened(&before, &w));
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
                let risky = flag("risky")
                    || risky_key(&chord, is_dialog(&w))
                    || self.looks_risky(&w, &[&focused, key]);
                let point = at.or_else(|| window_point(&w));
                allow(app, &root, "operate", &what, risky, point, &reason)?;
                let before = list_windows();
                let pressed = bring_forward(&w).and_then(|()| send_strokes(&chord_input(&chord)));
                let result = pressed.map(|()| {
                    thread::sleep(settle);
                    let mut said = format!("Pressed {key} in {:?}.", w.title);
                    said.push_str(&opened(&before, &w));
                    (vec![text(said.clone())], said)
                });
                report(app, &root, &what, result)
            }
            _ => Err(format!("there is no tool {name}")),
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

    /// Whether acting on `names` (an element, a key) in `w` looks risky:
    /// by them, the title, and for a small dialog, what its text says.
    fn looks_risky(&mut self, w: &Window, names: &[&str]) -> bool {
        let mut texts: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        texts.push(w.title.clone());
        if is_dialog(w) {
            if let Ok(all) = self.all(w) {
                if all.len() < SMALL_DIALOG {
                    texts.extend(
                        all.into_iter()
                            .filter(|a| a.control == "Text")
                            .map(|a| a.name),
                    );
                }
            }
        }
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        looks_risky(&refs)
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
        if button == "left" && !double {
            let done = if aim.invoke {
                Some((
                    "invoked",
                    on_the_side(&aim.el, |el| unsafe {
                        el.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)?
                            .Invoke()
                    }),
                ))
            } else if aim.toggle {
                Some((
                    "toggled",
                    on_the_side(&aim.el, |el| unsafe {
                        el.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)?
                            .Toggle()
                    }),
                ))
            } else if aim.select {
                Some((
                    "selected",
                    on_the_side(&aim.el, |el| unsafe {
                        el.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                            UIA_SelectionItemPatternId,
                        )?
                        .Select()
                    }),
                ))
            } else if aim.expand {
                let expanded = aim.expanded;
                Some((
                    if expanded { "collapsed" } else { "expanded" },
                    on_the_side(&aim.el, move |el| unsafe {
                        let p = el.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                            UIA_ExpandCollapsePatternId,
                        )?;
                        if expanded {
                            p.Collapse()
                        } else {
                            p.Expand()
                        }
                    }),
                ))
            } else {
                None
            };
            match done {
                Some((verb, Ok(true))) => {
                    return Ok(format!("Clicked {thing}: {verb} it through UI Automation."))
                }
                Some((verb, Ok(false))) => {
                    return Ok(format!(
                        "Clicked {thing}: {verb} it through UI Automation, and the program has \
                         not finished with it after {} seconds (a modal dialog may have opened).",
                        PATTERN_WAIT.as_secs()
                    ))
                }
                // A pattern that fails may have done half its work, so it
                // is not followed by a click that could do it again.
                Some((verb, Err(e))) => {
                    return Err(format!(
                        "{thing} was not {verb}: UI Automation said {e}; nothing else was tried"
                    ))
                }
                None => {}
            }
        }
        bring_forward(w)?;
        let r = aim.rect;
        if r.right <= r.left || r.bottom <= r.top {
            return Err(format!(
                "{thing} has no place on screen (scrolled out of view or hidden), so it can not be \
                 clicked; scroll to it first"
            ));
        }
        let (x, y) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
        let desk = unsafe {
            [
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            ]
        };
        // The click lands on whatever is on top at that point, which must
        // be the approved program and not something covering it.
        unsafe {
            let under = WindowFromPoint(POINT { x, y });
            let top = GetAncestor(under, GA_ROOT);
            let mut pid = 0;
            GetWindowThreadProcessId(top, Some(&mut pid));
            if under.is_invalid() || pid != w.pid {
                let c = info(top.0 as isize);
                return Err(format!(
                    "{thing} is covered there by {:?} ({}), so a click would land on that; \
                     nothing was clicked",
                    c.title, c.exe
                ));
            }
        }
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
        let o = window_rect(w.hwnd);
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
                format!(
                    "Typed {} characters into {thing} at its caret",
                    typed.chars().count()
                )
            };
        if submit {
            if aim.writable {
                self.focus(w, aim)
                    .map_err(|e| format!("{said}, but Enter was not pressed: {e}"))?;
            }
            send_strokes(&chord_strokes(&[], VK_RETURN))?;
            said.push_str(" and pressed Enter");
        }
        said.push('.');
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
            pid: 7,
            owner: None,
            class: String::new(),
        }
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
                "window": { "hwnd": 66, "title": "Proj - Sysmac Studio", "exe": "SysmacStudio.exe", "pid": 7 },
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
                "window": { "hwnd": 66, "title": "Proj", "exe": "SysmacStudio.exe", "pid": 7 },
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
    fn the_window_list_notes_dialogs() {
        let mut d = win(2, "Confirm", "a.exe");
        d.owner = Some(1);
        let t = windows_text(&[win(1, "Main", "a.exe"), d]);
        assert!(t.contains("- id=1 \"Main\" a.exe pid 7\n"), "{t}");
        assert!(
            t.ends_with("- id=2 \"Confirm\" a.exe pid 7 (dialog of id=1)"),
            "{t}"
        );
        assert!(windows_text(&[]).contains("No windows"));
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
    /// `HORADRIC_DESKTOP_OUT=<folder> cargo test -p horadric notepad_by_hand
    /// -- --ignored --nocapture`, then look at the pictures in the folder.
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
    /// horadric one_tool -- --ignored --nocapture`. A picture is written to
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

    #[test]
    fn short_keeps_one_line_and_cuts() {
        assert_eq!(short("a\n b", 10), "a b");
        assert_eq!(short("abcdefghij", 6), "abc...");
    }
}
