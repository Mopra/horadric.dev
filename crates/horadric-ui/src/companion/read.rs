//! Reading the text a window shows, through UI Automation.
//!
//! A document's own text is taken where the program offers it (a page
//! in the browser, a mail being written, an editor). Where it does not,
//! the window's parts are walked and the names and values of those that
//! bear text are gathered. Password fields are stepped over, and
//! everything is capped: depth, parts, characters and time.
//!
//! Each reading runs on a thread of its own, because a call into a
//! program that hangs can wait for ever. The caller waits a while and
//! moves on; the thread that is stuck is left to finish or not, and no
//! new reading of that window starts while it is.

use std::collections::HashSet;
use std::ffi::c_void;
use std::fmt;
use std::sync::mpsc;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, CUIAutomation8, IUIAutomation, IUIAutomation2, IUIAutomationCacheRequest,
    IUIAutomationCondition, IUIAutomationElement, IUIAutomationTextPattern, TreeScope_Children,
    UIA_ComboBoxControlTypeId, UIA_ControlTypePropertyId, UIA_DataItemControlTypeId,
    UIA_DocumentControlTypeId, UIA_EditControlTypeId, UIA_HeaderItemControlTypeId,
    UIA_HyperlinkControlTypeId, UIA_IsPasswordPropertyId, UIA_IsTextPatternAvailablePropertyId,
    UIA_ListItemControlTypeId, UIA_MenuBarControlTypeId, UIA_MenuControlTypeId,
    UIA_NamePropertyId, UIA_ScrollBarControlTypeId, UIA_TabItemControlTypeId,
    UIA_TextControlTypeId, UIA_TextPatternId, UIA_TitleBarControlTypeId, UIA_ToolTipControlTypeId,
    UIA_TreeItemControlTypeId, UIA_ValueValuePropertyId, UIA_CONTROLTYPE_ID, UIA_PROPERTY_ID,
};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

/// The most text one reading gives, in characters.
pub const MOST_CHARS: usize = 20_000;

/// The most parts of a window one reading looks at.
const MOST_PARTS: usize = 3_000;

const MOST_DEPTH: usize = 40;

/// How long the walk through a window's parts may go on.
const WALK_FOR: Duration = Duration::from_millis(1_500);

/// How long the caller waits for a reading. Longer than the walk, which
/// only looks at its clock between calls and one call can outlast it.
const WAIT_FOR: Duration = Duration::from_millis(2_500);

/// How long UI Automation itself waits on one call into a program
/// before it gives up, in milliseconds. Its own default is 20 seconds.
const CALL_MS: u32 = 1_000;

/// Readings that may be stuck at once before no more are started.
const STUCK_MOST: usize = 3;

/// Why a reading gave nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Miss {
    /// An earlier reading of this window has not come back.
    Stuck,
    /// Too many readings are stuck in programs that do not answer.
    Crowded,
    /// The program did not answer in time.
    Late,
    Failed(String),
}

impl fmt::Display for Miss {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Miss::Stuck => f.write_str("an earlier reading of this window is still stuck"),
            Miss::Crowded => f.write_str("too many readings are stuck in programs that hang"),
            Miss::Late => f.write_str("the program did not answer in time"),
            Miss::Failed(why) => write!(f, "UI Automation failed: {why}"),
        }
    }
}

/// The windows being read now, each by a thread that may be stuck.
static READING: Mutex<Vec<isize>> = Mutex::new(Vec::new());

/// The text `hwnd` shows, None when it gave none or did not answer.
pub fn text_of(hwnd: HWND) -> Option<String> {
    read(hwnd).ok().filter(|t| !t.trim().is_empty())
}

/// [`text_of`] that says why there is nothing, for `companion peek`.
pub fn read(hwnd: HWND) -> Result<String, Miss> {
    let id = hwnd.0 as isize;
    {
        let mut reading = READING.lock().unwrap_or_else(|e| e.into_inner());
        if reading.contains(&id) {
            return Err(Miss::Stuck);
        }
        if reading.len() >= STUCK_MOST {
            return Err(Miss::Crowded);
        }
        reading.push(id);
    }
    let done = move || {
        READING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|h| *h != id);
    };
    let (tx, rx) = mpsc::channel();
    let started = thread::Builder::new()
        .name("companion-read".into())
        .spawn(move || {
            let out = std::panic::catch_unwind(|| worker(id))
                .unwrap_or_else(|_| Err(Miss::Failed("the reading panicked".into())));
            done();
            // Nobody listens any more when the caller gave up.
            let _ = tx.send(out);
        });
    if let Err(e) = started {
        done();
        return Err(Miss::Failed(e.to_string()));
    }
    rx.recv_timeout(WAIT_FOR).unwrap_or(Err(Miss::Late))
}

fn worker(id: isize) -> Result<String, Miss> {
    // SAFETY: COM is started on this thread before it is used and ended
    // after `gather` has dropped everything it got from it.
    unsafe {
        let started = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let out = gather(HWND(id as *mut c_void)).map_err(|e| Miss::Failed(e.message()));
        if started {
            CoUninitialize();
        }
        out
    }
}

/// What a reading asks of every part in the one call that fetches it,
/// so the walk costs one call into the program for each set of
/// children and none for each part.
const ASKED: [UIA_PROPERTY_ID; 5] = [
    UIA_NamePropertyId,
    UIA_ControlTypePropertyId,
    UIA_IsPasswordPropertyId,
    UIA_ValueValuePropertyId,
    UIA_IsTextPatternAvailablePropertyId,
];

/// Parts whose name or value is text the human reads. Buttons, panes
/// and the like name themselves, which is not what is on the page.
const BEARS_TEXT: [UIA_CONTROLTYPE_ID; 10] = [
    UIA_TextControlTypeId,
    UIA_EditControlTypeId,
    UIA_DocumentControlTypeId,
    UIA_HyperlinkControlTypeId,
    UIA_ListItemControlTypeId,
    UIA_DataItemControlTypeId,
    UIA_TreeItemControlTypeId,
    UIA_HeaderItemControlTypeId,
    UIA_TabItemControlTypeId,
    UIA_ComboBoxControlTypeId,
];

/// Parts not gone into: the program's own furniture.
const PASSED_BY: [UIA_CONTROLTYPE_ID; 5] = [
    UIA_MenuBarControlTypeId,
    UIA_MenuControlTypeId,
    UIA_ScrollBarControlTypeId,
    UIA_TitleBarControlTypeId,
    UIA_ToolTipControlTypeId,
];

struct Walk {
    cache: IUIAutomationCacheRequest,
    children: IUIAutomationCondition,
    until: Instant,
    parts: usize,
    got: Gathered,
}

unsafe fn gather(hwnd: HWND) -> windows::core::Result<String> {
    let uia: IUIAutomation = CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)
        .or_else(|_| CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER))?;
    if let Ok(two) = uia.cast::<IUIAutomation2>() {
        let _ = two.SetConnectionTimeout(CALL_MS);
        let _ = two.SetTransactionTimeout(CALL_MS);
    }
    let cache = uia.CreateCacheRequest()?;
    for p in ASKED {
        cache.AddProperty(p)?;
    }
    // The document the human is in, when the keyboard is in one: its
    // text is the page or the mail, and the walk through the program's
    // frame around it is saved.
    if GetForegroundWindow() == hwnd {
        if let Ok(focused) = uia.GetFocusedElementBuildCache(&cache) {
            let document = focused
                .CachedControlType()
                .is_ok_and(|kind| kind == UIA_DocumentControlTypeId);
            if document && !password(&focused) && has_text(&focused) {
                if let Some(text) = pattern_text(&focused, MOST_CHARS) {
                    if !text.trim().is_empty() {
                        return Ok(tidy(&text, MOST_CHARS));
                    }
                }
            }
        }
    }
    let root = uia.ElementFromHandleBuildCache(hwnd, &cache)?;
    let mut walk = Walk {
        children: uia.ControlViewCondition()?,
        cache,
        until: Instant::now() + WALK_FOR,
        parts: 0,
        got: Gathered::new(MOST_CHARS),
    };
    walk.part(&root, 0, None);
    Ok(walk.got.text)
}

impl Walk {
    /// Gathers what `el` and the parts under it say. `within` is what a
    /// part above already said: a list item names itself by the texts
    /// inside it, which are then not said again.
    unsafe fn part(&mut self, el: &IUIAutomationElement, depth: usize, within: Option<&str>) {
        if self.got.full()
            || self.parts >= MOST_PARTS
            || depth > MOST_DEPTH
            || Instant::now() >= self.until
        {
            return;
        }
        self.parts += 1;
        // A password field, and whatever is inside it.
        if password(el) {
            return;
        }
        let kind = el.CachedControlType().unwrap_or_default();
        if PASSED_BY.contains(&kind) {
            return;
        }
        let whole = kind == UIA_DocumentControlTypeId || kind == UIA_EditControlTypeId;
        if whole && has_text(el) {
            if let Some(text) = pattern_text(el, self.got.left()) {
                if !text.trim().is_empty() {
                    self.got.whole(&text);
                    return;
                }
            }
        }
        let mut said = None;
        if BEARS_TEXT.contains(&kind) {
            let name = el.CachedName().map(|b| b.to_string()).unwrap_or_default();
            let value = cached_string(el, UIA_ValueValuePropertyId).unwrap_or_default();
            self.got.piece(&name, within);
            if value != name {
                self.got.piece(&value, within);
            }
            said = Some(format!("{name}\n{value}")).filter(|s| !s.trim().is_empty());
        }
        let Ok(children) = el.FindAllBuildCache(TreeScope_Children, &self.children, &self.cache)
        else {
            return;
        };
        let within = said.as_deref().or(within);
        for i in 0..children.Length().unwrap_or(0) {
            if let Ok(child) = children.GetElement(i) {
                self.part(&child, depth + 1, within);
            }
        }
    }
}

unsafe fn password(el: &IUIAutomationElement) -> bool {
    el.CachedIsPassword().is_ok_and(|b| b.as_bool())
}

unsafe fn has_text(el: &IUIAutomationElement) -> bool {
    el.GetCachedPropertyValue(UIA_IsTextPatternAvailablePropertyId)
        .ok()
        .and_then(|v| bool::try_from(&v).ok())
        .unwrap_or(false)
}

unsafe fn cached_string(el: &IUIAutomationElement, p: UIA_PROPERTY_ID) -> Option<String> {
    el.GetCachedPropertyValue(p)
        .ok()
        .and_then(|v| windows::core::BSTR::try_from(&v).ok())
        .map(|b| b.to_string())
}

/// The text of a part that has the text pattern, `most` characters of
/// it. A document longer than that gives what is on screen instead of
/// its first pages: the human is working where they have scrolled to.
unsafe fn pattern_text(el: &IUIAutomationElement, most: usize) -> Option<String> {
    let pattern = el
        .GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
        .ok()?;
    // One more than wanted tells a text that ends there from one cut.
    let ask = i32::try_from(most).unwrap_or(i32::MAX - 1) + 1;
    let all = pattern.DocumentRange().ok()?.GetText(ask).ok()?.to_string();
    if all.chars().count() <= most {
        return Some(all);
    }
    let visible = pattern.GetVisibleRanges().ok().and_then(|ranges| {
        let mut out = String::new();
        for i in 0..ranges.Length().ok()? {
            let text = ranges.GetElement(i).ok()?.GetText(ask).ok()?.to_string();
            out.push_str(&text);
            out.push('\n');
        }
        Some(out).filter(|t| !t.trim().is_empty())
    });
    Some(visible.unwrap_or(all))
}

/// `text` with its line ends made plain, the marks programs put where a
/// picture stands taken out, runs of empty lines made one, and no more
/// than `most` characters.
fn tidy(text: &str, most: usize) -> String {
    let mut out = String::new();
    let mut left = most;
    let mut empty = true;
    let plain = text.replace("\r\n", "\n").replace('\r', "\n");
    for line in plain.lines() {
        let line: String = line
            .chars()
            .filter(|c| *c != '\u{FFFC}' && (!c.is_control() || *c == '\t'))
            .collect();
        let line = line.trim_end();
        if line.trim().is_empty() {
            if !empty && left > 0 {
                out.push('\n');
                left -= 1;
            }
            empty = true;
            continue;
        }
        empty = false;
        let taken: String = line.chars().take(left.saturating_sub(1)).collect();
        left = left.saturating_sub(taken.chars().count() + 1);
        out.push_str(&taken);
        out.push('\n');
        if left == 0 {
            break;
        }
    }
    out.truncate(out.trim_end().len());
    out
}

/// What a walk has gathered so far, capped.
struct Gathered {
    text: String,
    left: usize,
    /// The lines already taken from names and values. Programs say the
    /// same thing in a row, its cells and the text inside each cell.
    said: HashSet<String>,
}

impl Gathered {
    fn new(most: usize) -> Gathered {
        Gathered {
            text: String::new(),
            left: most,
            said: HashSet::new(),
        }
    }

    fn full(&self) -> bool {
        self.left == 0
    }

    fn left(&self) -> usize {
        self.left
    }

    /// A document's own text, every line of it: a line that comes
    /// twice in a document is there twice.
    fn whole(&mut self, text: &str) {
        let tidy = tidy(text, self.left);
        if tidy.is_empty() {
            return;
        }
        self.left = self.left.saturating_sub(tidy.chars().count() + 1);
        for line in tidy.lines() {
            self.said.insert(line.trim().to_string());
        }
        self.text.push_str(&tidy);
        self.text.push('\n');
    }

    /// A part's name or value, unless a part above it said it already
    /// or it was said before in this reading.
    fn piece(&mut self, text: &str, within: Option<&str>) {
        let text = tidy(text, self.left);
        let text = text.trim();
        if text.is_empty() || within.is_some_and(|w| w.contains(text)) {
            return;
        }
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if self.left == 0 {
                return;
            }
            if !self.said.insert(line.to_string()) {
                continue;
            }
            let taken: String = line.chars().take(self.left).collect();
            self.left = self.left.saturating_sub(taken.chars().count() + 1);
            self.text.push_str(&taken);
            self.text.push('\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidy_makes_lines_plain() {
        assert_eq!(
            tidy("Dear Anna\r\n\r\n\r\n\r\nThe price\u{FFFC} is 12  \rBye\n\n", 1_000),
            "Dear Anna\n\nThe price is 12\nBye"
        );
        assert_eq!(tidy("\n\n  \n", 1_000), "");
        assert_eq!(tidy("a\tb\u{7}", 1_000), "a\tb");
    }

    #[test]
    fn tidy_stops_at_its_cap() {
        assert_eq!(tidy("abcdef\nghij", 4), "abc");
        assert_eq!(tidy("ab\ncdef", 6), "ab\ncd");
        assert_eq!(tidy("abc", 0), "");
        assert!(tidy(&"word ".repeat(10_000), 500).chars().count() <= 500);
    }

    #[test]
    fn a_piece_is_said_once() {
        let mut got = Gathered::new(1_000);
        got.piece("Inbox", None);
        got.piece("Anna, Offer, Monday", None);
        // The cells of that row, and the row again further down.
        got.piece("Anna", Some("Anna, Offer, Monday\n"));
        got.piece("Offer", Some("Anna, Offer, Monday\n"));
        got.piece("Anna, Offer, Monday", None);
        got.piece("  ", None);
        got.piece("Bo, Meeting, Tuesday", None);
        assert_eq!(got.text, "Inbox\nAnna, Offer, Monday\nBo, Meeting, Tuesday\n");
    }

    #[test]
    fn a_document_keeps_its_repeated_lines() {
        let mut got = Gathered::new(1_000);
        got.whole("fn a() {\n}\nfn b() {\n}\n");
        assert_eq!(got.text, "fn a() {\n}\nfn b() {\n}\n");
        // Its lines are known to the walk that goes on around it.
        got.piece("fn a() {", None);
        got.piece("Ln 3, Col 1", None);
        assert_eq!(got.text, "fn a() {\n}\nfn b() {\n}\nLn 3, Col 1\n");
    }

    #[test]
    fn gathering_stops_when_full() {
        let mut got = Gathered::new(10);
        got.piece("abcdef", None);
        assert!(!got.full());
        got.piece("ghijkl", None);
        assert!(got.full());
        got.piece("more", None);
        got.whole("and more");
        assert_eq!(got.text, "abcdef\ngh\n");
        assert_eq!(got.left(), 0);
    }
}
