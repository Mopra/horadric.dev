//! Hand edits from a file tab's page: text changed in place, slides
//! reordered, duplicated or deleted, an image cropped and rotated (step 6
//! of docs/plan/the-forge.md), and the source view.
//!
//! The page says what changed; this finds it in the source and writes the
//! file in one go on a thread of its own, never while a session's write to
//! the same file is between its `PreToolUse` and `PostToolUse`. A text edit
//! that cannot be put back exactly is answered so the page sends it as a
//! pin instead, and Claude makes it.

mod html;
mod page;
mod text;
mod write;

pub use page::{on_render, on_save_image};

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use horadric_core::forge::Kind;
use horadric_core::HookEvent;
use serde_json::{json, Value};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2, ICoreWebView2WebResourceRequestedEventArgs, ICoreWebView2_2,
    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT,
};
use webview2_com::WebResourceRequestedEventHandler;
use windows::core::{Interface, HSTRING, PWSTR};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::SHCreateMemStream;

/// How long a session's write may hold a hand edit back. A write waiting
/// on the human's permission never ends while they edit, so past this the
/// hand edit goes first and Claude's `Edit` fails loudly and reads again.
const WAIT_MAX: Duration = Duration::from_secs(20);

/// The most text a page may send as one edit.
const TEXT_MAX: usize = 200_000;

/// More slides than any deck has. A page's count is only a claim, and one
/// near `u64::MAX` must not overflow the check against the new order.
const SLIDES_MAX: usize = 10_000;

/// The largest file a page's editor may write whole. A deck with its
/// pictures inline runs to megabytes; past this it is not text to edit.
const WRITE_MAX: usize = 8 << 20;

/// How long after edit mode ends a page may still write: the editor saves
/// what was left as it closes, and that message can follow the one saying
/// it closed.
const WRITE_AFTER: Duration = Duration::from_secs(5);

/// How long a page's own write is remembered, so the watcher's report of
/// it is known as the page's and does not load the page again under the
/// human's hands.
const OWN_FOR: Duration = Duration::from_secs(30);

/// A page's write, by the bytes it put on disk.
struct Own {
    file: PathBuf,
    hash: u64,
    key: String,
    tab: u64,
    at: Instant,
}

/// Pages' writes lately. Several per file: the writer may have put the
/// next before the watcher reports the last.
static OWN: Mutex<Vec<Own>> = Mutex::new(Vec::new());

fn hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// The tabs whose page wrote what `file` holds now, none when a session
/// or a program did. Reads the file only when a page wrote it lately.
pub(super) fn written_by(file: &Path) -> Vec<(String, u64)> {
    let lately: Vec<(u64, String, u64)> = OWN.lock().map_or_else(
        |_| Vec::new(),
        |mut o| {
            o.retain(|w| w.at.elapsed() < OWN_FOR);
            (o.iter())
                .filter(|w| super::same(&w.file, file))
                .map(|w| (w.hash, w.key.clone(), w.tab))
                .collect()
        },
    );
    if lately.is_empty() {
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(file) else { return Vec::new() };
    let now = hash(&bytes);
    let mut tabs: Vec<(String, u64)> = Vec::new();
    for (h, key, tab) in lately {
        if h == now && !tabs.iter().any(|t| t.0 == key && t.1 == tab) {
            tabs.push((key, tab));
        }
    }
    tabs
}

/// What `file` holds as text for a page's editor, None when it is too
/// large or not UTF-8.
pub(super) fn text_of(file: &Path) -> Option<String> {
    let small = std::fs::metadata(file).is_ok_and(|m| m.len() <= WRITE_MAX as u64);
    if !small {
        return None;
    }
    String::from_utf8(std::fs::read(file).ok()?).ok()
}

/// Writes by sessions in flight: the file, the tool call and when it began.
static WRITING: Mutex<Vec<(PathBuf, Option<String>, Instant)>> = Mutex::new(Vec::new());

/// Off the feeder thread, every hook event: a session's write to a file
/// begins or ends.
pub fn hooked(event: &HookEvent) {
    let begins = match event.hook_event_name.as_str() {
        "PreToolUse" => true,
        "PostToolUse" | "PostToolUseFailure" => false,
        _ => return,
    };
    let Some(tool) = event.tool_name.as_deref() else { return };
    if !horadric_core::rarity::WRITERS.contains(&tool) {
        return;
    }
    let Some(input) = event.tool_input.as_ref() else { return };
    let path = input
        .get("file_path")
        .or_else(|| input.get("notebook_path"))
        .and_then(Value::as_str);
    let Some(path) = path else { return };
    let file = PathBuf::from(horadric_core::glow::full(path, &event.cwd).replace('/', "\\"));
    let call = event.tool_use_id.clone();
    let Ok(mut w) = WRITING.lock() else { return };
    w.retain(|e| e.2.elapsed() < WAIT_MAX);
    if begins {
        w.push((file, call, Instant::now()));
    } else if let Some(at) = w
        .iter()
        .position(|e| (call.is_some() && e.1 == call) || (call.is_none() && super::same(&e.0, &file)))
    {
        w.remove(at);
    }
}

/// Whether a session's write to `file` is in flight, so a write of ours
/// waits for it rather than landing in the middle.
pub(super) fn being_written(file: &Path) -> bool {
    WRITING.lock().is_ok_and(|w| busy(&w, file))
}

fn busy(w: &[(PathBuf, Option<String>, Instant)], file: &Path) -> bool {
    w.iter().any(|e| e.2.elapsed() < WAIT_MAX && super::same(&e.0, file))
}

/// What the writer thread does to a file.
enum Change {
    /// Text of a markdown block, by its lines.
    Lines { a: usize, b: usize, old: String, new: String },
    /// Text of an HTML element, by its content's byte range.
    Element { inner: (usize, usize), old: String, new: String },
    /// A deck's slides, as the current ones by number.
    /// `blank`: the doubled one's copy is a new empty slide of its layout.
    Slides { count: usize, order: Vec<usize>, blank: bool },
    /// An image's new bytes.
    Image(Vec<u8>),
    /// The whole file from the page's editor, if it still holds `base`.
    Whole { base: String, text: String },
}

struct Job {
    key: String,
    tab: u64,
    /// The page's id for the edit, given back in the answer.
    id: Value,
    what: &'static str,
    file: PathBuf,
    change: Change,
}

/// Answers for pages, waiting for the UI thread.
static DONE: Mutex<Vec<(String, u64, Value)>> = Mutex::new(Vec::new());

fn writer() -> Option<Sender<Job>> {
    static TX: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();
    let tx = TX.get_or_init(|| {
        let (tx, rx) = channel::<Job>();
        let _ = std::thread::Builder::new()
            .name("forge-edit".into())
            .spawn(move || {
                for job in rx {
                    let answer = run(&job);
                    let mut msg = if job.what == "write" {
                        json!({"forge": "write-result", "id": job.id})
                    } else {
                        json!({"forge": "edit-result", "what": job.what, "id": job.id})
                    };
                    match answer {
                        Ok(html) => {
                            msg["ok"] = json!(true);
                            if let Some(html) = html {
                                msg["html"] = json!(html);
                            }
                        }
                        Err(Failed::NotExact) => {
                            msg["ok"] = json!(false);
                            msg["pin"] = json!(true);
                        }
                        Err(Failed::Changed(current)) => {
                            msg["ok"] = json!(false);
                            msg["why"] = json!("changed");
                            msg["current"] = json!(current);
                        }
                        Err(Failed::Why(why)) => {
                            msg["ok"] = json!(false);
                            msg["why"] = json!(why);
                        }
                    }
                    if let Ok(mut d) = DONE.lock() {
                        d.push((job.key, job.tab, msg));
                    }
                    super::wake();
                }
            });
        Mutex::new(tx)
    });
    tx.lock().ok().map(|t| t.clone())
}

enum Failed {
    /// The page's text is not what the source says: a pin instead.
    NotExact,
    /// The file is no longer what the editor started from: what it holds.
    Changed(String),
    Why(String),
}

/// On the writer thread: waits out a session's write, then reads, changes
/// and writes the file, holding the lock so no write is heard beginning
/// halfway. A markdown file written whole answers with its page's body.
fn run(job: &Job) -> Result<Option<String>, Failed> {
    let started = Instant::now();
    loop {
        let w = WRITING.lock().map_err(|_| Failed::Why("lock".into()))?;
        if busy(&w, &job.file) && started.elapsed() < WAIT_MAX {
            drop(w);
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        let bytes = std::fs::read(&job.file).map_err(|e| Failed::Why(format!("cannot read it: {e}")))?;
        if let Change::Whole { base, text } = &job.change {
            return run_whole(job, &bytes, base, text);
        }
        let out = match &job.change {
            Change::Image(new) => new.clone(),
            change => {
                let src = String::from_utf8(bytes.clone())
                    .map_err(|_| Failed::Why("it is not UTF-8 text".into()))?;
                match changed(&src, change)? {
                    Some(s) => s.into_bytes(),
                    None => return Ok(None),
                }
            }
        };
        if out == bytes {
            return Ok(None);
        }
        // One write of the whole file, so a session reading it sees the
        // old or the new, and its next `Edit` sees the change.
        std::fs::write(&job.file, &out).map_err(|e| Failed::Why(format!("cannot write it: {e}")))?;
        return Ok(None);
    }
}

/// The editor's whole file over `bytes`, the file as it is, when that is
/// still the text the edit began from.
fn run_whole(job: &Job, bytes: &[u8], base: &str, text: &str) -> Result<Option<String>, Failed> {
    let disk = std::str::from_utf8(bytes).map_err(|_| Failed::Why("it is not UTF-8 text".into()))?;
    if !write::unchanged(disk, base) {
        return Err(Failed::Changed(write::normalise(disk).into_owned()));
    }
    let out = write::bytes(disk, text);
    if out != bytes {
        remember(job, &out);
        replace(&job.file, &out).map_err(|e| Failed::Why(format!("cannot write it: {e}")))?;
    }
    let markdown = super::kind(&job.file) == Some(Kind::Markdown);
    Ok(markdown.then(|| horadric_core::forge::markdown_body(text)))
}

/// Notes the page's write before it lands, so however soon the watcher
/// reports it, it is known as the page's own.
fn remember(job: &Job, out: &[u8]) {
    let Ok(mut o) = OWN.lock() else { return };
    o.retain(|w| w.at.elapsed() < OWN_FOR);
    o.push(Own {
        file: job.file.clone(),
        hash: hash(out),
        key: job.key.clone(),
        tab: job.tab,
        at: Instant::now(),
    });
}

/// `file` replaced by `bytes` in one step: written beside it, then moved
/// over it, so a session or the watcher never reads half a file. When the
/// move is refused (another program holds the file open without letting it
/// be replaced), it is written in place instead.
fn replace(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let beside = file.with_file_name(format!(".{name}.forge-write"));
    std::fs::write(&beside, bytes)?;
    if std::fs::rename(&beside, file).is_ok() {
        return Ok(());
    }
    let _ = std::fs::remove_file(&beside);
    std::fs::write(file, bytes)
}

/// The source after `change`, None when it is the same.
fn changed(src: &str, change: &Change) -> Result<Option<String>, Failed> {
    match change {
        Change::Lines { a, b, old, new } => {
            text::edit_markdown(src, *a, *b, old, new).map_err(|()| Failed::NotExact)
        }
        Change::Element { inner, old, new } => {
            // The range must still be an element's content in the file as
            // it is now, not just any bytes a stale page remembers.
            if !html::elements(src).iter().any(|e| e.inner == *inner) {
                return Err(Failed::NotExact);
            }
            text::edit_html(src, *inner, old, new).map_err(|()| Failed::NotExact)
        }
        Change::Slides { count, order, blank } => {
            let now = html::slides(src).len();
            if now != *count {
                return Err(Failed::Why(format!(
                    "the deck has {now} slides now, the page showed {count}; try again after it reloads"
                )));
            }
            let out = if *blank { html::add_after(src, order) } else { html::reorder(src, order) };
            out.map(Some)
                .ok_or_else(|| Failed::Why("that order of slides cannot be written".into()))
        }
        Change::Image(_) | Change::Whole { .. } => Ok(None),
    }
}

/// On the UI thread, woken by the writer: its answers go to their pages.
pub fn drain() {
    let done = DONE.lock().map(|mut d| std::mem::take(&mut *d)).unwrap_or_default();
    for (key, tab, msg) in done {
        answer(&key, tab, &msg);
    }
}

/// Posts an edit's answer. One that asks the page to send the edit as a
/// pin lets that one pin through, though the tab is not in pin mode.
fn answer(key: &str, tab: u64, msg: &Value) {
    if msg.get("pin").and_then(Value::as_bool) == Some(true) {
        super::pins::allow_pin(key, tab);
    }
    super::post(key, tab, msg);
}

/// The file the tab `tab` of `key` shows when the page's `file`
/// (its `location.pathname`) is that file, with the tab's folder.
fn own_file(key: &str, tab: u64, page_file: &str) -> Option<PathBuf> {
    let (file, root, host) = super::TABS.with(|t| {
        t.borrow()
            .iter()
            .find(|f| f.key == key && f.id == tab)
            .map(|f| (f.file.clone(), f.root.clone(), f.host.clone()))
    })?;
    // A page may only change itself: the tab can be led to another file
    // of the folder, and a page could name any.
    let url = format!("https://{host}{page_file}");
    let (_, named) = super::serve::parse_url(&url, |_| Some(root.clone()))?;
    super::same(&named, &file).then_some(file)
}

fn string(msg: &Value, k: &str) -> Option<String> {
    msg.get(k)
        .and_then(Value::as_str)
        .filter(|s| s.len() <= TEXT_MAX)
        .map(str::to_string)
}

fn pair(msg: &Value, k: &str) -> Option<(usize, usize)> {
    let v = msg.get(k)?.as_array()?;
    let n = |i: usize| v.get(i)?.as_u64().and_then(|x| usize::try_from(x).ok());
    let (a, b) = (n(0)?, n(1)?);
    (a <= b).then_some((a, b))
}

/// The kinds of file the page's editor writes whole.
pub(super) fn editable(file: &Path) -> bool {
    matches!(super::kind(file), Some(Kind::Markdown | Kind::Html | Kind::Deck | Kind::Csv))
}

/// The kinds of file whose page is always the editor: no edit mode, the
/// view itself takes the caret.
pub(super) fn always_editable(file: &Path) -> bool {
    matches!(super::kind(file), Some(Kind::Markdown | Kind::Csv))
}

/// How long after the human's last key or click a page that is always
/// the editor may still write its file: past the autosave's wait, yet
/// short enough that a page's own script cannot rewrite it unattended.
const HUMAN_FOR: Duration = Duration::from_secs(10);

/// Whether the tab shows the live file of a kind that is always edited (a
/// version from the history is only shown).
fn always_editing(key: &str, tab: u64) -> bool {
    super::file_of(key, tab).is_some_and(|f| always_editable(&f))
        && super::history::viewing(key, tab).is_none()
}

thread_local! {
    /// Tabs in edit mode, and tabs that left it and when, for the save
    /// that follows the leaving.
    static EDITING: RefCell<Vec<(String, u64, Option<Instant>)>> = const { RefCell::new(Vec::new()) };
}

/// Whether the tab's page is in edit mode.
pub(super) fn editing(key: &str, tab: u64) -> bool {
    always_editing(key, tab)
        || EDITING.with(|e| e.borrow().iter().any(|(k, t, off)| k == key && *t == tab && off.is_none()))
}

/// Whether the tab's page may write its file: in edit mode, or only just
/// out of it.
fn may_write(key: &str, tab: u64) -> bool {
    if always_editing(key, tab) {
        return crate::web::shown_tab(key) == Some(tab) && super::pins::human_within(key, HUMAN_FOR);
    }
    EDITING.with(|e| {
        e.borrow().iter().any(|(k, t, off)| {
            k == key && *t == tab && off.is_none_or(|at| at.elapsed() < WRITE_AFTER)
        })
    })
}

fn set_editing(key: &str, tab: u64, on: bool) {
    EDITING.with(|e| {
        let mut e = e.borrow_mut();
        let was = e.iter().any(|(k, t, off)| k == key && *t == tab && off.is_none());
        e.retain(|(k, t, off)| !(k == key && *t == tab) && off.is_none_or(|at| at.elapsed() < WRITE_AFTER));
        if on {
            e.push((key.to_string(), tab, None));
        } else if was {
            e.push((key.to_string(), tab, Some(Instant::now())));
        }
    });
}

/// The tab's page loaded again or shows another file: whatever edit mode
/// it was in went with the old page.
pub(super) fn page_gone(key: &str, tab: u64) {
    let was = editing(key, tab);
    EDITING.with(|e| e.borrow_mut().retain(|(k, t, _)| !(k == key && *t == tab)));
    if was {
        crate::web::redraw(key);
    }
}

/// The bar's Edit button: edit mode on or off in the project's shown tab.
pub fn toggle_editing(key: &str) {
    let Some(tab) = crate::web::shown_tab(key) else { return };
    let Some(file) = super::file_of(key, tab) else { return };
    if !editable(&file) || always_editable(&file) {
        return;
    }
    let on = !editing(key, tab);
    set_editing(key, tab, on);
    super::post(key, tab, &json!({"forge": "editing", "on": on}));
    crate::web::redraw(key);
}

/// The page turned edit mode on or off itself (the E key). As with pin
/// mode, on only with the human at the keys: edit mode is what lets a page
/// write its file, and a page's own script must not turn it on unbidden.
pub(super) fn page_editing(key: &str, tab: u64, on: bool) {
    if always_editing(key, tab) {
        return;
    }
    let fits = super::file_of(key, tab).is_some_and(|f| editable(&f));
    let on = on && fits && crate::web::shown_tab(key) == Some(tab) && super::pins::human_at(key);
    set_editing(key, tab, on);
    if !on {
        super::post(key, tab, &json!({"forge": "editing", "on": false}));
    }
    crate::web::redraw(key);
}

/// A `forge: "write"` from the page's editor: the whole file, written when
/// it is still what the edit began from.
pub fn on_write(key: &str, tab: u64, msg: &Value) {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let refuse = |why: &str| {
        super::post(key, tab, &json!({"forge": "write-result", "id": id, "ok": false, "why": why}));
    };
    if !may_write(key, tab) {
        if always_editing(key, tab) {
            return refuse("no key or click in the page lately; type again to save");
        }
        return refuse("the page is not in edit mode");
    }
    let page_file = msg.get("file").and_then(Value::as_str).unwrap_or_default();
    let Some(file) = own_file(key, tab, page_file) else {
        return refuse("a page can only change its own file");
    };
    if !editable(&file) {
        return refuse("this kind of file is not edited as text");
    }
    let whole = |k: &str| {
        msg.get(k)
            .and_then(Value::as_str)
            .filter(|s| s.len() <= WRITE_MAX)
            .map(str::to_string)
    };
    let (Some(base), Some(text)) = (whole("base"), whole("text")) else {
        return refuse("the write is missing its text, or it is too large");
    };
    let change = Change::Whole { base, text };
    let job = Job { key: key.to_string(), tab, id: id.clone(), what: "write", file, change };
    if writer().is_none_or(|tx| tx.send(job).is_err()) {
        refuse("the Forge's writer is not running");
    }
}

/// A `forge: "edit"`, `"reorder"` or `"crop"` message from the page of
/// the tab `tab` of the project `key`, after its origin was checked.
pub fn on_message(key: &str, tab: u64, msg: &Value) {
    let what: &'static str = match msg.get("forge").and_then(Value::as_str) {
        Some("edit") => "edit",
        Some("reorder") => "reorder",
        Some("crop") => "crop",
        _ => return,
    };
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let refuse = |why: &str| {
        super::post(key, tab, &json!({"forge": "edit-result", "what": what, "id": id, "ok": false, "why": why}));
    };
    let page_file = msg.get("file").and_then(Value::as_str).unwrap_or_default();
    let Some(file) = own_file(key, tab, page_file) else {
        return refuse("a page can only change its own file");
    };
    let kind = super::kind(&file);
    let change = match (what, kind) {
        ("edit", Some(Kind::Markdown)) => {
            let (Some((a, b)), Some(old), Some(new)) = (pair(msg, "lines"), string(msg, "old"), string(msg, "new")) else {
                return refuse("the edit is missing its lines or text");
            };
            Change::Lines { a, b, old, new }
        }
        ("edit", Some(Kind::Html | Kind::Deck)) => {
            let (Some(inner), Some(old), Some(new)) = (pair(msg, "offsets"), string(msg, "old"), string(msg, "new")) else {
                // No offsets: the element is not in the file as written.
                return answer(key, tab, &json!({"forge": "edit-result", "what": what, "id": id, "ok": false, "pin": true}));
            };
            Change::Element { inner, old, new }
        }
        ("reorder", Some(Kind::Html | Kind::Deck)) => {
            let count = msg.get("count").and_then(Value::as_u64).unwrap_or(0);
            let Some(count) = usize::try_from(count).ok().filter(|&c| c <= SLIDES_MAX) else {
                return refuse("the deck has more slides than can be reordered");
            };
            // Checked before collecting, so a page cannot make us hold a
            // huge list: the order is at most every slide and one doubled.
            let order: Option<Vec<usize>> = (msg.get("order").and_then(Value::as_array))
                .filter(|a| !a.is_empty() && a.len() <= count.saturating_add(1))
                .map(|a| {
                    (a.iter())
                        .filter_map(|v| v.as_u64().and_then(|x| usize::try_from(x).ok()))
                        .collect()
                });
            match order {
                Some(order) if !order.is_empty() => {
                    let blank = msg.get("blank").and_then(Value::as_bool).unwrap_or(false);
                    Change::Slides { count, order, blank }
                }
                _ => return refuse("the new order of slides is missing"),
            }
        }
        ("crop", Some(Kind::Image)) => match image_bytes(&file, msg) {
            Ok(bytes) => Change::Image(bytes),
            Err(why) => return refuse(&why),
        },
        _ => return refuse("this kind of file cannot be changed that way"),
    };
    let job = Job { key: key.to_string(), tab, id: id.clone(), what, file, change };
    if writer().is_none_or(|tx| tx.send(job).is_err()) {
        refuse("the Forge's writer is not running");
    }
}

/// The new image a crop sent, checked to be the file's own format.
fn image_bytes(file: &Path, msg: &Value) -> Result<Vec<u8>, String> {
    let data = msg.get("data").and_then(Value::as_str).ok_or("the crop has no image")?;
    let (head, body) = data.split_once(',').ok_or("the crop's image is not a data address")?;
    if !head.ends_with(";base64") {
        return Err("the crop's image is not base64".into());
    }
    if body.len() > 80 << 20 {
        return Err("the cropped image is too big".into());
    }
    let bytes = horadric_core::release::base64_decode(body).ok_or("the crop's image is not base64")?;
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let fits = match ext.as_str() {
        "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "jpg" | "jpeg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "webp" => bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        _ => return Err(format!("a .{ext} cannot be cropped here")),
    };
    if !fits {
        return Err(format!("the cropped image is not a .{ext}"));
    }
    Ok(bytes)
}

/// Opens the file the project's shown tab shows as text to edit, in the
/// editor Ctrl+click on a file opens. False when the tab shows no file.
pub fn source_view(key: &str) -> bool {
    let Some(id) = crate::web::shown_tab(key) else { return false };
    let found = super::TABS.with(|t| {
        t.borrow()
            .iter()
            .find(|f| f.key == key && f.id == id)
            .map(|f| (f.root.clone(), f.file.clone()))
    });
    let Some((root, file)) = found else { return false };
    let r = root.to_string_lossy().replace('/', "\\");
    let f = file.to_string_lossy().replace('/', "\\");
    let Some(rel) = f.get(r.trim_end_matches('\\').len()..) else { return false };
    crate::watch::open(&root, &rel.trim_start_matches('\\').replace('\\', "/"));
    true
}

/// Serves the tab's HTML pages with each element marked with where its
/// content is in the file ([`html::annotate`]), so a text edit on the page
/// finds its bytes. Called once as the tab's view is readied, beside the
/// folder mapping, which still serves everything else.
pub fn serve(view: &ICoreWebView2, host: &str, root: &Path) {
    unsafe {
        for ext in ["html", "htm"] {
            for pattern in [format!("https://{host}/*.{ext}"), format!("https://{host}/*.{ext}?*")] {
                let _ = view.AddWebResourceRequestedFilter(
                    &HSTRING::from(pattern),
                    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT,
                );
            }
        }
        let root = root.to_path_buf();
        let handler = WebResourceRequestedEventHandler::create(Box::new(move |view, args| {
            if let (Some(view), Some(args)) = (view, args) {
                marked(&view, &args, &root);
            }
            Ok(())
        }));
        let mut token = Default::default();
        let _ = view.add_WebResourceRequested(&handler, &mut token);
    }
}

/// The largest page that is marked; past it the page is served as is and
/// a text edit becomes a pin.
pub(super) const MARK_MAX: usize = 8 << 20;

fn marked(view: &ICoreWebView2, args: &ICoreWebView2WebResourceRequestedEventArgs, root: &Path) {
    let Ok(request) = (unsafe { args.Request() }) else { return };
    let url = read(|p| unsafe { request.Uri(p) });
    let path = url.split(['?', '#']).next().unwrap_or_default().to_ascii_lowercase();
    // The Forge's own pages (the artefact runtime) are not files.
    if path.contains("/__forge/") || !(path.ends_with(".html") || path.ends_with(".htm")) {
        return;
    }
    let Some((_, file)) = super::serve::parse_url(&url, |_| Some(root.to_path_buf())) else {
        return;
    };
    // Asked before reading, so a huge page is never read only to be
    // served as is.
    let small = std::fs::metadata(&file).is_ok_and(|m| m.len() <= MARK_MAX as u64);
    if !small {
        return;
    }
    let Ok(bytes) = std::fs::read(&file) else { return };
    if bytes.len() > MARK_MAX {
        return;
    }
    // A page in another encoding is served as is: offsets into a
    // re-encoded copy would not be the file's.
    let Ok(src) = String::from_utf8(bytes) else { return };
    let body = html::annotate(&src);
    unsafe {
        let Ok(env) = view.cast::<ICoreWebView2_2>().and_then(|v| v.Environment()) else { return };
        let Some(stream) = SHCreateMemStream(Some(body.as_bytes())) else { return };
        let headers = "Content-Type: text/html; charset=utf-8\r\nCache-Control: no-store";
        if let Ok(response) =
            env.CreateWebResourceResponse(&stream, 200, &HSTRING::from("OK"), &HSTRING::from(headers))
        {
            let _ = args.SetResponse(&response);
        }
    }
}

/// A string WebView2 hands out, freed after it is copied.
fn read(get: impl FnOnce(*mut PWSTR) -> windows::core::Result<()>) -> String {
    let mut p = PWSTR::null();
    if get(&mut p).is_err() || p.is_null() {
        return String::new();
    }
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    s
}
