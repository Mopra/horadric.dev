//! The Forge: files Claude and the human work on, shown in the browser
//! pane as tabs of their own (docs/plan/the-forge.md).
//!
//! A Forge tab is a browser tab whose page is a file. The browser keeps
//! the tab; this module knows which tabs are files, serves their folder to
//! them read only (`serve`), watches the files so a save shows at once
//! (`watch`), routes what the page says back (`message`) and draws the
//! pane's bar for them (`bar`).
//!
//! Everything here but the watcher's thread and [`heard`] runs on the UI
//! thread, as the browser does.

pub mod bar;
mod edit;
mod export;
mod history;
mod home;
pub mod message;
mod office;
pub mod pins;
mod present;
mod runtime;
mod serve;
mod settings;
pub mod tools;
mod watch;

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::Mutex;

use horadric_core::forge::Kind;
use horadric_core::{HookEvent, Registry};
use serde_json::Value;
use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2;
use webview2_com::ContentLoadingEventHandler;
use windows::core::{HSTRING, PWSTR};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::web;

pub use edit::source_view;
pub use settings::{auto, set_auto};

/// A browser tab that shows a file.
struct FileTab {
    key: String,
    id: u64,
    file: PathBuf,
    /// The folder served to it, the project's or, for a file outside the
    /// project, the file's own without its subfolders.
    root: PathBuf,
    /// The made up host the folder is served at.
    host: String,
    /// Where the page was scrolled to, given back after a reload.
    scroll: Option<(f64, f64)>,
    /// `file` is a folder, shown as the Forge's page of it: no pins, no
    /// history, no edits, only its own messages.
    folder: bool,
}

thread_local! {
    static TABS: RefCell<Vec<FileTab>> = const { RefCell::new(Vec::new()) };
}

/// The app's window and the message that wakes it to read its input, so
/// the watcher's and the hooks' threads can wake the UI thread.
static WAKE_WINDOW: AtomicIsize = AtomicIsize::new(0);
static WAKE_MSG: AtomicU32 = AtomicU32::new(0);

/// Files sessions wrote, by project, waiting for the UI thread.
static WRITTEN: Mutex<Vec<(String, PathBuf)>> = Mutex::new(Vec::new());

/// The files sessions wrote lately, by project, the newest last: the
/// Forge's home and the new tab page list them, read on any thread.
static RECENT: Mutex<Vec<(String, PathBuf)>> = Mutex::new(Vec::new());

/// How many written files [`RECENT`] keeps, across every project.
const RECENT_KEPT: usize = 64;

/// Called once from the UI thread as the app starts.
pub fn init(window: isize, msg: u32) {
    WAKE_WINDOW.store(window, Ordering::Release);
    WAKE_MSG.store(msg, Ordering::Release);
}

fn wake() {
    let window = WAKE_WINDOW.load(Ordering::Acquire);
    if window == 0 {
        return;
    }
    unsafe {
        let _ = PostMessageW(
            Some(HWND(window as *mut _)),
            WAKE_MSG.load(Ordering::Acquire),
            WPARAM(0),
            LPARAM(0),
        );
    }
}

/// Whether the Forge shows files of this kind. Every kind it knows now:
/// artefacts and diagrams through the bundled runtime, Office files by
/// conversion.
pub fn shows(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Markdown
            | Kind::Html
            | Kind::Deck
            | Kind::Image
            | Kind::Svg
            | Kind::Pdf
            | Kind::Csv
            | Kind::Jsx
            | Kind::Mermaid
            | Kind::Pptx
            | Kind::Docx
    )
}

/// The kind of `file` when the Forge shows it, None when it does not.
pub fn kind(file: &Path) -> Option<Kind> {
    horadric_core::forge::kind_of(file).filter(|k| shows(*k))
}

/// Windows paths compare without case and either slash.
fn same(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
    norm(a) == norm(b)
}

/// Shows `file` in a tab of the project `key`'s browser: the tab it has
/// already, or a new one. `root` is the project's folder.
/// Shown in front when `front`, never given the keyboard; the caller
/// decides that. The tab's id, None for a file the Forge does not show.
///
/// The browser pane itself is the app's to put on the stage.
pub fn open(key: &str, root: Option<&Path>, file: &Path, front: bool) -> Option<u64> {
    kind(file)?;
    if let Some(id) = tab_of(key, file) {
        // The watcher loads it again as it changes, so showing is all.
        if front {
            web::select_quiet(key, id);
        }
        return Some(id);
    }
    let (root, host) = served(root, file)?;
    let deep = !serve::is_flat(&host);
    let url = serve::url_for(&host, &root, file)?;
    settings::remember_root(&host, &root);
    let id = web::open_file_tab(key, &url, front);
    TABS.with(|t| {
        t.borrow_mut().push(FileTab {
            key: key.to_string(),
            id,
            file: file.to_path_buf(),
            root: root.clone(),
            host,
            scroll: None,
            folder: false,
        })
    });
    watch::follow(&root, deep);
    history::seen(file);
    Some(id)
}

/// Shows the Forge's home for the project `key`: the page of its folder
/// `root`, to pick a file from or start one. The home tab it has already,
/// wherever the human browsed it to, or a new one. None for a project with
/// no folder the Forge may serve.
pub fn open_home(key: &str, root: Option<&Path>, front: bool) -> Option<u64> {
    let root = root.filter(|r| !broad(r))?;
    prune();
    let known = TABS.with(|t| {
        t.borrow()
            .iter()
            .find(|f| f.key == key && f.folder)
            .map(|f| f.id)
    });
    if let Some(id) = known {
        if front {
            web::select_quiet(key, id);
        }
        return Some(id);
    }
    let host = serve::host_for(root);
    settings::remember_root(&host, root);
    let id = web::open_file_tab(key, &format!("https://{host}/"), front);
    TABS.with(|t| {
        t.borrow_mut().push(FileTab {
            key: key.to_string(),
            id,
            file: root.to_path_buf(),
            root: root.to_path_buf(),
            host,
            scroll: None,
            folder: true,
        })
    });
    watch::follow(root, true);
    Some(id)
}

/// The address of the Forge's page of the folder `dir` in `root`, served
/// at `host`, ending in a slash so the page's links are relative to it.
/// A flat host has only its top folder.
fn folder_url(host: &str, root: &Path, dir: &Path) -> Option<String> {
    if same(dir, root) || serve::is_flat(host) {
        return Some(format!("https://{host}/"));
    }
    serve::url_for(host, root, dir).map(|u| u + "/")
}

/// Takes the tab `id` to the Forge's page of the folder `dir`, or of its
/// top folder when None: the way from a file to the others beside it.
pub fn browse(key: &str, id: u64, dir: Option<&Path>) {
    let found = TABS.with(|t| {
        t.borrow()
            .iter()
            .find(|f| f.key == key && f.id == id)
            .map(|f| (f.root.clone(), f.host.clone()))
    });
    let Some((root, host)) = found else { return };
    let dir = dir.filter(|d| inside(&root, d)).unwrap_or(&root);
    if let Some(url) = folder_url(&host, &root, dir) {
        web::navigate(key, id, &url);
    }
}

/// The folder the tab `id` shows the page of, with its top folder, None
/// for a file's tab or a web page.
pub fn folder_of(key: &str, id: u64) -> Option<(PathBuf, PathBuf)> {
    TABS.with(|t| {
        t.borrow()
            .iter()
            .find(|f| f.key == key && f.id == id && f.folder)
            .map(|f| (f.root.clone(), f.file.clone()))
    })
}

/// The files sessions of the project `key` wrote lately, newest first, at
/// most `max`. From memory only, never the disk: the new tab page asks on
/// every paint.
pub fn recent_files(key: &str, max: usize) -> Vec<PathBuf> {
    let all = RECENT.lock().map(|r| r.clone()).unwrap_or_default();
    all.into_iter()
        .rev()
        .filter(|(k, _)| k == key)
        .map(|(_, f)| f)
        .take(max)
        .collect()
}

/// Every file sessions wrote lately, newest last, for the home page to
/// keep those in its folder.
fn recent_written() -> Vec<PathBuf> {
    RECENT
        .lock()
        .map(|r| r.iter().map(|(_, f)| f.clone()).collect())
        .unwrap_or_default()
}

/// The folder `file` is served from and the host it is served at: the
/// project's folder, or for a file outside it the file's own folder
/// without its subfolders. None when that folder is too broad to serve,
/// as a page could then read whatever the user keeps there.
fn served(project: Option<&Path>, file: &Path) -> Option<(PathBuf, String)> {
    if let Some(root) = project.filter(|r| inside(r, file) && !broad(r)) {
        return Some((root.to_path_buf(), serve::host_for(root)));
    }
    let dir = file.parent()?;
    if broad(dir) {
        return None;
    }
    Some((dir.to_path_buf(), serve::host_for_flat(dir)))
}

/// Whether the Forge may serve `file`, given the project's folder.
pub fn can_serve(project: Option<&Path>, file: &Path) -> bool {
    served(project, file).is_some()
}

/// A drive's root, the user's home folder or a folder holding it: served,
/// any page shown from it could read keys and mail.
fn broad(dir: &Path) -> bool {
    if dir.parent().is_none() {
        return true;
    }
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase();
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    home.is_some_and(|h| norm(&h) == norm(dir) || inside(dir, &h))
}

/// Whether `file` is in the folder `root`.
fn inside(root: &Path, file: &Path) -> bool {
    let r = root.to_string_lossy().replace('/', "\\").to_lowercase();
    let f = file.to_string_lossy().replace('/', "\\").to_lowercase();
    f.strip_prefix(r.trim_end_matches('\\'))
        .is_some_and(|rest| rest.starts_with('\\'))
}

/// Forgets file tabs whose browser tab closed. The browser does not say,
/// so whoever asks after a tab looks first.
fn prune() {
    let closed = TABS.with(|t| {
        let mut t = t.borrow_mut();
        let before = t.len();
        t.retain(|f| web::place_of(&f.key, f.id).is_some());
        t.len() < before
    });
    // Only when one went: prune runs on every lookup.
    if closed {
        history::forget_closed();
    }
}

/// The tab of the project `key` showing `file`, if one does.
pub fn tab_of(key: &str, file: &Path) -> Option<u64> {
    prune();
    TABS.with(|t| {
        t.borrow()
            .iter()
            .find(|f| f.key == key && !f.folder && same(&f.file, file))
            .map(|f| f.id)
    })
}

/// The file the tab `id` of the project `key` shows, None for a web page
/// or a folder's page.
pub fn file_of(key: &str, id: u64) -> Option<PathBuf> {
    TABS.with(|t| {
        t.borrow()
            .iter()
            .find(|f| f.key == key && f.id == id && !f.folder)
            .map(|f| f.file.clone())
    })
}

/// Sends the page of the tab `id` a message, `{forge: ...}` as
/// docs/plan/the-forge.md lists them. Waits for the page to be made.
pub fn post(key: &str, id: u64, msg: &Value) {
    let json = HSTRING::from(msg.to_string());
    web::with_view(key, id, move |view| {
        if let Some(v) = view {
            let _ = unsafe { v.PostWebMessageAsJson(&json) };
        }
    });
}

/// A browser tab was restored after a restart. A file's tab is a file's
/// again: served its folder and watched.
pub fn restored(key: &str, id: u64, url: &str) {
    let Some((root, file)) = serve::parse_url(url, settings::root_of) else {
        return;
    };
    let host = settings_host(url, &root);
    let deep = !serve::is_flat(&host);
    if broad(&root) {
        return;
    }
    let folder = file.is_dir();
    if !folder {
        history::seen(&file);
    }
    TABS.with(|t| {
        t.borrow_mut().push(FileTab {
            key: key.to_string(),
            id,
            file,
            root: root.clone(),
            host,
            scroll: None,
            folder,
        })
    });
    watch::follow(&root, deep);
}

/// The host a saved tab's address names, kept as it was: a flat host
/// stays flat.
fn settings_host(url: &str, root: &Path) -> String {
    serve::host_of(url)
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| serve::host_for(root))
}

/// WebView2 made the tab `id`. When it is a file's, its folder is served
/// to it and the page scripts and message routing go in, all before the
/// first page loads.
pub fn ready(key: &str, id: u64, view: &ICoreWebView2) {
    let found = TABS.with(|t| {
        t.borrow()
            .iter()
            .find(|f| f.key == key && f.id == id)
            .map(|f| (f.root.clone(), f.host.clone()))
    });
    let Some((root, host)) = found else {
        return;
    };
    serve::prepare(key, id, view, &root, &host);
    present::listen(key, id, view, &host);
    let owned = key.to_string();
    // Before the page's own scripts run, so its ready message already
    // finds the tab showing the file it is.
    let loading = ContentLoadingEventHandler::create(Box::new(move |view, _| {
        if let Some(view) = view {
            let mut p = PWSTR::null();
            if unsafe { view.Source(&mut p) }.is_ok() && !p.is_null() {
                let url = unsafe { p.to_string() }.unwrap_or_default();
                unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
                moved(&owned, id, &url);
            }
        }
        Ok(())
    }));
    let mut token = Default::default();
    let _ = unsafe { view.add_ContentLoading(&loading, &mut token) };
}

/// The tab `id` loaded `url`. A link followed inside its folder makes it
/// that file's tab: the bar, pins and history then act on the file shown.
/// A folder makes it that folder's page, the Forge's home. A page out on
/// the web leaves it as it was, heard by no one.
fn moved(key: &str, id: u64, url: &str) {
    let to = TABS.with(|t| {
        let mut t = t.borrow_mut();
        let f = t.iter_mut().find(|f| f.key == key && f.id == id)?;
        let (root, host) = (f.root.clone(), f.host.clone());
        let (_, file) = serve::parse_url(url, |h| h.eq_ignore_ascii_case(&host).then(|| root.clone()))?;
        if same(&file, &f.file) {
            return None;
        }
        f.folder = file.is_dir();
        f.file = file.clone();
        f.scroll = None;
        Some((file, f.folder))
    });
    let Some((file, folder)) = to else { return };
    if !folder {
        history::seen(&file);
    }
    history::tab_moved(key, id);
    pins::tab_moved(key, id);
    edit::page_gone(key, id);
}

/// The page of the tab `id` said something, as JSON. Only a page of the
/// tab's own made up host is heard: a file's tab can be taken to any site.
fn heard_page(key: &str, id: u64, source: &str, json: &str) {
    let found = TABS.with(|t| {
        t.borrow()
            .iter()
            .find(|f| f.key == key && f.id == id)
            .map(|f| (f.host.clone(), f.folder))
    });
    let Some((host, folder)) = found else { return };
    if serve::host_of(source).is_none_or(|h| !h.eq_ignore_ascii_case(&host)) {
        return;
    }
    // The editor's preview frame runs the page side too. Should its posts
    // ever reach here, its ready or scroll is not the tab's page's.
    if serve::is_preview(source) {
        return;
    }
    let Ok(msg) = serde_json::from_str::<Value>(json) else {
        return;
    };
    // A folder's page is ours, not a file: pins and edits have nothing to
    // land on there.
    if folder {
        home::on_message(key, id, &msg);
    } else {
        message::on_message(key, id, &msg);
    }
}

/// Where the page of the tab `id` was scrolled to.
fn scrolled(key: &str, id: u64, at: (f64, f64)) {
    TABS.with(|t| {
        if let Some(f) = t
            .borrow_mut()
            .iter_mut()
            .find(|f| f.key == key && f.id == id)
        {
            f.scroll = Some(at);
        }
    });
}

fn scroll_of(key: &str, id: u64) -> Option<(f64, f64)> {
    TABS.with(|t| {
        t.borrow()
            .iter()
            .find(|f| f.key == key && f.id == id)
            .and_then(|f| f.scroll)
    })
}

/// Files changed on disk in `root`, as paths relative to it with forward
/// slashes, or every file there when `all` (the watcher lost count). Each
/// tab showing one, or a page that may load one, is loaded again.
///
/// Except where the page is in the middle of it: a tab whose own editor
/// wrote its file is left as it is, and a tab in edit mode is told what
/// the file holds now rather than loaded again, which would throw away
/// what the human is typing.
fn changed(root: &Path, rel: &[String], all: bool) {
    prune();
    let own_file = |f: &FileTab| {
        all || rel.iter().any(|r| same(&f.root.join(r.replace('/', "\\")), &f.file))
    };
    let mine: Vec<PathBuf> = TABS.with(|t| {
        t.borrow()
            .iter()
            .filter(|f| same(&f.root, root) && !f.folder && own_file(f))
            .map(|f| f.file.clone())
            .collect()
    });
    // Which tabs' editors wrote what each file holds now, asked once a file.
    let mut writers: Vec<(PathBuf, Vec<(String, u64)>)> = Vec::new();
    for file in mine {
        if writers.iter().any(|(f, _)| same(f, &file)) {
            continue;
        }
        let by = edit::written_by(&file);
        // Before the reload, so the bytes it replaced are kept as a version.
        history::changed(root, &file, !by.is_empty());
        writers.push((file, by));
    }
    let hit: Vec<(String, u64, PathBuf, bool, bool)> = TABS.with(|t| {
        t.borrow()
            .iter()
            .filter(|f| same(&f.root, root))
            .filter(|f| all || rel.iter().any(|r| touches(f, r)))
            .map(|f| {
                let others = all || rel.iter().any(|r| touches(f, r) && !same(&f.root.join(r.replace('/', "\\")), &f.file));
                (f.key.clone(), f.id, f.file.clone(), !f.folder && own_file(f), others)
            })
            .collect()
    });
    for (key, id, file, mine, others) in hit {
        let by_itself = mine
            && writers
                .iter()
                .any(|(f, by)| same(f, &file) && by.iter().any(|(k, t)| *k == key && *t == id));
        if edit::editing(&key, id) {
            // A stylesheet or picture changing shows in the editor's preview
            // when it next loads; the page itself stays.
            if mine && !by_itself {
                match edit::text_of(&file) {
                    Some(current) => post(&key, id, &serde_json::json!({"forge": "changed-on-disk", "current": current})),
                    None => web::reload_fresh(&key, id),
                }
            }
            continue;
        }
        if by_itself && !others {
            continue;
        }
        web::reload_fresh(&key, id);
    }
}

/// Whether a change to `rel` shows in the tab `f`: its own file, or for a
/// page that loads others (HTML, a deck, markdown with pictures), a file
/// beside it or below that a page loads.
fn touches(f: &FileTab, rel: &str) -> bool {
    let changed = f.root.join(rel.replace('/', "\\"));
    // A folder's page is listed again when it is shown again. Loaded again
    // on a change, it could cancel the way to a file just made from it.
    if f.folder {
        return false;
    }
    if same(&changed, &f.file) {
        return true;
    }
    let loads_others = matches!(
        horadric_core::forge::kind_of(&f.file),
        Some(Kind::Html | Kind::Deck | Kind::Markdown)
    );
    let near = f.file.parent().is_some_and(|dir| inside(dir, &changed));
    loads_others && near && serve::is_subresource(&changed)
}

/// The files `watch` reports may be any in the folder; these are the
/// folders it must watch, the roots of every open file tab.
fn roots() -> HashSet<String> {
    TABS.with(|t| {
        t.borrow()
            .iter()
            .map(|f| watch::key_of(&f.root, !serve::is_flat(&f.host)))
            .collect()
    })
}

/// On the UI thread, woken by the watcher: loads again every tab whose
/// file, or a file its page loads, changed, and stops watching folders no
/// tab is served from any more.
pub fn drain() {
    serve::drain();
    tools::drain();
    history::drain();
    edit::drain();
    home::drain();
    for (root, rel, all) in watch::take() {
        changed(&root, &rel, all);
    }
    watch::keep_only(&roots());
}

/// Off the feeder thread: a session wrote a file the Forge shows. It is
/// queued for [`take_written`] and the UI thread woken.
pub fn heard(registry: &Mutex<Registry>, id: &str, event: &HookEvent) {
    edit::hooked(event);
    if event.hook_event_name != "PostToolUse" {
        return;
    }
    let Some(tool) = event.tool_name.as_deref() else {
        return;
    };
    if !horadric_core::rarity::WRITERS.contains(&tool) {
        return;
    }
    let Some(input) = event.tool_input.as_ref() else {
        return;
    };
    let Some(path) = input.get("file_path").and_then(Value::as_str) else {
        return;
    };
    let file = PathBuf::from(horadric_core::glow::full(path, &event.cwd).replace('/', "\\"));
    if kind(&file).is_none() {
        return;
    }
    // A brother's session writes on its own machine, not here.
    let key = registry.lock().ok().and_then(|r| {
        let s = r.get(id)?;
        s.brother
            .is_none()
            .then(|| crate::project::project_key(s))
    });
    let Some(key) = key else { return };
    pins::wrote(&key, &file, id);
    if let Ok(mut r) = RECENT.lock() {
        r.retain(|(k, f)| !(*k == key && same(f, &file)));
        r.push((key.clone(), file.clone()));
        let over = r.len().saturating_sub(RECENT_KEPT);
        r.drain(..over);
    }
    if let Ok(mut w) = WRITTEN.lock() {
        w.push((key, file));
    }
    wake();
}

/// The files sessions wrote since last asked, by project, each once, for
/// the app to open where the project's switch allows.
pub fn take_written() -> Vec<(String, PathBuf)> {
    let all = WRITTEN
        .lock()
        .map(|mut w| std::mem::take(&mut *w))
        .unwrap_or_default();
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for (key, file) in all {
        if !out.iter().any(|(k, f)| *k == key && same(f, &file)) {
            pins::written(&key, &file);
            out.push((key, file));
        }
    }
    out
}
