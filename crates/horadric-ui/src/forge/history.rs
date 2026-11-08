//! Every write to a file shown in the Forge keeps the bytes it replaced,
//! and the tab's history control steps through them, shows any rendered,
//! compares two and restores one (docs/plan/the-forge.md, Versions).
//!
//! The watcher only ever sees the new bytes, so the bytes each shown file
//! had when last seen are held by one keeper thread, read as its tab opens
//! and refreshed after each change; a change keeps the held bytes as a
//! version.
//!
//! A version is shown read only from a host of its own beside the tab's,
//! `<name>-<hash>.v.forge.horadric`, answered here from the kept bytes.
//! It still ends in the Forge's suffix, so the page side's scripts run and
//! a deck is a deck, but the tab's own messages are only heard from the
//! live host, so a pin on an old version goes nowhere.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use horadric_core::forge::versions::{self, Version};
use horadric_core::forge::{DiffLine, Kind};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2, ICoreWebView2Deferral, ICoreWebView2Environment,
    ICoreWebView2WebResourceRequestedEventArgs, ICoreWebView2_2,
    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
};
use webview2_com::WebResourceRequestedEventHandler;
use windows::core::{Interface, HSTRING, PWSTR};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::SHCreateMemStream;

use super::serve::{styled, SUFFIX};
use crate::menu::{self, Item};
use crate::web;

/// What is kept of one folder's files, oldest first deleted past it.
const LIMIT: u64 = 200 * 1024 * 1024;
/// Files larger than this are not held or kept: a version of a video
/// would take the whole limit by itself.
const LARGEST: u64 = 32 * 1024 * 1024;
/// A session's write that never said it ended stops holding a restore
/// back after this.
const WRITE_LASTS: Duration = Duration::from_secs(120);
/// While the human edits a file in its page, a version is kept at most
/// this often: the editor writes after every pause in typing, and a
/// version per pause would bury the ones worth going back to.
const EDITING_KEPT: Duration = Duration::from_secs(30);

/// Whether a change keeps the bytes it replaced as a version, given when
/// a page's write to the file last kept one (`last`), and what to remember
/// of that after. A session's or a program's change always keeps, and
/// ends the run of edits, so the next edit keeps what the session wrote.
fn coalesce(by_page: bool, last: Option<Instant>, now: Instant) -> (bool, Option<Instant>) {
    match (by_page, last) {
        (false, _) => (true, None),
        (true, Some(at)) if now.saturating_duration_since(at) < EDITING_KEPT => (false, Some(at)),
        (true, _) => (true, Some(now)),
    }
}

thread_local! {
    /// Tabs showing a version instead of the file.
    static VIEWING: RefCell<Vec<Viewing>> = const { RefCell::new(Vec::new()) };
    /// Tabs whose page answers the version host.
    static SERVING: RefCell<HashSet<(String, u64)>> = RefCell::new(HashSet::new());
}

struct Viewing {
    key: String,
    tab: u64,
    /// The version shown, by when it was kept in Unix ms: an index would
    /// move as new versions are kept.
    at: u64,
}

fn fold(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\").to_lowercase()
}

fn ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Where the versions of the files in `root` are kept: one folder for each
/// folder file tabs are served from, which for a project's files is the
/// project.
fn store(root: &Path) -> Option<PathBuf> {
    let host = super::serve::host_for(root);
    let name = host.trim_end_matches(SUFFIX);
    Some(horadric_hooks::state_dir()?.join("forge").join(name))
}

/// `file`'s path inside `root`, None for one outside it.
fn rel_of(root: &Path, file: &Path) -> Option<PathBuf> {
    let r = root.to_string_lossy().replace('/', "\\");
    let r = r.trim_end_matches('\\');
    let f = file.to_string_lossy().replace('/', "\\");
    if !f.to_lowercase().starts_with(&r.to_lowercase()) {
        return None;
    }
    let rest = f.get(r.len()..)?.strip_prefix('\\')?;
    (!rest.is_empty()).then(|| PathBuf::from(rest))
}

fn read_small(file: &Path) -> Option<Vec<u8>> {
    let len = std::fs::metadata(file).ok()?.len();
    if len > LARGEST {
        return None;
    }
    std::fs::read(file).ok()
}

/// What the keeper thread is told, in the order it happened.
enum Job {
    /// A tab showing the file opened.
    Seen(PathBuf),
    /// The file changed on disk; with where to keep what it held, when it
    /// is in its folder, and whether a page's editor wrote it.
    Changed(PathBuf, Option<(PathBuf, PathBuf)>, bool),
    /// The files tabs still show, by path folded: the rest are let go.
    Only(HashSet<String>),
}

/// Bumped whenever what a tab's history shows may differ: a version kept,
/// a step through the history, a file changed. The bar lays itself out
/// again when it moves, and not on every paint.
static GENERATION: AtomicU64 = AtomicU64::new(0);

pub(super) fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

fn bump() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

thread_local! {
    static KEEPER: RefCell<Option<Sender<Job>>> = const { RefCell::new(None) };
}

/// Hands `job` to the one keeper thread, started on first use. One thread,
/// so keeps land in the order the changes came and never race each other.
fn send(job: Job) {
    KEEPER.with(|k| {
        let mut k = k.borrow_mut();
        // A keeper that died (a panic) is started again with what it lost.
        let job = match k.as_ref() {
            Some(tx) => match tx.send(job) {
                Ok(()) => return,
                Err(e) => e.0,
            },
            None => job,
        };
        let (tx, rx) = mpsc::channel();
        let started = thread::Builder::new()
            .name("horadric-forge-keep".into())
            .spawn(move || keeper(rx));
        if started.is_ok() && tx.send(job).is_ok() {
            *k = Some(tx);
        }
    });
}

/// The keeper: holds the bytes each shown file had when last seen, and
/// reads, keeps and prunes off the UI thread.
fn keeper(rx: Receiver<Job>) {
    let mut seen: HashMap<String, Vec<u8>> = HashMap::new();
    let mut edited: HashMap<String, Instant> = HashMap::new();
    for job in rx {
        match job {
            Job::Seen(file) => {
                let key = fold(&file);
                if let std::collections::hash_map::Entry::Vacant(slot) = seen.entry(key) {
                    if let Some(bytes) = read_small(&file) {
                        slot.insert(bytes);
                    }
                }
            }
            Job::Changed(file, place, by_page) => {
                let key = fold(&file);
                let now = read_small(&file);
                let before = match &now {
                    Some(b) => seen.insert(key.clone(), b.clone()),
                    None => seen.remove(&key),
                };
                let Some(before) = before else { continue };
                if now.as_deref() == Some(before.as_slice()) {
                    continue;
                }
                let (keep, last) = coalesce(by_page, edited.get(&key).copied(), Instant::now());
                match last {
                    Some(at) => edited.insert(key, at),
                    None => edited.remove(&key),
                };
                if !keep {
                    continue;
                }
                let Some((store, rel)) = place else { continue };
                if let Err(e) = versions::keep(&store, &rel, &before) {
                    eprintln!("horadric: cannot keep a version of {}: {e}", rel.display());
                    continue;
                }
                if let Err(e) = versions::prune(&store, LIMIT) {
                    eprintln!("horadric: cannot prune {}: {e}", store.display());
                }
                bump();
            }
            Job::Only(open) => {
                seen.retain(|k, _| open.contains(k));
                edited.retain(|k, _| open.contains(k));
            }
        }
    }
}

/// The files the Forge's tabs show, by path folded.
fn open_files() -> HashSet<String> {
    super::TABS.with(|t| t.borrow().iter().map(|f| fold(&f.file)).collect())
}

/// A tab showing `file` opened: what it holds now is what a change will
/// keep.
pub(super) fn seen(file: &Path) {
    send(Job::Seen(file.to_path_buf()));
}

/// `file`, shown from `root`, changed on disk: the bytes it had are kept
/// as a version, unless they are what it holds still or, when a page's
/// editor wrote it (`by_page`), one was kept moments ago.
pub(super) fn changed(root: &Path, file: &Path, by_page: bool) {
    let place = store(root).zip(rel_of(root, file));
    send(Job::Changed(file.to_path_buf(), place, by_page));
    send(Job::Only(open_files()));
    bump();
}

/// Tabs closed: lets go of the bytes held for files no tab shows any more,
/// and of what closed tabs were showing and serving. Call after the tabs
/// are gone from `TABS`.
pub(super) fn forget_closed() {
    send(Job::Only(open_files()));
    let open: HashSet<(String, u64)> =
        super::TABS.with(|t| t.borrow().iter().map(|f| (f.key.clone(), f.id)).collect());
    VIEWING.with(|v| {
        v.borrow_mut()
            .retain(|v| open.contains(&(v.key.clone(), v.tab)))
    });
    SERVING.with(|s| s.borrow_mut().retain(|k| open.contains(k)));
    bump();
}

/// The tab's folder, file, and the file's path in the folder.
fn tab_file(key: &str, tab: u64) -> Option<(PathBuf, PathBuf, PathBuf)> {
    let (root, file) = super::TABS.with(|t| {
        t.borrow()
            .iter()
            .find(|f| f.key == key && f.id == tab)
            .map(|f| (f.root.clone(), f.file.clone()))
    })?;
    let rel = rel_of(&root, &file)?;
    Some((root, file, rel))
}

fn list(root: &Path, rel: &Path) -> Vec<Version> {
    store(root)
        .map(|s| versions::list(&s, rel))
        .unwrap_or_default()
}

/// The version the tab shows, when it shows one.
pub fn viewing(key: &str, tab: u64) -> Option<u64> {
    VIEWING.with(|v| {
        v.borrow()
            .iter()
            .find(|v| v.key == key && v.tab == tab)
            .map(|v| v.at)
    })
}

/// What the bar says while the tab shows a version: when it was kept, and
/// where it is among them.
pub fn label(key: &str, tab: u64) -> Option<String> {
    let at = viewing(key, tab)?;
    let (root, _, rel) = tab_file(key, tab)?;
    let all = list(&root, &rel);
    let i = all.iter().position(|v| ms(v.at) == at)?;
    Some(format!(
        "as it was until {}, {} of {} back",
        when(at),
        i + 1,
        all.len()
    ))
}

/// A time as the bar says it: the clock alone today, with the day before.
fn when(at_ms: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let offset = crate::questlog::utc_offset(now);
    let local = (at_ms / 1000) as i64 + offset;
    let day = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let clock = format!("{:02}:{:02}", secs / 3600, secs % 3600 / 60);
    if day == (now as i64 + offset).div_euclid(86_400) {
        return clock;
    }
    let (_, m, d) = civil(day);
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!("{d} {} {clock}", MONTHS[(m - 1) as usize])
}

/// Year, month and day of a day count from 1970, as Howard Hinnant's
/// `civil_from_days` has it.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// One step back: the newest version from the live file, else the one
/// before the version shown.
pub fn older(key: &str, tab: u64) {
    let Some((root, _, rel)) = tab_file(key, tab) else {
        return;
    };
    let all = list(&root, &rel);
    let next = match viewing(key, tab) {
        None => all.first(),
        Some(at) => match all.iter().position(|v| ms(v.at) == at) {
            Some(i) => all.get(i + 1).or(all.get(i)),
            None => all.first(),
        },
    };
    if let Some(v) = next {
        show(key, tab, ms(v.at));
    }
}

/// One step forward, and from the newest version back to the live file.
pub fn newer(key: &str, tab: u64) {
    let Some(at) = viewing(key, tab) else { return };
    let Some((root, _, rel)) = tab_file(key, tab) else {
        return;
    };
    let all = list(&root, &rel);
    match all.iter().position(|v| ms(v.at) == at) {
        Some(i) if i > 0 => show(key, tab, ms(all[i - 1].at)),
        _ => live(key, tab),
    }
}

/// The tab followed a link to another file, so the version it showed is
/// of a file it no longer shows.
pub(super) fn tab_moved(key: &str, tab: u64) {
    VIEWING.with(|v| v.borrow_mut().retain(|v| !(v.key == key && v.tab == tab)));
    bump();
}

/// Back to the file as it is, out of the history.
pub fn live(key: &str, tab: u64) {
    VIEWING.with(|v| v.borrow_mut().retain(|v| !(v.key == key && v.tab == tab)));
    bump();
    let url = super::TABS.with(|t| {
        t.borrow()
            .iter()
            .find(|f| f.key == key && f.id == tab)
            .and_then(|f| super::serve::url_for(&f.host, &f.root, &f.file))
    });
    if let Some(url) = url {
        web::navigate(key, tab, &url);
    }
}

/// The host versions of the folder served at `host` are shown from.
fn version_host(host: &str) -> String {
    format!("{}.v{SUFFIX}", host.trim_end_matches(SUFFIX))
}

/// A path in the project as an address holds it.
fn url_path(rel: &Path) -> String {
    rel.to_string_lossy()
        .split(['\\', '/'])
        .filter(|s| !s.is_empty())
        .map(|seg| {
            let mut out = String::new();
            for b in seg.bytes() {
                if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                    out.push(b as char);
                } else {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Shows the version kept at `at` in the tab.
fn show(key: &str, tab: u64, at: u64) {
    let Some(url) = version_url(key, tab, &at.to_string()) else {
        return;
    };
    VIEWING.with(|v| {
        let mut v = v.borrow_mut();
        match v.iter_mut().find(|v| v.key == key && v.tab == tab) {
            Some(v) => v.at = at,
            None => v.push(Viewing {
                key: key.to_string(),
                tab,
                at,
            }),
        }
    });
    bump();
    navigate_served(key, tab, url);
}

/// The address of the tab's file at `which` (a version's ms, or `now`),
/// or of a page of the history (`compare/<a>/<b>`).
fn version_url(key: &str, tab: u64, which: &str) -> Option<String> {
    let (host, root, file) = super::TABS.with(|t| {
        t.borrow()
            .iter()
            .find(|f| f.key == key && f.id == tab)
            .map(|f| (f.host.clone(), f.root.clone(), f.file.clone()))
    })?;
    let rel = rel_of(&root, &file)?;
    Some(format!(
        "https://{}/{which}/{}",
        version_host(&host),
        url_path(&rel)
    ))
}

/// Takes the tab to `url` on the version host, readying its page to answer
/// that host first.
fn navigate_served(key: &str, tab: u64, url: String) {
    let found = super::TABS.with(|t| {
        t.borrow()
            .iter()
            .find(|f| f.key == key && f.id == tab)
            .map(|f| (f.root.clone(), f.host.clone()))
    });
    let Some((root, host)) = found else { return };
    let owned = key.to_string();
    web::with_view(key, tab, move |view| {
        let Some(view) = view else { return };
        let first = SERVING.with(|s| s.borrow_mut().insert((owned.clone(), tab)));
        if first {
            // The version host's pages keep the look the tab had when it
            // was first asked for one, as the chrome does.
            let look = super::serve::look(crate::theme::accent(&owned));
            serve_versions(&view, &root, &version_host(&host), look);
        }
        web::navigate(&owned, tab, &url);
    });
}

/// Answers every request to the version host of the page `view`, its
/// pages in Horadric's `look`.
fn serve_versions(view: &ICoreWebView2, root: &Path, host: &str, look: String) {
    unsafe {
        let _ = view.AddWebResourceRequestedFilter(
            &HSTRING::from(format!("https://{host}/*")),
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
        );
        let root = root.to_path_buf();
        let prefix = format!("https://{host}/");
        let handler = WebResourceRequestedEventHandler::create(Box::new(move |view, args| {
            if let (Some(view), Some(args)) = (view, args) {
                answer(&view, &args, &root, &prefix, &look);
            }
            Ok(())
        }));
        let mut token = Default::default();
        let _ = view.add_WebResourceRequested(&handler, &mut token);
    }
}

/// What a request to the version host gets: a version of a file, a file as
/// it is now for what the version's page loads, or a comparison. Reading
/// the bytes and building a comparison happen on a worker thread; the
/// answer goes back from [`drain`] on the UI thread.
fn answer(
    view: &ICoreWebView2,
    args: &ICoreWebView2WebResourceRequestedEventArgs,
    root: &Path,
    prefix: &str,
    look: &str,
) {
    let Ok(request) = (unsafe { args.Request() }) else {
        return;
    };
    let url = read(|p| unsafe { request.Uri(p) });
    if !url
        .to_ascii_lowercase()
        .starts_with(&prefix.to_ascii_lowercase())
    {
        return;
    }
    let Ok(env) = (unsafe { view.cast::<ICoreWebView2_2>().and_then(|v| v.Environment()) }) else {
        return;
    };
    let path = url[prefix.len()..]
        .split(['?', '#'])
        .next()
        .unwrap_or_default();
    let segs: Vec<String> = path.split('/').map(decode).collect();
    if segs
        .iter()
        .any(|s| s == ".." || s == "." || s.contains(['\\', '/', ':']))
    {
        return respond(&env, args, &(403, "text/plain", b"Not here".to_vec()));
    }
    let (root, look) = (root.to_path_buf(), look.to_string());
    let Ok(deferral) = (unsafe { args.GetDeferral() }) else {
        return respond(&env, args, &reply_for(&root, segs, &look));
    };
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    WAITING.with(|w| {
        w.borrow_mut().insert(
            n,
            Waiting {
                args: args.clone(),
                deferral,
                env,
            },
        )
    });
    let started = thread::Builder::new()
        .name("horadric-forge-version".into())
        .spawn(move || {
            let reply = reply_for(&root, segs, &look);
            if let Ok(mut done) = ANSWERED.lock() {
                done.push((n, reply));
            }
            super::wake();
        });
    if started.is_err() {
        if let Some(w) = WAITING.with(|w| w.borrow_mut().remove(&n)) {
            respond(&w.env, &w.args, &(404, "text/plain", b"Not kept".to_vec()));
            let _ = unsafe { w.deferral.Complete() };
        }
    }
}

/// A status, a content type and the body.
type Reply = (i32, &'static str, Vec<u8>);

/// A request to the version host waiting on its worker.
struct Waiting {
    args: ICoreWebView2WebResourceRequestedEventArgs,
    deferral: ICoreWebView2Deferral,
    env: ICoreWebView2Environment,
}

thread_local! {
    static WAITING: RefCell<HashMap<u64, Waiting>> = RefCell::new(HashMap::new());
}

/// Answers the workers have made, by the number of the request.
static ANSWERED: Mutex<Vec<(u64, Reply)>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(1);

/// On the UI thread, woken by a worker: answers the requests to the version
/// host whose bytes are ready.
pub(super) fn drain() {
    let done = ANSWERED
        .lock()
        .map(|mut d| std::mem::take(&mut *d))
        .unwrap_or_default();
    for (n, reply) in done {
        if let Some(w) = WAITING.with(|w| w.borrow_mut().remove(&n)) {
            respond(&w.env, &w.args, &reply);
            let _ = unsafe { w.deferral.Complete() };
        }
    }
}

/// The answer to the path `segs` on the version host, its pages in
/// `look`. Reads files, so it is for a worker thread.
fn reply_for(root: &Path, mut segs: Vec<String>, look: &str) -> Reply {
    if segs.first().is_some_and(|s| s == "compare") && segs.len() > 3 {
        let rel: PathBuf = segs[3..].iter().collect();
        let page = compare_page(root, &rel, &segs[1], &segs[2], look);
        (200, "text/html; charset=utf-8", page.into_bytes())
    } else if segs.len() > 1 {
        let which = segs.remove(0);
        let rel: PathBuf = segs.iter().collect();
        match bytes_at(root, &rel, &which) {
            Some(bytes) => rendered(&rel, bytes, look),
            None => (404, "text/plain", b"Not kept".to_vec()),
        }
    } else {
        (404, "text/plain", b"Not kept".to_vec())
    }
}

/// The bytes of `rel` kept at `which` (Unix ms), or as it is now for `now`
/// and for a file with no version kept then, which is what a version's
/// page loads beside it.
fn bytes_at(root: &Path, rel: &Path, which: &str) -> Option<Vec<u8>> {
    if let Ok(at) = which.parse::<u64>() {
        let kept = list(root, rel).into_iter().find(|v| ms(v.at) == at);
        if let Some(v) = kept {
            return versions::read(&v).ok();
        }
    }
    std::fs::read(root.join(rel)).ok()
}

/// A file's bytes as the page gets them: markdown and CSV rendered, the
/// rest as they are.
fn rendered(rel: &Path, bytes: Vec<u8>, look: &str) -> (i32, &'static str, Vec<u8>) {
    let title = rel
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match horadric_core::forge::kind_of_bytes(rel, &bytes) {
        Some(Kind::Markdown) => {
            let src = String::from_utf8_lossy(&bytes);
            let html = styled(horadric_core::forge::markdown_html(&src, &title), look);
            (200, "text/html; charset=utf-8", html.into_bytes())
        }
        Some(Kind::Csv) => {
            let src = String::from_utf8_lossy(&bytes);
            let html = styled(horadric_core::forge::csv_html(&src, &title), look);
            (200, "text/html; charset=utf-8", html.into_bytes())
        }
        _ => (200, mime_of(rel), bytes),
    }
}

pub(super) fn mime_of(rel: &Path) -> &'static str {
    let ext = rel
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" | "jsx" | "tsx" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "md" | "markdown" | "txt" | "csv" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// What a side of a comparison is called on its page.
fn side_name(which: &str) -> String {
    match which.parse::<u64>() {
        Ok(at) => format!("until {}", when(at)),
        Err(_) => "now".to_string(),
    }
}

/// Lines of unchanged text shown round each change; longer runs fold.
const CONTEXT: usize = 3;
/// Text larger than this is not compared: its table would be too long to
/// read and too slow for the page to lay out.
const COMPARE_LARGEST: usize = 4 << 20;

/// The page comparing `rel` at `a` with it at `b`: the lines that changed
/// for text, the two side by side for a picture or a PDF. On Horadric's
/// plate in `look`: the file and the two sides engraved over a screen.
fn compare_page(root: &Path, rel: &Path, a: &str, b: &str, look: &str) -> String {
    let mut body = String::new();
    let kind = horadric_core::forge::kind_of_bytes(rel, &[]);
    if matches!(kind, Some(Kind::Image | Kind::Pdf)) {
        let path = url_path(rel);
        let tag = |w: &str| match kind {
            Some(Kind::Pdf) => format!("<iframe src=\"/{w}/{path}\"></iframe>"),
            _ => format!("<img src=\"/{w}/{path}\" alt=\"\">"),
        };
        body.push_str(&format!(
            "<div class=\"sides\"><figure><figcaption class=\"hd-legend was\"><i></i>{}</figcaption>\
             <div class=\"hd-screen\">{}</div></figure>\
             <figure><figcaption class=\"hd-legend now\"><i></i>{}</figcaption>\
             <div class=\"hd-screen\">{}</div></figure></div>",
            escape(&side_name(a)),
            tag(a),
            escape(&side_name(b)),
            tag(b)
        ));
    } else {
        let (ta, tb) = (bytes_at(root, rel, a), bytes_at(root, rel, b));
        let large = |t: &Option<Vec<u8>>| t.as_ref().is_some_and(|t| t.len() > COMPARE_LARGEST);
        if large(&ta) || large(&tb) {
            body.push_str(&format!(
                "<div class=\"hd-screen\"><p class=\"same\">Too large to compare line by line, over {} MB.</p></div>",
                COMPARE_LARGEST >> 20
            ));
        } else {
            let text = |t: Option<Vec<u8>>| {
                t.map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default()
            };
            body.push_str("<div class=\"hd-screen diff\">");
            body.push_str(&diff_table(&horadric_core::forge::diff_lines(
                &text(ta),
                &text(tb),
            )));
            body.push_str("</div>");
        }
    }
    compare_frame(rel, a, b, &body, look)
}

/// The comparison's page round its `body`.
fn compare_frame(rel: &Path, a: &str, b: &str, body: &str, look: &str) -> String {
    let title = format!("{}: {} and {}", rel.display(), side_name(a), side_name(b));
    let page = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{t}</title><style>{}{CSS}</style></head><body><div class=\"compare\">\
         <header><h1>{}</h1><div class=\"hd-legend\"><span class=\"was\"><i></i>{}</span>\
         <span class=\"now\"><i></i>{}</span></div></header>{body}</div></body></html>",
        horadric_core::forge::LOOK,
        escape(&rel.to_string_lossy().replace('\\', "/")),
        escape(&side_name(a)),
        escape(&side_name(b)),
        t = escape(&title)
    );
    styled(page, look)
}

/// The diff as a table of numbered lines, long unchanged runs folded.
fn diff_table(lines: &[DiffLine]) -> String {
    let near_change = |i: usize| {
        let lo = i.saturating_sub(CONTEXT);
        let hi = (i + CONTEXT + 1).min(lines.len());
        lines[lo..hi]
            .iter()
            .any(|l| !matches!(l, DiffLine::Same(_)))
    };
    let mut out = String::from("<table>");
    let (mut a, mut b) = (0usize, 0usize);
    let mut folded = 0usize;
    let changes = lines.iter().any(|l| !matches!(l, DiffLine::Same(_)));
    if !changes {
        return "<p class=\"same\">The two are the same.</p>".to_string();
    }
    for (i, line) in lines.iter().enumerate() {
        let (class, mark, text, na, nb) = match line {
            DiffLine::Same(t) => {
                a += 1;
                b += 1;
                ("", " ", t, a.to_string(), b.to_string())
            }
            DiffLine::Removed(t) => {
                a += 1;
                ("del", "-", t, a.to_string(), String::new())
            }
            DiffLine::Added(t) => {
                b += 1;
                ("add", "+", t, String::new(), b.to_string())
            }
        };
        if class.is_empty() && !near_change(i) {
            folded += 1;
            continue;
        }
        if folded > 0 {
            out.push_str(&format!(
                "<tr class=\"fold\"><td colspan=\"4\">{folded} lines the same</td></tr>"
            ));
            folded = 0;
        }
        out.push_str(&format!(
            "<tr class=\"{class}\"><td class=\"n\">{na}</td><td class=\"n\">{nb}</td>\
             <td class=\"m\">{mark}</td><td>{}</td></tr>",
            escape(text)
        ));
    }
    if folded > 0 {
        out.push_str(&format!(
            "<tr class=\"fold\"><td colspan=\"4\">{folded} lines the same</td></tr>"
        ));
    }
    out.push_str("</table>");
    out
}

/// The comparison drawn as the Folio draws a diff: removed rows tinted
/// in the error colour and added ones in the done colour, numbers and
/// marks in the legends' ink, unchanged runs folded behind an engraved
/// count.
const CSS: &str = r#"
body { padding: 30px 26px 40px; }
.compare { max-width: 1120px; margin: 0 auto; }
header { display: flex; flex-wrap: wrap; align-items: baseline; gap: 8px 20px; margin: 0 4px 14px; }
h1 { margin: 0; font: 600 16px/1.3 var(--hd-mono); color: var(--hd-text); overflow-wrap: anywhere; }
header .hd-legend { display: flex; gap: 18px; }
.hd-legend span, figcaption { display: inline-flex; align-items: center; gap: 8px; }
/* Which side is which: a lamp lit in the colour its rows are tinted. */
.hd-legend i { width: 6px; height: 6px; border-radius: 50%; }
.was i { background: var(--hd-error); }
.now i { background: var(--hd-done); }
.diff { padding: 6px 0; overflow: auto; }
table { border-collapse: collapse; width: 100%; font: 12.5px/1.65 var(--hd-mono); }
td { padding: 0 10px; vertical-align: top; white-space: pre-wrap; word-break: break-word;
  color: var(--hd-read); }
td.n { color: var(--hd-legend); text-align: right; width: 1%; white-space: nowrap; user-select: none;
  font-variant-numeric: tabular-nums; }
td.m { width: 1%; color: var(--hd-legend); user-select: none; }
tr.del td { background: color-mix(in srgb, var(--hd-error) 15%, transparent); }
tr.add td { background: color-mix(in srgb, var(--hd-done) 13%, transparent); }
tr.del td.m { color: var(--hd-error); }
tr.add td.m { color: var(--hd-done); }
tr.fold td { padding: 6px; text-align: center; font: 600 10.5px/1.2 var(--hd-ui);
  letter-spacing: 0.12em; text-transform: uppercase; color: var(--hd-legend);
  border-block: 1px solid color-mix(in srgb, var(--hd-read) 6%, transparent); }
.same { margin: 0; padding: 30px 20px; text-align: center; color: var(--hd-dim); font-size: 14px; }
.sides { display: flex; gap: 18px; align-items: flex-start; }
figure { flex: 1; margin: 0; min-width: 0; display: flex; flex-direction: column; gap: 10px; }
figcaption { padding: 0 4px; }
figure .hd-screen { padding: 10px; }
figure img { max-width: 100%; display: block; margin: 0 auto; }
figure iframe { width: 100%; height: 80vh; border: 0; border-radius: 6px; display: block; }
@media print {
  body { padding: 0; }
  .hd-screen { background: none; box-shadow: none; }
  td { color: #1d1e21; }
  tr.del td { background: #fbe3e4; }
  tr.add td { background: #e0f5e9; }
  h1 { color: #000; }
}
"#;

fn respond(
    env: &ICoreWebView2Environment,
    args: &ICoreWebView2WebResourceRequestedEventArgs,
    reply: &Reply,
) {
    let (status, mime, body) = reply;
    let reason = match status {
        200 => "OK",
        403 => "Forbidden",
        _ => "Not Found",
    };
    unsafe {
        let Some(stream) = SHCreateMemStream(Some(body)) else {
            return;
        };
        let headers = format!(
            "Content-Type: {mime}
Cache-Control: no-store"
        );
        if let Ok(response) = env.CreateWebResourceResponse(
            &stream,
            *status,
            &HSTRING::from(reason),
            &HSTRING::from(headers),
        ) {
            let _ = args.SetResponse(&response);
        }
    }
}

fn read(get: impl FnOnce(*mut PWSTR) -> windows::core::Result<()>) -> String {
    let mut p = PWSTR::null();
    if get(&mut p).is_err() || p.is_null() {
        return String::new();
    }
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    s
}

const NOW: usize = 1;
const BEFORE: usize = 2;

/// Compares the version shown with the file now, or with the version kept
/// before it.
pub fn compare(key: &str, tab: u64) {
    let Some(at) = viewing(key, tab) else { return };
    let Some((root, _, rel)) = tab_file(key, tab) else {
        return;
    };
    let all = list(&root, &rel);
    let before = all
        .iter()
        .position(|v| ms(v.at) == at)
        .and_then(|i| all.get(i + 1))
        .map(|v| ms(v.at));
    let mut items = vec![Item::action(NOW, "With the file as it is now").default()];
    match before {
        Some(_) => items.push(Item::action(BEFORE, "With the version before it")),
        None => items.push(Item::Disabled("With the version before it".into())),
    }
    let which = match menu::popup(&items) {
        Some(NOW) => format!("compare/{at}/now"),
        Some(BEFORE) => match before {
            Some(b) => format!("compare/{b}/{at}"),
            None => return,
        },
        _ => return,
    };
    if let Some(url) = version_url(key, tab, &which) {
        navigate_served(key, tab, url);
    }
}

/// Writes the version shown back over the file, which keeps what it
/// replaces like any write, and shows the file again. Waits out a
/// session's write to it, so the two do not land on each other.
pub fn restore(key: &str, tab: u64) {
    let Some(at) = viewing(key, tab) else { return };
    let Some((root, file, rel)) = tab_file(key, tab) else {
        return;
    };
    let Some(v) = list(&root, &rel).into_iter().find(|v| ms(v.at) == at) else {
        return;
    };
    live(key, tab);
    let _ = thread::Builder::new()
        .name("horadric-forge-restore".into())
        .spawn(move || {
            let since = Instant::now();
            while super::edit::being_written(&file) && since.elapsed() < WRITE_LASTS {
                thread::sleep(Duration::from_millis(100));
            }
            let written = versions::read(&v).and_then(|bytes| std::fs::write(&file, bytes));
            if let Err(e) = written {
                eprintln!("horadric: cannot restore {}: {e}", file.display());
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_in_the_folder_has_a_path_there() {
        let root = Path::new(r"C:\Work\Site");
        assert_eq!(
            rel_of(root, Path::new(r"c:\work\site\docs\a.md")),
            Some(PathBuf::from(r"docs\a.md"))
        );
        assert_eq!(rel_of(root, Path::new(r"C:\Work\Sitemap\a.md")), None);
        assert_eq!(
            url_path(Path::new(r"docs\my deck.html")),
            "docs/my%20deck.html"
        );
    }

    #[test]
    fn edits_keep_one_version_per_half_minute() {
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        // The first edit keeps what the file held before editing began.
        assert_eq!(coalesce(true, None, s(0)), (true, Some(s(0))));
        // Edits after it, within the half minute, keep nothing.
        assert_eq!(coalesce(true, Some(s(0)), s(10)), (false, Some(s(0))));
        assert_eq!(coalesce(true, Some(s(0)), s(29)), (false, Some(s(0))));
        // Past it, the next edit keeps one and starts the next half minute.
        assert_eq!(coalesce(true, Some(s(0)), s(30)), (true, Some(s(30))));
        // A session's write always keeps, and the edit after it keeps too.
        assert_eq!(coalesce(false, Some(s(30)), s(31)), (true, None));
        assert_eq!(coalesce(true, None, s(32)), (true, Some(s(32))));
    }

    #[test]
    fn civil_days_are_right() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_397), (2025, 11, 5));
    }

    /// Writes a comparison to `FORGE_SAMPLES`, to look at in the browser
    /// pane beside the other pages (serve.rs writes those).
    #[test]
    #[ignore]
    fn write_a_sample_comparison() {
        let Ok(dir) = std::env::var("FORGE_SAMPLES") else {
            return;
        };
        let a: String = (1..=30)
            .map(|i| format!("Line {i} of the plan\n"))
            .collect();
        let b = a.replace(
            "Line 12 of the plan\n",
            "Line 12, rewritten\nA line added\n",
        );
        let body = format!(
            "<div class=\"hd-screen diff\">{}</div>",
            diff_table(&horadric_core::forge::diff_lines(&a, &b))
        );
        let look = super::super::serve::look(crate::theme::ACCENTS[2].0);
        let page = compare_frame(
            Path::new(r"docs\plan.md"),
            "1762380000000",
            "now",
            &body,
            &look,
        );
        std::fs::write(Path::new(&dir).join("compare.html"), page).unwrap();
    }

    #[test]
    fn long_same_runs_fold() {
        let a: String = (1..=20).map(|i| format!("{i}\n")).collect();
        let b = a.replace("10\n", "ten\n");
        let html = diff_table(&horadric_core::forge::diff_lines(&a, &b));
        assert!(html.contains("6 lines the same"));
        assert!(html.contains("ten"));
        assert!(!html.contains(">2<"));
    }
}
