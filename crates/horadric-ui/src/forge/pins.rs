//! The open pins of each file shown in the Forge, numbered as the page
//! draws them, until a session's next write to the file lands; and
//! handing a pin, or an error the page shows, to the session it is for.
//!
//! Runs on the UI thread, but for [`wrote`], which the hook feeder calls.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use horadric_core::forge::{pin_message, At, Pin, Rect};
use serde_json::{json, Value};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use crate::app::{push, Input};
use crate::menu::{self, Item};
use crate::web;

/// A pin the human made and the anchor the page gave for its rune.
struct Open {
    key: String,
    n: u32,
    pin: Pin,
    anchor: Value,
}

/// What goes to a session from a file's tab.
enum Gift {
    Pin(Pin),
    Text(String),
}

thread_local! {
    static OPEN: RefCell<Vec<Open>> = const { RefCell::new(Vec::new()) };
    /// File tabs in pin mode, as (project, tab).
    static PINNING: RefCell<Vec<(String, u64)>> = const { RefCell::new(Vec::new()) };
    /// Gifts waiting for the timer that hands them out, with their project
    /// and file.
    static WAITING: RefCell<Vec<(String, PathBuf, Gift)>> = const { RefCell::new(Vec::new()) };
}

/// The session that last wrote each file, by project: the one a pin on it
/// is most likely for.
static WRITERS: Mutex<Vec<(String, PathBuf, String)>> = Mutex::new(Vec::new());
/// Enough files for any day's work; the oldest is forgotten first.
const WRITERS_MAX: usize = 200;

/// The most of the human's words, an error's message and its stack that
/// go to a session. A page could post megabytes.
const WORDS_MAX: usize = 4000;
const STACK_MAX: usize = 3000;
/// An anchor is the page's to draw from; one bigger than this is not one.
const ANCHOR_MAX: usize = 4000;
/// Crops are for the turn they go with; a week covers looking back.
const CROPS_KEPT: Duration = Duration::from_secs(7 * 24 * 3600);

/// The open pins of the project `key`, of `file` alone when given, in
/// the order they were made.
pub fn list(key: &str, file: Option<&Path>) -> Vec<Pin> {
    OPEN.with(|o| {
        o.borrow()
            .iter()
            .filter(|p| p.key == key && file.is_none_or(|f| super::same(&p.pin.file, f)))
            .map(|p| p.pin.clone())
            .collect()
    })
}

/// Off the feeder thread: the session `id` wrote `file` of the project
/// `key`.
pub fn wrote(key: &str, file: &Path, id: &str) {
    let Ok(mut w) = WRITERS.lock() else { return };
    w.retain(|(k, f, _)| !(k == key && super::same(f, file)));
    if w.len() >= WRITERS_MAX {
        w.remove(0);
    }
    w.push((key.to_string(), file.to_path_buf(), id.to_string()));
}

fn writer_of(key: &str, file: &Path) -> Option<String> {
    let w = WRITERS.lock().ok()?;
    w.iter()
        .find(|(k, f, _)| k == key && super::same(f, file))
        .map(|(_, _, id)| id.clone())
}

/// A session's write to `file` landed: it was told what the pins said,
/// so they are done and their runes fade.
pub fn written(key: &str, file: &Path) {
    let had = OPEN.with(|o| {
        let mut o = o.borrow_mut();
        let before = o.len();
        o.retain(|p| !(p.key == key && super::same(&p.pin.file, file)));
        o.len() != before
    });
    if had {
        if let Some(tab) = super::tab_of(key, file) {
            super::post(key, tab, &json!({"forge": "clear-pins"}));
        }
    }
}

/// A pin taken back before it was sent (a chip's cross in the Folio).
pub fn unpin(pin: &Pin) {
    let gone = OPEN.with(|o| {
        let mut o = o.borrow_mut();
        let i = o.iter().position(|p| p.pin == *pin)?;
        Some(o.remove(i).key)
    });
    if let Some(key) = gone {
        runes(&key, &pin.file);
    }
}

/// Sends the page showing `file` its runes, all of them, numbered.
fn runes(key: &str, file: &Path) {
    let Some(tab) = super::tab_of(key, file) else {
        return;
    };
    let pins: Vec<Value> = OPEN.with(|o| {
        o.borrow()
            .iter()
            .filter(|p| p.key == key && super::same(&p.pin.file, file))
            .map(|p| {
                let mut v = p.anchor.clone();
                if !v.is_object() {
                    v = json!({});
                }
                v["n"] = json!(p.n);
                v
            })
            .collect()
    });
    super::post(key, tab, &json!({"forge": "pins", "pins": pins}));
}

/// Whether the shown tab of the project `key` is a file's in pin mode,
/// for the bar's pin button.
pub fn pinning(key: &str) -> bool {
    let Some(tab) = web::shown_tab(key) else {
        return false;
    };
    PINNING.with(|p| p.borrow().iter().any(|(k, t)| k == key && *t == tab))
}

/// The bar's pin button: pin mode on or off in the project's shown tab,
/// when it shows a file.
pub fn toggle_pinning(key: &str) {
    let Some(tab) = web::shown_tab(key) else {
        return;
    };
    if super::file_of(key, tab).is_none() {
        return;
    }
    let on = !pinning(key);
    set(key, tab, on);
    super::post(key, tab, &json!({"forge": "pinning", "on": on}));
}

fn set(key: &str, tab: u64, on: bool) {
    PINNING.with(|p| {
        let mut p = p.borrow_mut();
        p.retain(|(k, t)| !(k == key && *t == tab));
        if on {
            p.push((key.to_string(), tab));
        }
    });
}

/// The page turned pin mode on or off itself (the P key, Esc).
pub(super) fn page_pinning(key: &str, tab: u64, on: bool) {
    set(key, tab, on);
    web::redraw(key);
}

/// The page of the tab `tab` loaded: a reload must not lose the runes, nor
/// pin mode the human turned on.
pub(super) fn page_ready(key: &str, tab: u64) {
    let Some(file) = super::file_of(key, tab) else {
        return;
    };
    if !list(key, Some(&file)).is_empty() {
        runes(key, &file);
    }
    if PINNING.with(|p| p.borrow().iter().any(|(k, t)| k == key && *t == tab)) {
        super::post(key, tab, &json!({"forge": "pinning", "on": true}));
    }
}

/// A `pin` from the page: kept, drawn as a rune, cropped, and handed to
/// the session it is for.
pub(super) fn page_pin(key: &str, tab: u64, msg: &Value) {
    let Some(file) = super::file_of(key, tab) else {
        return;
    };
    let words = msg.get("words").and_then(Value::as_str).unwrap_or_default().trim();
    if words.is_empty() {
        return;
    }
    let words = clip(words, WORDS_MAX);
    let mut at: At = msg
        .get("at")
        .cloned()
        .and_then(|a| serde_json::from_value(a).ok())
        .unwrap_or_default();
    let anchor = msg
        .get("anchor")
        .filter(|a| a.is_object() && a.to_string().len() <= ANCHOR_MAX)
        .cloned()
        .unwrap_or_else(|| json!({}));
    let shown = rect_of(msg.get("rect"));
    // In the page, where the box is in the document, which is what the
    // crop takes; the viewport box moves with the scroll.
    let in_doc = rect_of(anchor.get("rect")).or_else(|| {
        let (x, y) = super::scroll_of(key, tab).unwrap_or_default();
        shown.map(|r| Rect { x: r.x + x, y: r.y + y, ..r })
    });
    // An image's own pixels say more than where it sat on the screen.
    at.rect = msg
        .pointer("/at/image")
        .and_then(|i| rect_of(Some(i)))
        .or(in_doc);
    let pin = Pin {
        file: file.clone(),
        words,
        at,
        crop: None,
    };
    let n = OPEN.with(|o| {
        let mut o = o.borrow_mut();
        let n = (o.iter())
            .filter(|p| p.key == key && super::same(&p.pin.file, &file))
            .map(|p| p.n)
            .max()
            .unwrap_or(0)
            + 1;
        o.push(Open {
            key: key.to_string(),
            n,
            pin: pin.clone(),
            anchor,
        });
        n
    });
    runes(key, &file);
    let Some(r) = in_doc.filter(|r| r.w >= 1.0 && r.h >= 1.0) else {
        give(key, &file, Gift::Pin(pin));
        return;
    };
    let params = crate::pick::crop_params([r.x, r.y, r.w, r.h]);
    let owned = key.to_string();
    web::devtools(key, tab, "Page.captureScreenshot", &params, move |shot| {
        web::forget_call(&owned, tab);
        let crop = shot.ok().and_then(|s| save_crop(&s));
        let mut pin = pin;
        if let Some(c) = crop {
            pin.crop = Some(c.clone());
            OPEN.with(|o| {
                let mut o = o.borrow_mut();
                let kept = o
                    .iter_mut()
                    .find(|p| p.key == owned && p.n == n && super::same(&p.pin.file, &file));
                if let Some(p) = kept {
                    p.pin.crop = Some(c);
                }
            });
        }
        give(&owned, &file, Gift::Pin(pin));
    });
}

/// "Send to session" on an error card the page shows.
pub(super) fn page_send_error(key: &str, tab: u64, msg: &Value) {
    let Some(file) = super::file_of(key, tab) else {
        return;
    };
    let field = |k: &str| msg.get(k).and_then(Value::as_str).unwrap_or_default().trim();
    let message = clip(field("message"), WORDS_MAX);
    if message.is_empty() {
        return;
    }
    let stack = clip(field("stack"), STACK_MAX);
    let root = root_of(key, tab).unwrap_or_default();
    let name = shown_path(&file, &root);
    let mut text = format!("This file shows an error: {name}\n\n{message}\n");
    if !stack.is_empty() && stack != message {
        text.push_str(&format!("\n{stack}\n"));
    }
    give(key, &file, Gift::Text(text));
}

fn rect_of(v: Option<&Value>) -> Option<Rect> {
    let v = v?;
    let n = |k: &str| v.get(k).and_then(Value::as_f64).filter(|n| n.is_finite());
    Some(Rect {
        x: n("x")?,
        y: n("y")?,
        w: n("w")?.max(0.0),
        h: n("h")?.max(0.0),
    })
}

fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}...", &s[..i]),
        None => s.to_string(),
    }
}

fn root_of(key: &str, tab: u64) -> Option<PathBuf> {
    super::TABS.with(|t| {
        t.borrow()
            .iter()
            .find(|f| f.key == key && f.id == tab)
            .map(|f| f.root.clone())
    })
}

/// `file` as a session in the project reads it best: relative to `root`.
fn shown_path(file: &Path, root: &Path) -> String {
    match file.strip_prefix(root) {
        Ok(rel) if !root.as_os_str().is_empty() => rel.to_string_lossy().replace('\\', "/"),
        _ => file.display().to_string(),
    }
}

/// Where crops are kept: the app's own folder, as a session reads them
/// later by path.
fn crops_dir() -> Option<PathBuf> {
    horadric_hooks::state_dir().map(|d| d.join("forge").join("crops"))
}

/// Writes the picture in a `Page.captureScreenshot` answer, and forgets
/// crops older than a week.
fn save_crop(answer: &str) -> Option<PathBuf> {
    let v: Value = serde_json::from_str(answer).ok()?;
    let png = horadric_core::release::base64_decode(v.get("data")?.as_str()?)?;
    let dir = crops_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let now = SystemTime::now();
        for e in entries.flatten() {
            let old = (e.metadata().and_then(|m| m.modified()).ok())
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age > CROPS_KEPT);
            if old {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let path = dir.join(format!("pin-{stamp}.png"));
    std::fs::write(&path, png).ok()?;
    Some(path)
}

/// Queues `gift` for the session it is for. Handed out from a timer, not
/// here: this runs inside WebView2's callbacks, where a picker's menu
/// loop has no business, and the app may be busy.
fn give(key: &str, file: &Path, gift: Gift) {
    WAITING.with(|w| {
        w.borrow_mut()
            .push((key.to_string(), file.to_path_buf(), gift))
    });
    unsafe {
        SetTimer(None, 0, 0, Some(hand_out));
    }
}

unsafe extern "system" fn hand_out(_: HWND, _: u32, id: usize, _: u32) {
    let _ = KillTimer(None, id);
    let all = WAITING.with(|w| std::mem::take(&mut *w.borrow_mut()));
    for (key, file, gift) in all {
        match session_for(&key, &file) {
            Some((id, folio)) => hand(&key, &id, folio.as_deref(), gift),
            // The pin stays open, drawn and listed for `forge_pins`.
            None => eprintln!("horadric: forge pin in {key} has no session to go to"),
        }
    }
}

/// Which session a pin on `file` goes to: the one that last wrote it, else
/// the one that last had the keyboard, else the only one; with several
/// and no clue, the human picks. Its id and its Folio's key when it is
/// one.
fn session_for(key: &str, file: &Path) -> Option<(String, Option<String>)> {
    let (agents, known) = crate::app::forge_agents(key);
    let writer = writer_of(key, file);
    let chosen = if let Some(a) = writer.and_then(|w| agents.iter().find(|a| a.0 == w)) {
        a
    } else if agents.len() == 1 || (known && !agents.is_empty()) {
        &agents[0]
    } else if agents.is_empty() {
        return None;
    } else {
        const FIRST: usize = 1;
        let mut items = vec![Item::Disabled("Send the pin to".into())];
        items.extend(
            (agents.iter().enumerate()).map(|(i, a)| Item::action(FIRST + i, a.1.clone())),
        );
        let picked = menu::popup(&items)?;
        agents.get(picked.checked_sub(FIRST)?)?
    };
    Some((chosen.0.clone(), chosen.2.clone()))
}

fn hand(key: &str, id: &str, folio: Option<&str>, gift: Gift) {
    match (gift, folio) {
        // A Folio gathers pins as chips, to go with the next message.
        (Gift::Pin(pin), Some(f)) => {
            crate::folio::pinned(f, pin);
            push(Input::Hand(id.to_string(), String::new()));
        }
        (Gift::Pin(pin), None) => {
            let root = super::tab_of(key, &pin.file)
                .and_then(|t| root_of(key, t))
                .unwrap_or_default();
            push(Input::Hand(id.to_string(), pin_message(&[pin], &root)));
        }
        (Gift::Text(text), _) => push(Input::Hand(id.to_string(), text)),
    }
}
