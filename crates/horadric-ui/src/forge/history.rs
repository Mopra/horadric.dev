//! Every write to a file shown in the Forge keeps the bytes it replaced,
//! and the tab's history control steps through them, shows any rendered,
//! compares two and restores one (docs/plan/the-forge.md, Versions).
//!
//! The watcher only ever sees the new bytes, so the bytes each shown file
//! had when last seen are held here, read as its tab opens and refreshed
//! after each change; a change keeps the held bytes as a version.
//!
//! A version is shown read only from a host of its own beside the tab's,
//! `<name>-<hash>.v.forge.horadric`, answered here from the kept bytes.
//! It still ends in the Forge's suffix, so the page side's scripts run and
//! a deck is a deck, but the tab's own messages are only heard from the
//! live host, so a pin on an old version goes nowhere.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use horadric_core::forge::versions::{self, Version};
use horadric_core::forge::{DiffLine, Kind};
use horadric_core::HookEvent;
use serde_json::Value;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2, ICoreWebView2WebResourceRequestedEventArgs, ICoreWebView2_2,
    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
};
use webview2_com::WebResourceRequestedEventHandler;
use windows::core::{Interface, HSTRING, PWSTR};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::SHCreateMemStream;

use super::serve::SUFFIX;
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

thread_local! {
    /// The bytes each shown file had when last seen, by its path folded.
    static SEEN: RefCell<HashMap<String, Vec<u8>>> = RefCell::new(HashMap::new());
    /// Tabs showing a version instead of the file.
    static VIEWING: RefCell<Vec<Viewing>> = const { RefCell::new(Vec::new()) };
    /// Tabs whose page answers the version host.
    static SERVING: RefCell<HashSet<(String, u64)>> = RefCell::new(HashSet::new());
}

/// Files a session is writing now, between its PreToolUse and PostToolUse,
/// so a restore does not land in the middle and get overwritten.
static WRITING: Mutex<Vec<(PathBuf, Instant)>> = Mutex::new(Vec::new());

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
    at.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
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

/// A tab showing `file` opened: what it holds now is what a change will
/// keep.
pub(super) fn seen(file: &Path) {
    let key = fold(file);
    if SEEN.with(|s| s.borrow().contains_key(&key)) {
        return;
    }
    if let Some(bytes) = read_small(file) {
        SEEN.with(|s| s.borrow_mut().insert(key, bytes));
    }
}

/// `file`, shown from `root`, changed on disk: the bytes it had are kept
/// as a version, unless they are what it holds still.
pub(super) fn changed(root: &Path, file: &Path) {
    let key = fold(file);
    let now = read_small(file);
    let before = SEEN.with(|s| {
        let mut s = s.borrow_mut();
        match &now {
            Some(b) => s.insert(key, b.clone()),
            None => s.remove(&key),
        }
    });
    let Some(before) = before else { return };
    if now.as_deref() == Some(before.as_slice()) {
        return;
    }
    let (Some(store), Some(rel)) = (store(root), rel_of(root, file)) else {
        return;
    };
    // Off the UI thread: a prune walks every version kept.
    let _ = thread::Builder::new()
        .name("horadric-forge-keep".into())
        .spawn(move || {
            if let Err(e) = versions::keep(&store, &rel, &before) {
                eprintln!("horadric: cannot keep a version of {}: {e}", rel.display());
                return;
            }
            if let Err(e) = versions::prune(&store, LIMIT) {
                eprintln!("horadric: cannot prune {}: {e}", store.display());
            }
        });
}

/// Off the feeder thread: a session started or ended a write, so a restore
/// waits for it to end.
pub(super) fn heard_tool(event: &HookEvent) {
    let pre = match event.hook_event_name.as_str() {
        "PreToolUse" => true,
        "PostToolUse" | "PostToolUseFailure" => false,
        _ => return,
    };
    let Some(tool) = event.tool_name.as_deref() else {
        return;
    };
    if !horadric_core::rarity::WRITERS.contains(&tool) {
        return;
    }
    let Some(path) = event
        .tool_input
        .as_ref()
        .and_then(|i| i.get("file_path"))
        .and_then(Value::as_str)
    else {
        return;
    };
    let file = PathBuf::from(horadric_core::glow::full(path, &event.cwd).replace('/', "\\"));
    if let Ok(mut w) = WRITING.lock() {
        w.retain(|(f, at)| at.elapsed() < WRITE_LASTS && fold(f) != fold(&file));
        if pre {
            w.push((file, Instant::now()));
        }
    }
}

fn being_written(file: &Path) -> bool {
    WRITING.lock().is_ok_and(|w| {
        w.iter()
            .any(|(f, at)| at.elapsed() < WRITE_LASTS && fold(f) == fold(file))
    })
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

/// Back to the file as it is, out of the history.
pub fn live(key: &str, tab: u64) {
    VIEWING.with(|v| v.borrow_mut().retain(|v| !(v.key == key && v.tab == tab)));
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
            serve_versions(&view, &root, &version_host(&host));
        }
        web::navigate(&owned, tab, &url);
    });
}

/// Answers every request to the version host of the page `view`.
fn serve_versions(view: &ICoreWebView2, root: &Path, host: &str) {
    unsafe {
        let _ = view.AddWebResourceRequestedFilter(
            &HSTRING::from(format!("https://{host}/*")),
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
        );
        let root = root.to_path_buf();
        let prefix = format!("https://{host}/");
        let handler = WebResourceRequestedEventHandler::create(Box::new(move |view, args| {
            if let (Some(view), Some(args)) = (view, args) {
                answer(&view, &args, &root, &prefix);
            }
            Ok(())
        }));
        let mut token = Default::default();
        let _ = view.add_WebResourceRequested(&handler, &mut token);
    }
}

/// What a request to the version host gets: a version of a file, a file as
/// it is now for what the version's page loads, or a comparison.
fn answer(
    view: &ICoreWebView2,
    args: &ICoreWebView2WebResourceRequestedEventArgs,
    root: &Path,
    prefix: &str,
) {
    let Ok(request) = (unsafe { args.Request() }) else {
        return;
    };
    let url = read(|p| unsafe { request.Uri(p) });
    if !url.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase()) {
        return;
    }
    let path = url[prefix.len()..].split(['?', '#']).next().unwrap_or_default();
    let mut segs: Vec<String> = path.split('/').map(decode).collect();
    if segs
        .iter()
        .any(|s| s == ".." || s == "." || s.contains(['\\', '/', ':']))
    {
        return respond(view, args, 403, "text/plain", b"Not here".to_vec());
    }
    let (status, mime, body) = if segs.first().is_some_and(|s| s == "compare") && segs.len() > 3 {
        let rel: PathBuf = segs[3..].iter().collect();
        (200, "text/html; charset=utf-8", compare_page(root, &rel, &segs[1], &segs[2]).into_bytes())
    } else if segs.len() > 1 {
        let which = segs.remove(0);
        let rel: PathBuf = segs.iter().collect();
        match bytes_at(root, &rel, &which) {
            Some(bytes) => rendered(&rel, bytes),
            None => (404, "text/plain", b"Not kept".to_vec()),
        }
    } else {
        (404, "text/plain", b"Not kept".to_vec())
    };
    respond(view, args, status, mime, body);
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
fn rendered(rel: &Path, bytes: Vec<u8>) -> (i32, &'static str, Vec<u8>) {
    let title = rel
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match horadric_core::forge::kind_of_bytes(rel, &bytes) {
        Some(Kind::Markdown) => {
            let src = String::from_utf8_lossy(&bytes);
            let html = horadric_core::forge::markdown_html(&src, &title);
            (200, "text/html; charset=utf-8", html.into_bytes())
        }
        Some(Kind::Csv) => {
            let src = String::from_utf8_lossy(&bytes);
            let html = horadric_core::forge::csv_html(&src, &title);
            (200, "text/html; charset=utf-8", html.into_bytes())
        }
        _ => (200, mime_of(rel), bytes),
    }
}

fn mime_of(rel: &Path) -> &'static str {
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

/// The page comparing `rel` at `a` with it at `b`: the lines that changed
/// for text, the two side by side for a picture or a PDF.
fn compare_page(root: &Path, rel: &Path, a: &str, b: &str) -> String {
    let title = format!(
        "{}: {} and {}",
        rel.display(),
        side_name(a),
        side_name(b)
    );
    let mut body = String::new();
    let kind = horadric_core::forge::kind_of_bytes(rel, &[]);
    if matches!(kind, Some(Kind::Image | Kind::Pdf)) {
        let path = url_path(rel);
        let tag = |w: &str| match kind {
            Some(Kind::Pdf) => format!("<iframe src=\"/{w}/{path}\"></iframe>"),
            _ => format!("<img src=\"/{w}/{path}\" alt=\"\">"),
        };
        body.push_str(&format!(
            "<div class=\"sides\"><figure><figcaption>{}</figcaption>{}</figure>\
             <figure><figcaption>{}</figcaption>{}</figure></div>",
            escape(&side_name(a)),
            tag(a),
            escape(&side_name(b)),
            tag(b)
        ));
    } else {
        let text = |w: &str| {
            bytes_at(root, rel, w)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default()
        };
        body.push_str(&diff_table(&horadric_core::forge::diff_lines(&text(a), &text(b))));
    }
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{t}</title><style>{CSS}</style>\
         </head><body><h1>{t}</h1>{body}</body></html>",
        t = escape(&title)
    )
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

const CSS: &str = "\
:root{color-scheme:dark light;--bg:#1b1a19;--fg:#e8e4dc;--dim:#8d877c;--del:#4a2424;--add:#1f3b26}\
@media (prefers-color-scheme:light){:root{--bg:#faf8f4;--fg:#22201c;--dim:#7a746a;--del:#f6dcdc;--add:#dcefdc}}\
body{margin:0;padding:16px 20px;background:var(--bg);color:var(--fg);font:14px/1.45 'Segoe UI',sans-serif}\
h1{font-size:15px;font-weight:600;margin:0 0 12px}\
table{border-collapse:collapse;width:100%;font:12.5px/1.5 Consolas,'Cascadia Mono',monospace}\
td{padding:0 8px;vertical-align:top;white-space:pre-wrap;word-break:break-word}\
td.n{color:var(--dim);text-align:right;width:1%;white-space:nowrap;user-select:none}\
td.m{color:var(--dim);width:1%;user-select:none}\
tr.del{background:var(--del)}tr.add{background:var(--add)}\
tr.fold td{color:var(--dim);text-align:center;padding:4px;font-family:'Segoe UI',sans-serif}\
.same{color:var(--dim)}\
.sides{display:flex;gap:16px;align-items:flex-start}\
figure{flex:1;margin:0;min-width:0}figcaption{color:var(--dim);margin-bottom:6px}\
figure img{max-width:100%;display:block}figure iframe{width:100%;height:85vh;border:0}";

fn respond(
    view: &ICoreWebView2,
    args: &ICoreWebView2WebResourceRequestedEventArgs,
    status: i32,
    mime: &str,
    body: Vec<u8>,
) {
    let reason = if status == 200 { "OK" } else { "Not Found" };
    unsafe {
        let Ok(env) = view.cast::<ICoreWebView2_2>().and_then(|v| v.Environment()) else {
            return;
        };
        let Some(stream) = SHCreateMemStream(Some(&body)) else {
            return;
        };
        let headers = format!("Content-Type: {mime}\r\nCache-Control: no-store");
        if let Ok(response) = env.CreateWebResourceResponse(
            &stream,
            status,
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
            while being_written(&file) && since.elapsed() < WRITE_LASTS {
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
        assert_eq!(url_path(Path::new(r"docs\my deck.html")), "docs/my%20deck.html");
    }

    #[test]
    fn civil_days_are_right() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_397), (2025, 11, 5));
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
