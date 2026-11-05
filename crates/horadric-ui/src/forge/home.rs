//! The Forge's home: the page of a folder of the project, to find a file
//! to work on or start a new one. HTML in the look of the markdown pages,
//! made by `serve` on a worker thread for any page asked of a folder.
//!
//! Folders come first, then the files the Forge shows with what kind they
//! are. The rest (other files, dotfolders, `.git`, `node_modules`,
//! `target`) wait behind a toggle. The top folder's page also lists what
//! sessions wrote lately.
//!
//! A new file is a message from the page: Horadric writes it, never over
//! another, and takes the tab to it.

use std::cmp::Ordering;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use horadric_core::forge::Kind;
use serde_json::{json, Value};

use super::serve::{encode, Reply};
use crate::web;

/// Folders a project fills with what no one reads, kept out of sight.
const TUCKED: [&str; 3] = [".git", "node_modules", "target"];

/// The most names a folder's page lists. A folder of more is a cache, not
/// work, and a page of tens of thousands of rows is of no use.
const MOST: usize = 2000;

/// How many recent files the top folder's page lists.
const RECENT: usize = 8;

/// The walk for recent files when no session wrote any: how many names it
/// looks at and how deep, so a huge project costs a bounded wait.
const WALK_MOST: usize = 5000;
const WALK_DEPTH: usize = 6;

const DECK: &str = include_str!("../../assets/forge/templates/deck.html");
const DECK_CSS: &str = include_str!("../../assets/forge/templates/deck.css");

/// A name in a folder's listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    /// What the Forge shows it as, None for a file it does not show.
    pub label: Option<&'static str>,
    /// Behind the toggle until the human asks for everything.
    pub quiet: bool,
}

/// What a file is called on the page, None when the Forge does not show
/// it. By its name alone: telling a deck from a page means reading it.
fn label(name: &str) -> Option<&'static str> {
    let kind = horadric_core::forge::kind_of_bytes(Path::new(name), &[])?;
    super::shows(kind).then_some(match kind {
        Kind::Markdown => "Markdown",
        Kind::Html | Kind::Deck => "HTML",
        Kind::Jsx => "Artefact",
        Kind::Pptx => "PowerPoint",
        Kind::Docx => "Word",
        Kind::Pdf => "PDF",
        Kind::Image => "Image",
        Kind::Svg => "SVG",
        Kind::Mermaid => "Diagram",
        Kind::Csv => "Table",
    })
}

/// A name kept out of sight: a dotfile, a dotfolder or a tucked folder.
fn tucked(name: &str) -> bool {
    name.starts_with('.') || TUCKED.iter().any(|t| t.eq_ignore_ascii_case(name))
}

/// The names in `dir`, folders first, each part in natural order, and
/// whether some were left out past [`MOST`]. A `flat` host lists no
/// folders, as it serves none. Links are left out: one can lead anywhere
/// on the disk, and a listing is never more than its own folder.
pub fn list(dir: &Path, flat: bool) -> io::Result<(Vec<Entry>, bool)> {
    let mut out = Vec::new();
    let mut cut = false;
    for e in std::fs::read_dir(dir)? {
        let Ok(e) = e else { continue };
        if out.len() >= MOST {
            cut = true;
            break;
        }
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if ft.is_dir() {
            if !flat {
                let quiet = tucked(&name);
                out.push(Entry {
                    name,
                    dir: true,
                    label: None,
                    quiet,
                });
            }
        } else {
            let label = label(&name);
            let quiet = label.is_none() || name.starts_with('.');
            out.push(Entry {
                name,
                dir: false,
                label,
                quiet,
            });
        }
    }
    sort(&mut out);
    Ok((out, cut))
}

fn sort(entries: &mut [Entry]) {
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| natural(&a.name, &b.name)));
}

/// Names compared as a person reads them: without case, and numbers by
/// their value, so `slide 2` comes before `slide 10`.
fn natural(a: &str, b: &str) -> Ordering {
    let (la, lb) = (a.to_lowercase(), b.to_lowercase());
    let (ca, cb) = (runs(&la), runs(&lb));
    for (x, y) in ca.iter().zip(&cb) {
        let by = match (x.0, y.0) {
            (true, true) => {
                let (tx, ty) = (x.1.trim_start_matches('0'), y.1.trim_start_matches('0'));
                tx.len().cmp(&ty.len()).then_with(|| tx.cmp(ty))
            }
            _ => x.1.cmp(y.1),
        };
        if by != Ordering::Equal {
            return by;
        }
    }
    ca.len().cmp(&cb.len()).then_with(|| a.cmp(b))
}

/// `s` cut into runs of digits and of everything else, each marked with
/// whether it is digits.
fn runs(s: &str) -> Vec<(bool, &str)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut digits = None;
    for (i, c) in s.char_indices() {
        let d = c.is_ascii_digit();
        if digits.is_some_and(|was| was != d) {
            out.push((digits.unwrap_or(false), &s[start..i]));
            start = i;
        }
        digits = Some(d);
    }
    if let Some(d) = digits {
        out.push((d, &s[start..]));
    }
    out
}

/// Whether `dir` is `root` or in it, links followed: an address can only
/// name paths under the root, but a link among them could lead out.
pub fn within(root: &Path, dir: &Path) -> bool {
    match (std::fs::canonicalize(root), std::fs::canonicalize(dir)) {
        (Ok(r), Ok(d)) => d.starts_with(&r),
        _ => false,
    }
}

/// The folders from `root` down to `path`, None when it is not in it.
fn segments(root: &Path, path: &Path) -> Option<Vec<String>> {
    if super::same(root, path) {
        return Some(Vec::new());
    }
    if !super::inside(root, path) {
        return None;
    }
    let r = root.to_string_lossy().replace('/', "\\");
    let p = path.to_string_lossy().replace('/', "\\");
    let rest = p.get(r.trim_end_matches('\\').len()..)?;
    Some(
        rest.split('\\')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// Whether a path below the root passes through a tucked folder.
fn under_tucked(segs: &[String]) -> bool {
    segs.iter()
        .take(segs.len().saturating_sub(1))
        .any(|s| tucked(s))
}

/// A recent file: its path under the root and when it was last written.
#[derive(Debug, Clone, PartialEq)]
pub struct Recent {
    pub segs: Vec<String>,
    pub at: Option<SystemTime>,
}

/// The files of `root` sessions wrote lately (`written`, newest last), or
/// when none of those are here, the newest the Forge shows found in a
/// bounded walk. At most [`RECENT`].
pub fn recent(root: &Path, written: &[PathBuf]) -> Vec<Recent> {
    let mut out: Vec<Recent> = Vec::new();
    for file in written.iter().rev() {
        if out.len() >= RECENT {
            break;
        }
        let Some(segs) = segments(root, file).filter(|s| !s.is_empty()) else {
            continue;
        };
        let name = segs.last().map(String::as_str).unwrap_or_default();
        if label(name).is_none() || under_tucked(&segs) || out.iter().any(|r| r.segs == segs) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(file) else { continue };
        if meta.is_file() {
            out.push(Recent {
                segs,
                at: meta.modified().ok(),
            });
        }
    }
    if out.is_empty() {
        out = newest(root);
    }
    out
}

/// The newest files the Forge shows under `root`, by when they were
/// written, from a walk of at most [`WALK_MOST`] names and [`WALK_DEPTH`]
/// folders deep that skips what is tucked away and every link.
fn newest(root: &Path) -> Vec<Recent> {
    let mut found: Vec<(SystemTime, Vec<String>)> = Vec::new();
    let mut stack = vec![(root.to_path_buf(), Vec::<String>::new())];
    let mut looked = 0;
    'walk: while let Some((dir, segs)) = stack.pop() {
        let Ok(names) = std::fs::read_dir(&dir) else { continue };
        for e in names.flatten() {
            looked += 1;
            if looked > WALK_MOST {
                break 'walk;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            if tucked(&name) {
                continue;
            }
            let mut here = segs.clone();
            here.push(name);
            if ft.is_dir() {
                if here.len() < WALK_DEPTH {
                    stack.push((e.path(), here));
                }
            } else if label(&here[here.len() - 1]).is_some() {
                if let Ok(at) = e.metadata().and_then(|m| m.modified()) {
                    found.push((at, here));
                }
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found
        .into_iter()
        .take(RECENT)
        .map(|(at, segs)| Recent { segs, at: Some(at) })
        .collect()
}

/// The page of the folder `dir` in `root`. Reads the disk, so it is for a
/// worker thread. `written` is what sessions wrote lately, newest last.
pub fn page(root: &Path, dir: &Path, flat: bool, written: &[PathBuf]) -> Reply {
    let segs = segments(root, dir).filter(|_| within(root, dir) && dir.is_dir());
    let Some(segs) = segs.filter(|s| !flat || s.is_empty()) else {
        return Reply::html("<!doctype html><title>Not found</title><p>No such folder here.".into())
            .status(404);
    };
    let top = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    let (entries, cut) = match list(dir, flat) {
        Ok(listed) => listed,
        Err(e) => {
            let text = format!("This folder cannot be read: {e}");
            return Reply::html(html(&top, &segs, &[], false, &[], &text));
        }
    };
    let recent = if segs.is_empty() {
        recent(root, written)
    } else {
        Vec::new()
    };
    Reply::html(html(&top, &segs, &entries, cut, &recent, ""))
}

fn esc(s: &str) -> String {
    super::runtime::attr(s)
}

/// An address from the host's top to `segs`, each encoded.
fn href(segs: &[String]) -> String {
    let mut out = String::from("/");
    out.push_str(&segs.iter().map(|s| encode(s)).collect::<Vec<_>>().join("/"));
    out
}

/// How long ago `at` was, in a few words.
fn ago(at: Option<SystemTime>, now: SystemTime) -> String {
    let Some(secs) = at.and_then(|a| now.duration_since(a).ok()).map(|d| d.as_secs()) else {
        return String::new();
    };
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86_400 => format!("{} h ago", secs / 3600),
        86_400..172_800 => "yesterday".into(),
        _ => format!("{} days ago", secs / 86_400),
    }
}

/// The page itself, from what was read: the folder's place under `top`,
/// its names, and on the top folder its recent files. `trouble` is said
/// in place of the listing when the folder could not be read.
fn html(top: &str, segs: &[String], entries: &[Entry], cut: bool, recent: &[Recent], trouble: &str) -> String {
    let title = segs.last().map_or(top, String::as_str);
    let mut b = String::with_capacity(4096 + entries.len() * 96);
    b.push_str("<!doctype html>\n<html><head><meta charset=\"utf-8\">");
    b.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    b.push_str(&format!("<title>{}</title>\n<style>{STYLE}</style></head>\n", esc(title)));
    b.push_str("<body><main class=\"forge-home\">\n<nav class=\"crumbs\">");
    b.push_str(&format!("<a href=\"/\">{}</a>", esc(top)));
    for i in 0..segs.len() {
        b.push_str("<span class=\"sep\">/</span>");
        b.push_str(&format!(
            "<a href=\"{}/\">{}</a>",
            esc(&href(&segs[..=i])),
            esc(&segs[i])
        ));
    }
    b.push_str("</nav>\n");

    b.push_str("<section class=\"new\"><span class=\"head\">New</span>");
    for (what, name) in [("md", "Markdown note"), ("deck", "HTML deck"), ("csv", "CSV table")] {
        b.push_str(&format!("<button type=\"button\" data-new=\"{what}\">{name}</button>"));
    }
    b.push_str("<span class=\"said\" id=\"said\"></span></section>\n");

    if !recent.is_empty() {
        let now = SystemTime::now();
        b.push_str("<section><h2>Recent</h2><ul class=\"list\">\n");
        for r in recent {
            let name = r.segs.last().map(String::as_str).unwrap_or_default();
            let folder = r.segs[..r.segs.len().saturating_sub(1)].join("/");
            b.push_str(&format!(
                "<li><a href=\"{}\">{}</a><span class=\"where\">{}</span><span class=\"kind\">{}</span><span class=\"when\">{}</span></li>\n",
                esc(&href(&r.segs)),
                esc(name),
                esc(&folder),
                label(name).unwrap_or_default(),
                ago(r.at, now),
            ));
        }
        b.push_str("</ul></section>\n");
    }

    let quiet = entries.iter().filter(|e| e.quiet).count();
    b.push_str("<section><div class=\"bar\"><h2>Files</h2>");
    if quiet > 0 {
        b.push_str(&format!(
            "<button type=\"button\" id=\"all\" data-show=\"Show {quiet} more\">Show {quiet} more</button>"
        ));
    }
    b.push_str("</div>\n");
    if !trouble.is_empty() {
        b.push_str(&format!("<p class=\"note\">{}</p>\n", esc(trouble)));
    } else if entries.iter().all(|e| e.quiet) {
        b.push_str("<p class=\"note none\">Nothing here the Forge shows yet.</p>\n");
    }
    b.push_str("<ul class=\"list\">\n");
    for e in entries {
        let class = match (e.dir, e.quiet) {
            (true, false) => "dir",
            (true, true) => "dir quiet",
            (false, false) => "file",
            (false, true) => "file quiet",
        };
        let link = encode(&e.name);
        if e.dir {
            b.push_str(&format!(
                "<li class=\"{class}\"><a href=\"{}/\">{}/</a></li>\n",
                esc(&link),
                esc(&e.name)
            ));
        } else if let Some(kind) = e.label {
            b.push_str(&format!(
                "<li class=\"{class}\"><a href=\"{}\">{}</a><span class=\"kind\">{kind}</span></li>\n",
                esc(&link),
                esc(&e.name)
            ));
        } else {
            b.push_str(&format!(
                "<li class=\"{class} other\"><span>{}</span></li>\n",
                esc(&e.name)
            ));
        }
    }
    b.push_str("</ul>\n");
    if cut {
        b.push_str(&format!("<p class=\"note\">Only the first {MOST} names are listed.</p>\n"));
    }
    b.push_str("</section></main>\n<script>");
    b.push_str(SCRIPT);
    b.push_str("</script>\n</body></html>\n");
    b
}

/// The palette is the markdown pages' (horadric-core's forge/html.rs), so
/// the home looks like the files it leads to.
const STYLE: &str = r#"
:root { color-scheme: light dark; --fg: #1f2328; --muted: #59636e; --bg: #ffffff;
  --line: #d1d9e0; --soft: #f6f8fa; --link: #0969da; }
@media (prefers-color-scheme: dark) {
  :root { --fg: #e6e1d8; --muted: #9a9389; --bg: #16130f; --line: #3a342c;
    --soft: #201c17; --link: #d9a75f; }
}
* { box-sizing: border-box; }
html { background: var(--bg); color: var(--fg); }
body { margin: 0; font: 15px/1.5 "Segoe UI", system-ui, -apple-system, sans-serif; }
main { max-width: 48rem; margin: 0 auto; padding: 1.5rem 1.5rem 4rem; }
a { color: var(--link); text-decoration: none; } a:hover { text-decoration: underline; }
.crumbs { font-size: 1.25em; font-weight: 600; margin-bottom: 1rem; overflow-wrap: anywhere; }
.crumbs .sep { color: var(--muted); margin: 0 0.35em; font-weight: 400; }
.crumbs a:last-child { color: var(--fg); }
h2 { font-size: 0.8em; font-weight: 600; text-transform: uppercase; letter-spacing: 0.06em;
  color: var(--muted); margin: 1.6rem 0 0.4rem; }
.new { display: flex; flex-wrap: wrap; align-items: center; gap: 0.5rem;
  padding: 0.6rem 0; border-bottom: 1px solid var(--line); }
.new .head { color: var(--muted); font-size: 0.9em; margin-right: 0.25rem; }
button { font: inherit; font-size: 0.9em; color: var(--fg); background: var(--soft);
  border: 1px solid var(--line); border-radius: 6px; padding: 0.2rem 0.7rem; cursor: pointer; }
button:hover { border-color: var(--muted); }
button:disabled { opacity: 0.5; cursor: default; }
.said { color: var(--muted); font-size: 0.9em; }
.bar { display: flex; align-items: baseline; justify-content: space-between; gap: 1rem; }
.bar button { font-size: 0.8em; }
ul.list { list-style: none; margin: 0; padding: 0; }
ul.list li { display: flex; align-items: baseline; gap: 0.75rem; padding: 0.3rem 0.5rem;
  border-radius: 4px; }
ul.list li:hover { background: var(--soft); }
ul.list li > a, ul.list li > span:first-child { flex: 1; min-width: 0; overflow-wrap: anywhere; }
li.dir > a { font-weight: 600; }
.kind, .where, .when { color: var(--muted); font-size: 0.85em; white-space: nowrap; }
.when { min-width: 5.5em; text-align: right; }
li.other { color: var(--muted); }
li.quiet { display: none; }
body.all li.quiet { display: flex; }
li.quiet > a, li.quiet > span { opacity: 0.7; }
.note { color: var(--muted); }
body.all .note.none { display: none; }
"#;

/// The page's own script: the New buttons ask Horadric for a file, and
/// the toggle shows what is tucked away, remembered per project.
const SCRIPT: &str = r#"
(() => {
  "use strict";
  const webview = window.chrome && window.chrome.webview;
  const said = document.getElementById("said");
  const news = Array.from(document.querySelectorAll("button[data-new]"));
  for (const b of news) {
    b.addEventListener("click", () => {
      if (!webview) { said.textContent = "Only inside Horadric."; return; }
      for (const n of news) n.disabled = true;
      said.textContent = "";
      webview.postMessage({ forge: "new", what: b.dataset.new });
    });
  }
  if (webview) {
    webview.addEventListener("message", (e) => {
      let m = e.data;
      if (typeof m === "string") { try { m = JSON.parse(m); } catch (_) { return; } }
      if (!m || m.forge !== "home-said") return;
      said.textContent = String(m.text || "");
      for (const n of news) n.disabled = false;
    });
  }
  const all = document.getElementById("all");
  const kept = "forge-home-all";
  let on = false;
  try { on = localStorage.getItem(kept) === "1"; } catch (_) { /* no storage here */ }
  const set = (v) => {
    on = v;
    document.body.classList.toggle("all", v);
    if (all) all.textContent = v ? "Show less" : all.dataset.show;
    try { localStorage.setItem(kept, v ? "1" : "0"); } catch (_) { /* no storage here */ }
  };
  if (all) {
    all.addEventListener("click", () => set(!on));
    set(on);
  }
})();
"#;

/// What a new file starts as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum New {
    Note,
    Deck,
    Table,
}

/// Makes a new file of `what` in `dir`, named so it takes no other's
/// place: `Untitled note.md`, then `Untitled note 2.md` and on. A deck
/// gets the house stylesheet beside it unless one is there already.
fn create(dir: &Path, what: New) -> io::Result<PathBuf> {
    let (stem, ext, body) = match what {
        New::Note => ("Untitled note", "md", "# Untitled note\n\n"),
        New::Deck => ("Untitled deck", "html", DECK),
        New::Table => ("Untitled table", "csv", "Name,Notes\n,\n"),
    };
    for n in 1..1000 {
        let name = if n == 1 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem} {n}.{ext}")
        };
        let path = dir.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                f.write_all(body.as_bytes())?;
                if what == New::Deck {
                    // Already there, it is the folder's own style: kept.
                    let _ = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(dir.join("deck.css"))
                        .and_then(|mut c| c.write_all(DECK_CSS.as_bytes()));
                }
                return Ok(path);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("there are too many untitled files here already"))
}

/// What a folder's page said, after its origin was checked. Only `new`:
/// a file made in the folder the tab shows (never one the page names, as
/// the page could name any) and the tab taken to it.
pub fn on_message(key: &str, tab: u64, msg: &Value) {
    if msg.get("forge").and_then(Value::as_str) != Some("new") {
        return;
    }
    let what = match msg.get("what").and_then(Value::as_str) {
        Some("md") => New::Note,
        Some("deck") => New::Deck,
        Some("csv") => New::Table,
        _ => return,
    };
    let found = super::TABS.with(|t| {
        t.borrow()
            .iter()
            .find(|f| f.key == key && f.id == tab && f.folder)
            .map(|f| (f.root.clone(), f.host.clone(), f.file.clone()))
    });
    let Some((root, host, dir)) = found else { return };
    let say = |text: String| super::post(key, tab, &json!({"forge": "home-said", "text": text}));
    if !within(&root, &dir) || dir.components().any(|c| c == Component::ParentDir) {
        return say("This folder is not the project's.".into());
    }
    match create(&dir, what) {
        Ok(file) => match super::serve::url_for(&host, &root, &file) {
            Some(url) => web::navigate(key, tab, &url),
            None => say(format!("Made {}.", file.display())),
        },
        Err(e) => say(format!("The file could not be made: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("forge-home-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn folders_come_first_and_numbers_read_as_numbers() {
        let dir = scratch("sort");
        for f in ["slide 10.md", "Slide 2.md", "b.csv", "notes.txt", ".env"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        for d in ["zeta", "Alpha", ".git", "node_modules", ".cache"] {
            std::fs::create_dir(dir.join(d)).unwrap();
        }
        let (entries, cut) = list(&dir, false).unwrap();
        assert!(!cut);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [".cache", ".git", "Alpha", "node_modules", "zeta", ".env", "b.csv", "notes.txt", "Slide 2.md", "slide 10.md"]
        );
        let quiet = |n: &str| entries.iter().find(|e| e.name == n).unwrap().quiet;
        assert!(quiet(".git") && quiet("node_modules") && quiet(".cache") && quiet(".env"));
        assert!(quiet("notes.txt"), "a file the Forge does not show waits behind the toggle");
        assert!(!quiet("Alpha") && !quiet("b.csv") && !quiet("Slide 2.md"));
        assert_eq!(entries.iter().find(|e| e.name == "b.csv").unwrap().label, Some("Table"));
        // A flat host serves no subfolder, so its page lists none.
        let (flat, _) = list(&dir, true).unwrap();
        assert!(flat.iter().all(|e| !e.dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn natural_order() {
        assert_eq!(natural("a2", "a10"), Ordering::Less);
        assert_eq!(natural("A", "b"), Ordering::Less);
        assert_eq!(natural("v007", "v8"), Ordering::Less);
        assert_eq!(natural("x", "x1"), Ordering::Less);
    }

    #[test]
    fn nothing_is_listed_outside_the_folder() {
        let dir = scratch("out");
        let root = dir.join("project");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(dir.join("secret.md"), "x").unwrap();
        assert!(within(&root, &root.join("docs")));
        assert!(!within(&root, &dir));
        assert!(!within(&root, &root.join("docs").join("..").join("..")));
        let page = page(&root, &dir, false, &[]);
        assert_eq!(page.status, 404);
        // A flat host has only its top folder's page.
        assert_eq!(super::page(&root, &root.join("docs"), true, &[]).status, 404);
        assert_eq!(super::page(&root, &root, true, &[]).status, 200);
        assert_eq!(segments(&root, &root.join("docs").join("a.md")).unwrap(), ["docs", "a.md"]);
        assert!(segments(&root, &dir.join("secret.md")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_page_links_relative_and_escapes_names() {
        let entries = vec![
            Entry { name: "a <b>".into(), dir: true, label: None, quiet: false },
            Entry { name: "my deck #1.html".into(), dir: false, label: Some("HTML"), quiet: false },
            Entry { name: "build.rs".into(), dir: false, label: None, quiet: true },
        ];
        let page = html("site", &["docs".into()], &entries, false, &[], "");
        assert!(page.contains("<a href=\"a%20%3Cb%3E/\">a &lt;b&gt;/</a>"));
        assert!(page.contains("href=\"my%20deck%20%231.html\""));
        assert!(page.contains("<a href=\"/docs/\">docs</a>"));
        assert!(page.contains("Show 1 more"));
        assert!(!page.contains("href=\"build.rs\""), "a file the Forge does not show is no link");
        for bad in ['\u{2014}', '\u{2013}'] {
            assert!(!page.contains(bad));
        }
    }

    #[test]
    fn recent_prefers_what_sessions_wrote_then_walks() {
        let root = scratch("recent");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::write(root.join("docs").join("plan.md"), "x").unwrap();
        std::fs::write(root.join("node_modules").join("x.md"), "x").unwrap();
        std::fs::write(root.join("main.rs"), "x").unwrap();
        let walked = recent(&root, &[]);
        assert_eq!(walked.len(), 1);
        assert_eq!(walked[0].segs, ["docs", "plan.md"]);
        std::fs::write(root.join("deck.html"), "x").unwrap();
        let written = [
            root.join("deck.html"),
            root.join("main.rs"),
            PathBuf::from(r"C:\elsewhere\x.md"),
            root.join("deck.html"),
        ];
        let listed = recent(&root, &written);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].segs, ["deck.html"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_new_file_never_takes_another_s_place() {
        let dir = scratch("new");
        let a = create(&dir, New::Note).unwrap();
        let b = create(&dir, New::Note).unwrap();
        assert_eq!(a.file_name().unwrap(), "Untitled note.md");
        assert_eq!(b.file_name().unwrap(), "Untitled note 2.md");
        std::fs::write(dir.join("deck.css"), "mine").unwrap();
        let deck = create(&dir, New::Deck).unwrap();
        assert!(std::fs::read_to_string(&deck).unwrap().contains("class=\"slide"));
        assert_eq!(std::fs::read_to_string(dir.join("deck.css")).unwrap(), "mine");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
