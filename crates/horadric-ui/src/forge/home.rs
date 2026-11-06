//! The Forge's home: the page of a folder of the project, to find a file
//! to work on, start a new one, or tidy what is there. HTML in Horadric's
//! own look, its theme and the project's colour, made by `serve` on a
//! worker thread for any page asked of a folder.
//!
//! Folders come first, then the files the Forge shows with what kind they
//! are. The rest (other files, dotfolders, `.git`, `node_modules`,
//! `target`) wait behind a toggle. The top folder's page also lists what
//! sessions wrote lately, and in a git repository every page lists what
//! changed since the last commit under it ([`git`]).
//!
//! What the page does to files is a message: Horadric checks it, does it
//! off the UI thread ([`files`]), never over another file, and sends the
//! page its listing again, so it never reloads under the human.

mod files;
mod git;

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

use horadric_core::forge::Kind;
use serde_json::{json, Value};

use super::serve::{encode, Reply};
use crate::sparks::RUNES;
use crate::theme::{self, Color};
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
const NOTE: &str = include_str!("../../assets/forge/templates/note.md");
const TABLE: &str = include_str!("../../assets/forge/templates/table.csv");

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

/// What a folder's page shows, read from the disk.
struct Shown {
    top: String,
    /// The folder's place under the top, a name for each folder down.
    segs: Vec<String>,
    flat: bool,
    entries: Vec<Entry>,
    cut: bool,
    recent: Vec<Recent>,
    /// Said in place of the listing when the folder could not be read.
    trouble: String,
    /// What changed since the last commit, None outside a repository.
    changes: Option<Vec<git::Change>>,
}

/// Reads what the page of `dir` shows, None when it is no folder of
/// `root` this host serves. Asks git, so it is for a worker thread.
fn read(root: &Path, dir: &Path, flat: bool, written: &[PathBuf]) -> Option<Shown> {
    let segs = segments(root, dir).filter(|_| within(root, dir) && dir.is_dir())?;
    if flat && !segs.is_empty() {
        return None;
    }
    let top = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    let (entries, cut, trouble) = match list(dir, flat) {
        Ok((entries, cut)) => (entries, cut, String::new()),
        Err(e) => (Vec::new(), false, format!("This folder cannot be read: {e}")),
    };
    let recent = if segs.is_empty() && trouble.is_empty() {
        recent(root, written)
    } else {
        Vec::new()
    };
    let changes = git::changes(root, flat).map(|(_, c)| c);
    Some(Shown {
        top,
        segs,
        flat,
        entries,
        cut,
        recent,
        trouble,
        changes,
    })
}

fn not_found() -> Reply {
    Reply::html("<!doctype html><title>Not found</title><p>No such folder here.".into()).status(404)
}

/// The page of the folder `dir` in `root`. Reads the disk, so it is for a
/// worker thread. `written` is what sessions wrote lately, newest last.
/// `accent` is the project's colour, read on the UI thread, where the
/// colours projects were given live.
#[cfg(test)]
pub fn page(root: &Path, dir: &Path, flat: bool, written: &[PathBuf], accent: Color) -> Reply {
    page_asked(root, dir, flat, written, accent, "")
}

/// [`page`], or what its address's `query` asks instead: `diff=<path>`
/// for what changed in one file since the last commit, `changes` for
/// every change. Asked of any folder's address, so the tab stays the
/// home's whichever page of it is shown.
pub fn page_asked(
    root: &Path,
    dir: &Path,
    flat: bool,
    written: &[PathBuf],
    accent: Color,
    query: &str,
) -> Reply {
    if let Some(asked) = git::asked(query) {
        // Only a change git names is shown, so a path here can reach no
        // further than git's own list; checked anyway, as cheap as it is.
        if let git::Asked::One(rel) = &asked {
            if rel.split('/').any(|s| !files::name_ok(s)) || (flat && rel.contains('/')) {
                return not_found();
            }
        }
        return git::page(root, flat, &asked, &super::serve::look(accent));
    }
    match read(root, dir, flat, written) {
        Some(shown) => Reply::html(html(&vars(accent), &shown)),
        None => not_found(),
    }
}

/// The address of the page of what changed in `file` since the last
/// commit, for the bar to offer. Git is not asked here, as that would
/// hold the UI thread: the page itself says when nothing changed or the
/// folder is in no repository. None for a file outside `root`.
pub fn diff_url(root: &Path, file: &Path) -> Option<String> {
    let segs = segments(root, file).filter(|s| !s.is_empty())?;
    // The host the tabs on this folder use: a flat one answers only its
    // own folder, and a host no tab has is answered by none.
    let host = super::TABS
        .with(|t| {
            t.borrow()
                .iter()
                .find(|f| super::same(&f.root, root))
                .map(|f| f.host.clone())
        })
        .unwrap_or_else(|| super::serve::host_for(root));
    if super::serve::is_flat(&host) && segs.len() > 1 {
        return None;
    }
    Some(format!("https://{host}{}", git::diff_href(&segs.join("/"))))
}

/// Horadric's look as CSS custom properties: the theme's colours and the
/// project's, so the Forge's own pages and chrome are drawn in what the
/// app is drawn in. Prefixed `--hd-`, since the chrome sets them beside a
/// page's own properties and must never take one of its names.
pub fn vars(accent: Color) -> String {
    let p = theme::palette();
    let (black, white) = (Color::rgb(0), Color::rgb(0xFFFFFF));
    // The Folio's reading ink: the terminal's text softened toward its
    // glass by the Folio's own amount, so a page reads as the Folio does.
    let read = p.term_fg.mix(p.term_bg, 0.09);
    let named = [
        ("plate", p.plate_top),
        ("plate-low", p.plate_bottom),
        // A key's face, side and lit top as the app draws its keys.
        ("key-top", p.surface.mix(white, 0.07)),
        ("key-low", p.surface.mix(black, 0.1)),
        ("key-side", p.surface.mix(black, 0.6)),
        ("surface", p.surface),
        ("well", p.well),
        ("screen", p.screen),
        ("lamp-off", p.lamp_off),
        ("text", p.text),
        ("dim", p.text_dim),
        ("legend", p.legend),
        ("read", read),
        ("cast", p.cast),
        ("bevel-light", p.bevel_light),
        ("bevel-shade", p.bevel_shade),
        ("hollow-shade", p.hollow_shade),
        ("hollow-light", p.hollow_light),
        ("engrave-dark", p.engrave_dark),
        ("engrave-light", p.engrave_light),
        ("hover", p.hover_fill),
        ("press", p.press_fill),
        ("accent", accent),
        ("accent-dim", accent.with_alpha(0.5)),
        ("accent-faint", accent.with_alpha(0.12)),
        // The runes' ink at rest, the Folio's: the reading ink taking some
        // of the project's colour.
        ("rune", read.mix(accent, 0.45)),
        ("waiting", p.waiting),
        ("error", p.error),
        ("error-dim", p.error.with_alpha(0.4)),
        ("done", p.done),
    ];
    named
        .iter()
        .map(|(n, c)| format!("--hd-{n}: {};", css_colour(*c)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn css_colour(c: Color) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    if c.a >= 1.0 {
        format!("#{:02x}{:02x}{:02x}", b(c.r), b(c.g), b(c.b))
    } else {
        format!("rgba({}, {}, {}, {:.3})", b(c.r), b(c.g), b(c.b), c.a)
    }
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

/// The runes the Folio drifts and the stone carries, as SVG symbols `#r0`
/// on, so the page cuts the same ones.
fn rune_symbols() -> String {
    let mut s = String::from("<svg class=\"sprite\" aria-hidden=\"true\">");
    for (i, strokes) in RUNES.iter().enumerate() {
        s.push_str(&format!(
            "<symbol id=\"r{i}\" viewBox=\"-0.35 -0.08 0.7 1.16\"><path vector-effect=\"non-scaling-stroke\" d=\""
        ));
        for line in strokes.iter() {
            for (j, (x, y)) in line.iter().enumerate() {
                s.push_str(&format!("{}{x} {y} ", if j == 0 { "M" } else { "L" }));
            }
        }
        s.push_str("\"/></symbol>");
    }
    s.push_str("</svg>\n");
    s
}

fn rune(i: usize) -> String {
    format!(
        "<svg class=\"rune\" aria-hidden=\"true\"><use href=\"#r{}\"/></svg>",
        i % RUNES.len()
    )
}

/// The rune a name is cut with: the same every visit, so a folder is
/// known by its mark as a project is by its colour.
fn rune_of(name: &str) -> usize {
    name.bytes()
        .fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize))
        % RUNES.len()
}

/// The most changes a folder's page lists; the page of every change has
/// the rest.
const CHANGES_SHOWN: usize = 200;

/// The page itself, from what was read. `look` is [`vars`].
fn html(look: &str, s: &Shown) -> String {
    let title = s.segs.last().map_or(s.top.as_str(), String::as_str);
    let mut b = String::with_capacity(12288 + s.entries.len() * 220);
    b.push_str("<!doctype html>\n<html><head><meta charset=\"utf-8\">");
    b.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    b.push_str(&format!(
        "<title>{}</title>\n<style>:root {{ {look} }}\n{STYLE}</style></head>\n",
        esc(title)
    ));
    b.push_str(&format!(
        "<body data-flat=\"{}\" data-dir=\"{}\">",
        if s.flat { "1" } else { "0" },
        esc(&s.segs.join("/"))
    ));
    b.push_str(&rune_symbols());
    b.push_str("<main class=\"forge-home\">\n<nav class=\"crumbs\">");
    b.push_str(&rune(rune_of(&s.top)));
    b.push_str(&format!("<a href=\"/\" data-rel=\"\">{}</a>", esc(&s.top)));
    for i in 0..s.segs.len() {
        b.push_str("<span class=\"sep\">/</span>");
        b.push_str(&format!(
            "<a href=\"{}/\" data-rel=\"{}\">{}</a>",
            esc(&href(&s.segs[..=i])),
            esc(&s.segs[..=i].join("/")),
            esc(&s.segs[i])
        ));
    }
    b.push_str("</nav>\n");

    b.push_str("<section class=\"new\"><span class=\"legend\">New</span>");
    for (what, name) in [("md", "Markdown note"), ("deck", "HTML deck"), ("csv", "CSV table")] {
        b.push_str(&format!("<button type=\"button\" data-new=\"{what}\">{name}</button>"));
    }
    if !s.flat {
        b.push_str("<button type=\"button\" data-op=\"mkdir\">Folder</button>");
    }
    b.push_str("<span class=\"work\" id=\"work\">");
    for i in [3, 7, 10] {
        b.push_str(&rune(i));
    }
    b.push_str("</span><span class=\"said\" id=\"said\"></span></section>\n");
    b.push_str(
        "<div class=\"ask\" id=\"ask\" hidden><span class=\"text\" id=\"ask-text\"></span>\
         <button type=\"button\" class=\"small danger\" id=\"ask-yes\">Delete</button>\
         <button type=\"button\" class=\"small\" id=\"ask-no\">Keep</button></div>\n",
    );
    b.push_str("<div id=\"listing\">");
    b.push_str(&listing(s));
    b.push_str("</div></main>\n");
    b.push_str(
        "<div class=\"menu\" id=\"menu\" role=\"menu\" hidden>\
         <button type=\"button\" data-act=\"open\">Open<kbd>Enter</kbd></button>\
         <button type=\"button\" data-act=\"rename\">Rename<kbd>F2</kbd></button>\
         <button type=\"button\" data-act=\"duplicate\">Duplicate<kbd>Ctrl+D</kbd></button>\
         <button type=\"button\" data-act=\"move\">Move to folder</button><hr>\
         <button type=\"button\" data-act=\"delete\" class=\"danger\">Delete<kbd>Del</kbd></button></div>\n\
         <div class=\"picker\" id=\"picker\" hidden><span class=\"legend\" id=\"pick-what\">Move to</span>\
         <input id=\"pick-filter\" type=\"text\" placeholder=\"Find a folder\" spellcheck=\"false\" autocomplete=\"off\">\
         <div class=\"screen\"><ul class=\"list\" id=\"pick-list\"></ul></div></div>\n",
    );
    b.push_str(&format!(
        "<div class=\"drop\" id=\"drop\"><span class=\"legend\">Drop to copy into {}</span></div>\n",
        esc(title)
    ));
    b.push_str("<script>");
    b.push_str(SCRIPT);
    b.push_str("</script>\n</body></html>\n");
    b
}

/// The part of the page that changes as files do: the changes, the
/// recent files and the listing. Sent again whole after each operation.
fn listing(s: &Shown) -> String {
    let title = s.segs.last().map_or(s.top.as_str(), String::as_str);
    let mut b = String::with_capacity(8192 + s.entries.len() * 220);
    // Changes are named from the top; the page lists those under it.
    let prefix = if s.segs.is_empty() {
        String::new()
    } else {
        format!("{}/", s.segs.join("/"))
    };
    let here: Vec<&git::Change> = s
        .changes
        .iter()
        .flatten()
        .filter(|c| c.rel.starts_with(&prefix))
        .collect();
    if !here.is_empty() {
        b.push_str(&format!(
            "<section><div class=\"bar\"><h2>Changes</h2><span class=\"count\">{}</span><span class=\"groove\"></span>\
             <a class=\"key small\" href=\"{}\">All changes</a></div><div class=\"screen\"><ul class=\"list\" id=\"changes\">\n",
            here.len(),
            git::ALL_HREF
        ));
        for c in here.iter().take(CHANGES_SHOWN) {
            let from = c
                .from
                .as_deref()
                .map(|f| format!("<span class=\"where\">from {}</span>", esc(f)))
                .unwrap_or_default();
            b.push_str(&format!(
                "<li class=\"change\"><span class=\"lamp {cl}\"></span><span class=\"tag {cl}\" title=\"{w}\">{l}</span>\
                 <a href=\"{}\">{}</a>{from}</li>\n",
                esc(&git::diff_href(&c.rel)),
                esc(&c.rel[prefix.len()..]),
                cl = c.state.class(),
                w = c.state.word(),
                l = c.state.letter(),
            ));
        }
        if here.len() > CHANGES_SHOWN {
            b.push_str(&format!(
                "<li class=\"more-changes\"><a href=\"{}\">{} more</a></li>\n",
                git::ALL_HREF,
                here.len() - CHANGES_SHOWN
            ));
        }
        b.push_str("</ul></div></section>\n");
    }

    if !s.recent.is_empty() {
        let now = SystemTime::now();
        b.push_str("<section><div class=\"bar\"><h2>Recent</h2><span class=\"groove\"></span></div>");
        b.push_str("<div class=\"screen\"><ul class=\"list\">\n");
        for r in &s.recent {
            let name = r.segs.last().map(String::as_str).unwrap_or_default();
            let folder = r.segs[..r.segs.len().saturating_sub(1)].join("/");
            let kind = label(name)
                .map(|k| format!("<span class=\"kind\">{k}</span>"))
                .unwrap_or_default();
            b.push_str(&format!(
                "<li class=\"file\"><span class=\"lamp\"></span><a href=\"{}\">{}</a><span class=\"where\">{}</span>{kind}<span class=\"when\">{}</span></li>\n",
                esc(&href(&r.segs)),
                esc(name),
                esc(&folder),
                ago(r.at, now),
            ));
        }
        b.push_str("</ul></div></section>\n");
    }

    // How each name listed changed: a file by its own change, a folder by
    // any change inside it.
    let mut marks: HashMap<String, Option<git::State>> = HashMap::new();
    for c in &here {
        let rest = &c.rel[prefix.len()..];
        match rest.split_once('/') {
            Some((dir, _)) => {
                marks.entry(dir.to_lowercase()).or_insert(None);
            }
            None => {
                marks.insert(rest.to_lowercase(), Some(c.state));
            }
        }
    }

    let quiet = s.entries.iter().filter(|e| e.quiet).count();
    b.push_str("<section><div class=\"bar\"><h2>Files</h2><span class=\"groove\"></span>");
    if quiet > 0 {
        b.push_str(&format!(
            "<button type=\"button\" class=\"small\" id=\"all\" data-show=\"Show {quiet} more\">Show {quiet} more</button>"
        ));
    }
    b.push_str("</div>\n<div class=\"screen\">\n");
    if !s.trouble.is_empty() {
        b.push_str(&format!(
            "<p class=\"note empty trouble\">{}<span>{}</span></p>\n",
            rune(2),
            esc(&s.trouble)
        ));
    } else if s.entries.iter().all(|e| e.quiet) {
        b.push_str(&format!(
            "<p class=\"note empty none\">{}<span>Nothing here the Forge shows yet. Drop files here to copy them in.</span></p>\n",
            rune(rune_of(title))
        ));
    }
    b.push_str("<ul class=\"list\" id=\"files\">\n");
    for e in &s.entries {
        let class = match (e.dir, e.quiet) {
            (true, false) => "dir",
            (true, true) => "dir quiet",
            (false, false) => "file",
            (false, true) => "file quiet",
        };
        let link = encode(&e.name);
        let mark = marks.get(&e.name.to_lowercase()).copied();
        b.push_str(&format!(
            "<li class=\"{class}{}\" data-name=\"{}\" data-kind=\"{}\" draggable=\"true\">",
            if e.label.is_none() && !e.dir { " other" } else { "" },
            esc(&e.name),
            if e.dir { "dir" } else { "file" }
        ));
        if e.dir {
            let inside = if mark.is_some() {
                "<span class=\"inside\" title=\"Changed inside\"></span>"
            } else {
                ""
            };
            b.push_str(&format!(
                "{}<a href=\"{}/\" draggable=\"false\">{}/</a>{inside}",
                rune(rune_of(&e.name)),
                esc(&link),
                esc(&e.name)
            ));
        } else {
            let state = mark.flatten();
            let lamp = state.map_or(String::new(), |st| format!(" {}", st.class()));
            b.push_str(&format!("<span class=\"lamp{lamp}\"></span>"));
            match e.label {
                Some(_) => b.push_str(&format!(
                    "<a href=\"{}\" draggable=\"false\">{}</a>",
                    esc(&link),
                    esc(&e.name)
                )),
                None => b.push_str(&format!("<span class=\"name\">{}</span>", esc(&e.name))),
            }
            if let Some(st) = state {
                b.push_str(&format!(
                    "<a class=\"tag {}\" href=\"{}\" title=\"{}, show what changed\" draggable=\"false\">{}</a>",
                    st.class(),
                    esc(&git::diff_href(&format!("{prefix}{}", e.name))),
                    st.word(),
                    st.letter()
                ));
            }
            if let Some(kind) = e.label {
                b.push_str(&format!("<span class=\"kind\">{kind}</span>"));
            }
        }
        b.push_str(&format!(
            "<button type=\"button\" class=\"more\" tabindex=\"-1\" aria-label=\"What to do with {}\">&middot;&middot;&middot;</button></li>\n",
            esc(&e.name)
        ));
    }
    b.push_str("</ul>\n");
    if s.cut {
        b.push_str(&format!("<p class=\"note\">Only the first {MOST} names are listed.</p>\n"));
    }
    b.push_str("</div></section>\n");
    b
}

/// The page in Horadric's skeuomorph: a matte faceplate with a seam cut
/// round it, keys that stand off it and go down when pressed, listings on
/// screens sunk into it, legends engraved. Folders are marked with runes
/// cut in the project's colour and files with unlit lamps, which light on
/// the row the pointer is on. The colours are [`vars`], set before this.
const STYLE: &str = r#"
:root { color-scheme: dark;
  --hd-ui: "Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif;
  --hd-mono: "Cascadia Mono", "Cascadia Code", Consolas, monospace; }
* { box-sizing: border-box; }
html { background: var(--hd-plate); color: var(--hd-text); }
body { margin: 0; min-height: 100vh; font: 14px/1.5 var(--hd-ui); }
/* The seam every Horadric window has inside its edge: a dark cut with
   the light catching the lip under it. */
body::before { content: ""; position: fixed; inset: 5px; border-radius: 9px;
  pointer-events: none; border: 1px solid var(--hd-engrave-dark);
  box-shadow: 0 1px 0 var(--hd-engrave-light), inset 0 1px 0 var(--hd-engrave-light); }
.sprite { position: absolute; width: 0; height: 0; overflow: hidden; }
/* The Folio's reading column. */
main { max-width: 724px; margin: 0 auto; padding: 36px 22px 64px; }
a { color: inherit; text-decoration: none; }
:focus-visible { outline: 1px solid var(--hd-accent); outline-offset: 2px; border-radius: 4px; }

svg.rune { flex: none; width: 10px; height: 17px; fill: none; stroke: currentColor;
  stroke-width: 1.5px; stroke-linecap: round; stroke-linejoin: round; overflow: visible; }

/* Engraved: the plate's shade above the letters, its lit lip below. */
.legend, h2 { margin: 0; font: 600 11px/1 var(--hd-ui); letter-spacing: 0.14em;
  text-transform: uppercase; color: var(--hd-legend);
  text-shadow: 0 -1px 0 var(--hd-engrave-dark), 0 1px 0 var(--hd-engrave-light); }

.crumbs { display: flex; flex-wrap: wrap; align-items: center; row-gap: 4px;
  font: 16px/1.3 var(--hd-mono); color: var(--hd-dim); overflow-wrap: anywhere; }
.crumbs > svg.rune { width: 11px; height: 18px; margin-right: 12px; color: var(--hd-accent);
  stroke-width: 1.8px; }
.crumbs a:hover { color: var(--hd-text); }
.crumbs a:last-child { color: var(--hd-text); font-weight: 600; }
.crumbs .sep { color: var(--hd-legend); margin: 0 8px; }

.new { display: flex; flex-wrap: wrap; align-items: center; gap: 10px 10px; margin-top: 24px; }
.new .legend { margin-right: 6px; }

/* Keys: the face lit from above with a bevel on its top edge, its side
   showing under it and its shadow on the plate. Pressed, it goes down to
   the plate; latched, it stays there while Horadric is at it. */
button, a.key { font: 500 13px/1 var(--hd-ui); color: var(--hd-text); cursor: pointer;
  display: inline-block; border: 0; border-radius: 6px; padding: 8px 14px 9px; margin: 0 0 3px;
  background: linear-gradient(var(--hd-key-top), var(--hd-key-low));
  box-shadow: inset 0 1px 0 var(--hd-bevel-light), inset 0 -1px 0 var(--hd-bevel-shade),
    0 3px 0 var(--hd-key-side), 0 4px 8px var(--hd-cast);
  transition: transform 70ms ease, box-shadow 70ms ease; }
button:hover, a.key:hover { background: linear-gradient(var(--hd-hover), var(--hd-hover)),
  linear-gradient(var(--hd-key-top), var(--hd-key-low)); }
button:active:not(:disabled), a.key:active, button:disabled { transform: translateY(2px);
  box-shadow: inset 0 1px 0 var(--hd-bevel-light), 0 1px 0 var(--hd-key-side),
    0 1px 3px var(--hd-cast); }
button:disabled { color: var(--hd-dim); cursor: default; }
button.small, a.key.small { font-size: 11px; padding: 5px 10px 6px; }

/* Work going on: a few runes lighting in turn, in the project's colour. */
.work { display: none; align-items: center; gap: 5px; color: var(--hd-accent); }
.work.on { display: inline-flex; }
.work svg.rune { animation: kindle 1.2s ease-in-out infinite; opacity: 0.25; }
.work svg.rune:nth-child(2) { animation-delay: 0.2s; }
.work svg.rune:nth-child(3) { animation-delay: 0.4s; }
@keyframes kindle { 40% { opacity: 1; } }
.said { color: var(--hd-dim); font-size: 13px; }

.bar { display: flex; align-items: center; gap: 14px; margin: 30px 2px 10px; min-height: 28px; }
/* A groove across the plate to the end of the row. */
.groove { flex: 1; height: 2px; border-top: 1px solid var(--hd-engrave-dark);
  border-bottom: 1px solid var(--hd-engrave-light); }

/* A screen sunk into the plate: shade under its top edge, the plate's lit
   lip along its bottom. */
.screen { background: var(--hd-screen); border-radius: 8px; padding: 5px;
  box-shadow: inset 0 2px 6px var(--hd-hollow-shade), inset 0 -1px 0 var(--hd-hollow-light),
    0 1px 0 var(--hd-engrave-light); }
ul.list { list-style: none; margin: 0; padding: 0; }
ul.list li { display: flex; align-items: center; gap: 12px; min-height: 34px;
  padding: 6px 10px; border-radius: 5px; color: var(--hd-read); }
ul.list li:hover { background: var(--hd-hover); }
ul.list li > a:not(.tag), ul.list li > .name { flex: 1; min-width: 0; overflow-wrap: anywhere; }
li.dir > a { color: var(--hd-text); font-weight: 600; }
li.dir > svg.rune { margin: 0 1px; color: var(--hd-rune); }
li.dir:hover > svg.rune { color: var(--hd-accent); }
/* An unlit lamp sunk in the glass; the row the pointer is on lights it. */
.lamp { flex: none; display: flex; justify-content: center; width: 11px; }
.lamp::before { content: ""; width: 6px; height: 6px; border-radius: 50%;
  background: var(--hd-lamp-off); box-shadow: inset 0 1px 1px var(--hd-hollow-shade); }
li:hover > .lamp::before { background: var(--hd-accent); box-shadow: none; }
.kind { flex: none; font: 600 10px/1 var(--hd-ui); letter-spacing: 0.1em;
  text-transform: uppercase; color: var(--hd-legend); padding: 4px 7px 3px;
  border-radius: 4px; border: 1px solid var(--hd-engrave-light); }
.where { flex: 0 1 auto; min-width: 0; max-width: 40%; overflow: hidden; text-overflow: ellipsis;
  white-space: nowrap; color: var(--hd-dim); font: 12px/1 var(--hd-mono); }
.when { flex: none; min-width: 6em; text-align: right; color: var(--hd-dim);
  font-size: 12px; font-variant-numeric: tabular-nums; }
li.other .name { color: var(--hd-dim); }
ul.list li.quiet { display: none; }
body.all ul.list li.quiet { display: flex; }
li.quiet > a, li.quiet > .name, li.quiet > svg.rune { opacity: 0.6; }
.note { margin: 0; padding: 10px; color: var(--hd-dim); font-size: 13px; }
/* Nothing to list: one unlit rune on the glass. */
.empty { display: flex; flex-direction: column; align-items: center; gap: 12px;
  padding: 34px 10px 30px; text-align: center; }
.empty svg.rune { width: 14px; height: 24px; stroke-width: 2px; color: var(--hd-lamp-off); }
.trouble svg.rune { color: var(--hd-error); }
.trouble span { color: var(--hd-read); }
body.all .note.none { display: none; }

[hidden] { display: none !important; }
.said.bad { color: var(--hd-error); }
.count { font: 600 11px/1 var(--hd-mono); color: var(--hd-legend); }

/* How a file changed since the last commit: its lamp lit in that colour,
   kept lit under the pointer, and a letter cut beside its name. */
.st-m { --st: var(--hd-waiting); } .st-a, .st-u { --st: var(--hd-done); }
.st-d, .st-c { --st: var(--hd-error); } .st-r { --st: var(--hd-accent); }
.lamp[class*="st-"]::before, li:hover > .lamp[class*="st-"]::before { background: var(--st); box-shadow: none; }
.tag { flex: none; font: 700 10px/1 var(--hd-ui); padding: 3px 5px 2px; border-radius: 4px;
  color: var(--st); border: 1px solid var(--st); }
a.tag:hover { background: var(--hd-hover); }
/* A folder with a change somewhere inside: one small lamp after its name. */
.inside { flex: none; width: 5px; height: 5px; border-radius: 50%; background: var(--hd-waiting); }
ul.list li > a + .inside { margin-left: -6px; }
li.change .where { max-width: 50%; }
li.more-changes a { color: var(--hd-dim); }

/* The row the keys act on: the pointer's tint kept, a thin rule in the
   project's colour at its edge. */
ul.list li.sel { background: var(--hd-hover); box-shadow: inset 2px 0 0 var(--hd-accent); }
/* Each row's key, shown only on the row the pointer or the keys are on. */
li > button.more { flex: none; visibility: hidden; margin: 0 0 2px; padding: 2px 8px 6px;
  font: 700 13px/1 var(--hd-ui); letter-spacing: 1px; color: var(--hd-dim); }
li:hover > button.more, li.sel > button.more { visibility: visible; }
/* Where a row being dragged would land. */
ul.list li.over { background: var(--hd-accent-faint); box-shadow: inset 0 0 0 1px var(--hd-accent-dim); }
.crumbs a.over { color: var(--hd-accent); }
li.dragged { opacity: 0.5; }
/* A name being changed: a field sunk into the glass where it stood. */
input.rename, .picker input { font: inherit; color: var(--hd-text); background: var(--hd-well);
  border: 0; border-radius: 4px; padding: 4px 8px; outline: 1px solid var(--hd-accent-dim);
  box-shadow: inset 0 1px 3px var(--hd-hollow-shade); }
input.rename { flex: 1; min-width: 0; margin: -4px 0; }
input.rename::selection, .picker input::selection { background: var(--hd-accent-dim); color: var(--hd-text); }

/* The row's menu: a small panel standing off the plate. */
.menu { position: fixed; z-index: 10; min-width: 200px; padding: 5px; border-radius: 8px;
  background: var(--hd-surface); box-shadow: inset 0 1px 0 var(--hd-bevel-light),
    0 0 0 1px var(--hd-engrave-dark), 0 10px 26px var(--hd-cast); }
.menu button { display: flex; width: 100%; justify-content: space-between; align-items: center; gap: 22px;
  margin: 0; padding: 8px 10px; border-radius: 5px; font-weight: 400; text-align: left;
  background: none; box-shadow: none; transform: none; }
.menu button:hover, .menu button:focus-visible { background: var(--hd-hover); outline: none; }
.menu kbd { font: 11px/1 var(--hd-mono); color: var(--hd-legend); }
.menu hr { border: 0; height: 2px; margin: 4px 6px; border-top: 1px solid var(--hd-engrave-dark);
  border-bottom: 1px solid var(--hd-engrave-light); }
.danger, .menu .danger { color: var(--hd-error); }

/* Where a row goes: the project's folders on a screen, found by typing. */
.picker { position: fixed; z-index: 10; left: 50%; top: 12vh; transform: translateX(-50%);
  width: min(480px, calc(100vw - 32px)); display: flex; flex-direction: column; gap: 10px;
  padding: 14px; border-radius: 10px; background: var(--hd-plate);
  box-shadow: inset 0 1px 0 var(--hd-bevel-light), 0 0 0 1px var(--hd-engrave-dark), 0 14px 36px var(--hd-cast); }
.picker ul { max-height: 46vh; overflow: auto; }
.picker li { cursor: pointer; font: 13px/1.4 var(--hd-mono); }
.picker li.top { font-family: var(--hd-ui); font-weight: 600; color: var(--hd-text); }

/* Asking before a delete: a strip of glass with the question and keys. */
.ask { display: flex; align-items: center; gap: 10px; margin-top: 18px; padding: 8px 10px 8px 14px;
  border-radius: 8px; background: var(--hd-screen);
  box-shadow: inset 0 2px 6px var(--hd-hollow-shade), inset 0 -1px 0 var(--hd-hollow-light); }
.ask .text { flex: 1; color: var(--hd-read); overflow-wrap: anywhere; }

/* Files from Explorer held over the page: the window's seam traced in the
   project's colour, and a legend saying where they go. */
.drop { display: none; position: fixed; inset: 5px; z-index: 20; border-radius: 9px;
  border: 1px dashed var(--hd-accent); background: color-mix(in srgb, var(--hd-plate) 82%, transparent);
  align-items: center; justify-content: center; pointer-events: none; }
.drop.on { display: flex; }
.drop .legend { font-size: 12px; color: var(--hd-text); }

@media (prefers-reduced-motion: reduce) {
  .work svg.rune { animation: none; opacity: 1; }
  button { transition: none; }
}
"#;

/// The page's own script. The New keys ask Horadric for a file; the rows
/// are selected with the pointer or the arrows and acted on with their
/// menu, their keys, or by dragging one onto a folder; files dropped from
/// Explorer are sent to be kept here. Every change is Horadric's to make:
/// the page asks, and puts in the listing it is sent back. The toggle
/// shows what is tucked away, remembered per project.
const SCRIPT: &str = r##"
(() => {
  "use strict";
  const webview = window.chrome && window.chrome.webview;
  const $ = (id) => document.getElementById(id);
  const said = $("said"), work = $("work"), listing = $("listing");
  const menu = $("menu"), picker = $("picker"), ask = $("ask"), drop = $("drop");
  const pickList = $("pick-list"), pickFilter = $("pick-filter");
  const flat = document.body.dataset.flat === "1";
  const here = document.body.dataset.dir;
  const MINE = "application/x-forge-name";
  // Sent in parts small enough for one message each, a multiple of three
  // bytes so each part encodes on its own.
  const PART = 3 * 1024 * 1024;
  const LARGEST = 100 * 1024 * 1024;

  let seq = 0;
  const waiting = new Map();
  let current = null;

  const say = (text, bad) => { said.textContent = text || ""; said.classList.toggle("bad", !!bad); };
  const busy = () => work.classList.toggle("on", waiting.size > 0);
  const send = (op, fields, then) => {
    if (!webview) { say("Only inside Horadric.", true); return; }
    const n = ++seq;
    waiting.set(n, then || null);
    busy();
    webview.postMessage(Object.assign({ forge: "home", op, seq: n }, fields || {}));
  };

  const news = Array.from(document.querySelectorAll("button[data-new]"));
  for (const b of news) {
    b.addEventListener("click", () => {
      if (!webview) { say("Only inside Horadric.", true); return; }
      for (const n of news) n.disabled = true;
      say("");
      work.classList.add("on");
      webview.postMessage({ forge: "new", what: b.dataset.new });
    });
  }
  const mkdir = document.querySelector("button[data-op=mkdir]");
  if (mkdir) mkdir.addEventListener("click", () => send("mkdir", {}, (m) => {
    const li = m.ok && m.name && rowOf(m.name);
    if (li) rename(li);
  }));

  // Rows.
  const shown = (li) => li.offsetParent !== null;
  const rows = () => Array.from(listing.querySelectorAll("li")).filter((li) => shown(li) && li.querySelector("a, .name"));
  const rowOf = (name) => Array.from(listing.querySelectorAll("#files li[data-name]")).find((li) => li.dataset.name === name) || null;
  const relOf = (name) => (here ? here + "/" : "") + name;
  const select = (li, scroll) => {
    if (current) current.classList.remove("sel");
    current = li || null;
    if (!current) return;
    current.classList.add("sel");
    if (scroll) current.scrollIntoView({ block: "nearest" });
  };
  const open = (li) => {
    const a = li && li.querySelector("a:not(.tag)");
    if (a) location.href = a.href;
  };

  // What the toggle shows, kept across visits.
  const kept = "forge-home-all";
  let all = false;
  try { all = localStorage.getItem(kept) === "1"; } catch (_) { /* no storage here */ }
  const showAll = (v) => {
    all = v;
    document.body.classList.toggle("all", v);
    const b = $("all");
    if (b) b.textContent = v ? "Show less" : b.dataset.show;
    try { localStorage.setItem(kept, v ? "1" : "0"); } catch (_) { /* no storage here */ }
  };
  showAll(all);

  // The listing again, from Horadric, keeping the row the keys were on.
  const swap = (html) => {
    if (listing.querySelector("input.rename")) return;
    const name = current && current.dataset.name;
    listing.innerHTML = html;
    current = null;
    showAll(all);
    if (name) select(rowOf(name));
  };

  if (webview) {
    webview.addEventListener("message", (e) => {
      let m = e.data;
      if (typeof m === "string") { try { m = JSON.parse(m); } catch (_) { return; } }
      if (!m) return;
      if (m.forge === "home-said") {
        say(m.text);
        work.classList.remove("on");
        for (const n of news) n.disabled = false;
        return;
      }
      if (m.forge !== "home-did") return;
      const then = waiting.get(m.seq);
      waiting.delete(m.seq);
      busy();
      if (typeof m.listing === "string") swap(m.listing);
      if (m.seq) say(m.text, !m.ok);
      else if (!m.ok && m.text) say(m.text, true);
      if (m.ok && m.name) select(rowOf(m.name), true);
      if (then) then(m);
    });
  }

  // Seen again after a while away: listed again, as files and git move on
  // while the page waits.
  let listed = Date.now();
  const relist = () => {
    if (!webview || document.hidden || Date.now() - listed < 1500) return;
    listed = Date.now();
    webview.postMessage({ forge: "home", op: "list", seq: 0 });
  };
  window.addEventListener("focus", relist);
  document.addEventListener("visibilitychange", relist);

  // What can be done to a row.
  const rename = (li) => {
    const name = li.dataset.name;
    const label = li.querySelector("a:not(.tag), .name");
    if (!label || li.querySelector("input.rename")) return;
    select(li);
    const input = document.createElement("input");
    input.className = "rename";
    input.value = name;
    input.spellcheck = false;
    label.hidden = true;
    label.after(input);
    li.draggable = false;
    input.focus();
    const dot = li.dataset.kind === "dir" ? -1 : name.lastIndexOf(".");
    input.setSelectionRange(0, dot > 0 ? dot : name.length);
    let done = false;
    const finish = (keep) => {
      if (done) return;
      done = true;
      const to = input.value.trim();
      input.remove();
      label.hidden = false;
      li.draggable = true;
      if (keep && to && to !== name) send("rename", { name, to });
    };
    input.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Enter") { e.preventDefault(); finish(true); }
      else if (e.key === "Escape") { e.preventDefault(); finish(false); }
    });
    input.addEventListener("blur", () => finish(true));
  };
  const duplicate = (li) => send("duplicate", { name: li.dataset.name });
  const remove = (li) => {
    const name = li.dataset.name;
    $("ask-text").textContent = "Move " + name + (li.dataset.kind === "dir" ? "/ and all in it" : "") + " to the Recycle Bin?";
    ask.dataset.name = name;
    ask.hidden = false;
    $("ask-yes").focus();
  };
  $("ask-yes").addEventListener("click", () => {
    ask.hidden = true;
    send("delete", { names: [ask.dataset.name] });
  });
  $("ask-no").addEventListener("click", () => { ask.hidden = true; });

  // The folder picker, for a move without dragging.
  let moving = null, folders = [], picked = 0;
  const fill = () => {
    const q = pickFilter.value.trim().toLowerCase();
    const from = relOf(moving.name);
    const fits = folders.filter((f) =>
      f !== here && f !== from && !f.startsWith(from + "/") && (!q || f.toLowerCase().includes(q)));
    pickList.textContent = "";
    fits.slice(0, 300).forEach((f, i) => {
      const li = document.createElement("li");
      li.dataset.rel = f;
      li.textContent = f || document.querySelector(".crumbs a").textContent;
      if (!f) li.classList.add("top");
      if (i === picked) li.classList.add("sel");
      pickList.appendChild(li);
    });
    if (!fits.length) {
      const li = document.createElement("li");
      li.className = "other";
      li.textContent = "No folder by that name.";
      pickList.appendChild(li);
    }
  };
  const pick = (li) => {
    if (!li || li.dataset.rel === undefined) return;
    const name = moving.name;
    closeAll();
    send("move", { name, to: li.dataset.rel });
  };
  const move = (li) => {
    if (flat) return;
    moving = { name: li.dataset.name };
    $("pick-what").textContent = "Move " + li.dataset.name + " to";
    send("folders", {}, (m) => {
      if (!m.ok || !moving) return;
      folders = m.folders || [];
      picked = 0;
      pickFilter.value = "";
      picker.hidden = false;
      fill();
      pickFilter.focus();
    });
  };
  pickFilter.addEventListener("input", () => { picked = 0; fill(); });
  pickFilter.addEventListener("keydown", (e) => {
    e.stopPropagation();
    const items = Array.from(pickList.querySelectorAll("li[data-rel]"));
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      picked = Math.max(0, Math.min(items.length - 1, picked + (e.key === "ArrowDown" ? 1 : -1)));
      items.forEach((li, i) => li.classList.toggle("sel", i === picked));
      if (items[picked]) items[picked].scrollIntoView({ block: "nearest" });
    } else if (e.key === "Enter") {
      e.preventDefault();
      pick(items[picked]);
    } else if (e.key === "Escape") {
      e.preventDefault();
      closeAll();
    }
  });
  pickList.addEventListener("click", (e) => pick(e.target.closest("li[data-rel]")));

  // The row's menu, from its key or a right click.
  let menuRow = null;
  const closeAll = () => {
    menu.hidden = true;
    picker.hidden = true;
    menuRow = null;
    moving = null;
  };
  const openMenu = (li, x, y) => {
    select(li);
    menuRow = li;
    menu.querySelector("[data-act=move]").hidden = flat;
    menu.querySelector("[data-act=open]").hidden = !li.querySelector("a:not(.tag)");
    menu.hidden = false;
    const r = menu.getBoundingClientRect();
    menu.style.left = Math.max(8, Math.min(x, innerWidth - r.width - 8)) + "px";
    menu.style.top = Math.max(8, Math.min(y, innerHeight - r.height - 8)) + "px";
    menu.querySelector("button:not([hidden])").focus();
  };
  menu.addEventListener("click", (e) => {
    const b = e.target.closest("button[data-act]");
    const li = menuRow;
    if (!b || !li) return;
    closeAll();
    ({ open, rename, duplicate, move, delete: remove })[b.dataset.act](li);
  });
  menu.addEventListener("keydown", (e) => {
    const items = Array.from(menu.querySelectorAll("button:not([hidden])"));
    const at = items.indexOf(document.activeElement);
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      e.stopPropagation();
      const n = (at + (e.key === "ArrowDown" ? 1 : items.length - 1)) % items.length;
      items[n].focus();
    }
  });
  listing.addEventListener("click", (e) => {
    const more = e.target.closest("button.more");
    const li = e.target.closest("li");
    if (more && li) {
      const r = more.getBoundingClientRect();
      openMenu(li, r.right - 200, r.bottom + 4);
      return;
    }
    if (li && !e.target.closest("a, input")) select(li);
  });
  listing.addEventListener("contextmenu", (e) => {
    const li = e.target.closest("#files li[data-name]");
    if (!li || li.querySelector("input.rename")) return;
    e.preventDefault();
    openMenu(li, e.clientX, e.clientY);
  });
  listing.addEventListener("dblclick", (e) => {
    const li = e.target.closest("li");
    if (li && !e.target.closest("a, input, button")) open(li);
  });
  document.addEventListener("mousedown", (e) => {
    if (!menu.hidden && !menu.contains(e.target)) menu.hidden = true;
    if (!picker.hidden && !picker.contains(e.target)) closeAll();
  });
  listing.addEventListener("click", (e) => {
    if (e.target.id === "all") showAll(!all);
  });

  // The keys.
  document.addEventListener("keydown", (e) => {
    if (e.target.closest && e.target.closest("input")) return;
    if (!menu.hidden || !picker.hidden) {
      if (e.key === "Escape") { e.preventDefault(); closeAll(); if (current) current.focus?.(); }
      return;
    }
    if (!ask.hidden) {
      if (e.key === "Escape") { e.preventDefault(); ask.hidden = true; }
      return;
    }
    const list = rows();
    const at = current ? list.indexOf(current) : -1;
    const named = current && current.dataset.name !== undefined;
    const ctrl = e.ctrlKey || e.metaKey;
    if (e.key === "ArrowDown" && !ctrl) select(list[Math.min(at + 1, list.length - 1)], true);
    else if (e.key === "ArrowUp" && !ctrl) select(list[Math.max(at - 1, 0)], true);
    else if (e.key === "Home" && list.length) select(list[0], true);
    else if (e.key === "End" && list.length) select(list[list.length - 1], true);
    else if (e.key === "Enter" && current) open(current);
    else if (e.key === "F2" && named) rename(current);
    else if (e.key === "Delete" && named) remove(current);
    else if ((e.key === "d" || e.key === "D") && ctrl && named) duplicate(current);
    else if (e.key === "ContextMenu" && named) {
      const r = current.getBoundingClientRect();
      openMenu(current, r.left + 40, r.bottom);
    } else if (e.key === "Backspace" || (e.key === "ArrowUp" && e.altKey)) {
      const up = Array.from(document.querySelectorAll(".crumbs a"));
      if (up.length > 1) location.href = up[up.length - 2].href;
      else return;
    } else if (e.key === "Escape") select(null);
    else return;
    e.preventDefault();
  });

  // Dragging a row onto a folder, or onto a folder above in the trail.
  const targetOf = (e) => {
    const t = e.target.closest && e.target.closest("#files li.dir[data-name], .crumbs a[data-rel]");
    if (!t) return null;
    const rel = t.matches("a") ? t.dataset.rel : relOf(t.dataset.name);
    return rel === here || (dragging && (rel === relOf(dragging) || rel.startsWith(relOf(dragging) + "/"))) ? null : { el: t, rel };
  };
  let dragging = null, over = null;
  const lit = (el) => {
    if (over && over !== el) over.classList.remove("over");
    over = el;
    if (over) over.classList.add("over");
  };
  listing.addEventListener("dragstart", (e) => {
    const li = e.target.closest && e.target.closest("#files li[data-name]");
    if (!li || flat) { e.preventDefault(); return; }
    dragging = li.dataset.name;
    li.classList.add("dragged");
    e.dataTransfer.setData(MINE, dragging);
    e.dataTransfer.effectAllowed = "move";
  });
  listing.addEventListener("dragend", (e) => {
    const li = e.target.closest && e.target.closest("li");
    if (li) li.classList.remove("dragged");
    dragging = null;
    lit(null);
  });
  document.addEventListener("dragover", (e) => {
    const types = Array.from(e.dataTransfer.types || []);
    if (types.includes(MINE)) {
      const t = targetOf(e);
      lit(t && t.el);
      if (t) { e.preventDefault(); e.dataTransfer.dropEffect = "move"; }
      return;
    }
    if (types.includes("Files")) {
      e.preventDefault();
      e.dataTransfer.dropEffect = "copy";
      drop.classList.add("on");
    }
  });
  document.addEventListener("dragleave", (e) => {
    if (!e.relatedTarget) { drop.classList.remove("on"); lit(null); }
  });
  document.addEventListener("drop", (e) => {
    drop.classList.remove("on");
    const types = Array.from(e.dataTransfer.types || []);
    if (types.includes(MINE)) {
      const t = targetOf(e);
      const name = e.dataTransfer.getData(MINE);
      lit(null);
      if (t && name) { e.preventDefault(); send("move", { name, to: t.rel }); }
      return;
    }
    if (types.includes("Files")) {
      e.preventDefault();
      keep(e.dataTransfer);
    }
  });

  // Files dropped from Explorer: the page is given their bytes, not where
  // they are, so the bytes are sent, a part to a message.
  const base64 = (bytes) => {
    let s = "";
    for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
    return btoa(s);
  };
  const keep = async (dt) => {
    if (!webview) { say("Only inside Horadric.", true); return; }
    const files = [];
    let folderCount = 0;
    for (const item of Array.from(dt.items || [])) {
      if (item.kind !== "file") continue;
      const entry = item.webkitGetAsEntry && item.webkitGetAsEntry();
      if (entry && entry.isDirectory) { folderCount++; continue; }
      const f = item.getAsFile();
      if (f) files.push(f);
    }
    if (!files.length) {
      say(folderCount ? "Folders are not copied in, only files." : "Nothing there to copy in.", true);
      return;
    }
    for (const f of files) {
      if (f.size > LARGEST) { say(f.name + " is over 100 MB, too large to copy in here.", true); continue; }
      let bytes;
      try { bytes = new Uint8Array(await f.arrayBuffer()); }
      catch (_) { say(f.name + " could not be read.", true); continue; }
      const parts = Math.max(1, Math.ceil(bytes.length / PART));
      const id = Date.now().toString(36) + Math.random().toString(36).slice(2);
      for (let p = 0; p < parts; p++) {
        const fields = { id, name: f.name, part: p, parts, data: base64(bytes.subarray(p * PART, (p + 1) * PART)) };
        if (p < parts - 1) webview.postMessage(Object.assign({ forge: "home", op: "upload", seq: 0 }, fields));
        else send("upload", fields);
      }
    }
    if (folderCount) say("Folders are not copied in, only files.", true);
  };
})();
"##;

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
        New::Note => ("Untitled note", "md", NOTE),
        New::Deck => ("Untitled deck", "html", DECK),
        New::Table => ("Untitled table", "csv", TABLE),
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

/// The folder tab `tab` shows: its served root, host and folder, checked
/// to be in the root. None for a tab that is no folder's page any more.
fn folder_tab(key: &str, tab: u64) -> Option<(PathBuf, String, PathBuf)> {
    let found = super::TABS.with(|t| {
        t.borrow()
            .iter()
            .find(|f| f.key == key && f.id == tab && f.folder)
            .map(|f| (f.root.clone(), f.host.clone(), f.file.clone()))
    })?;
    let (root, _, dir) = &found;
    (within(root, dir) && !dir.components().any(|c| c == Component::ParentDir)).then_some(found)
}

/// What a folder's page said, after its origin was checked: `new` for a
/// new file, `home` for anything done to the files it lists.
pub fn on_message(key: &str, tab: u64, msg: &Value) {
    match msg.get("forge").and_then(Value::as_str) {
        Some("new") => new_file(key, tab, msg),
        Some("home") => act(key, tab, msg),
        _ => {}
    }
}

/// A file made in the folder the tab shows (never one the page names, as
/// the page could name any) and the tab taken to it.
fn new_file(key: &str, tab: u64, msg: &Value) {
    let what = match msg.get("what").and_then(Value::as_str) {
        Some("md") => New::Note,
        Some("deck") => New::Deck,
        Some("csv") => New::Table,
        _ => return,
    };
    let say = |text: String| super::post(key, tab, &json!({"forge": "home-said", "text": text}));
    let Some((root, host, dir)) = folder_tab(key, tab) else {
        return say("This folder is not the project's.".into());
    };
    match create(&dir, what) {
        Ok(file) => match super::serve::url_for(&host, &root, &file) {
            Some(url) => web::navigate(key, tab, &url),
            None => say(format!("Made {}.", file.display())),
        },
        Err(e) => say(format!("The file could not be made: {e}")),
    }
}

/// Replies made on worker threads, waiting for the UI thread to send them
/// to their pages: the project, the tab, the message.
static DONE: Mutex<Vec<(String, u64, Value)>> = Mutex::new(Vec::new());

/// Runs `work` on a worker thread and sends the page of `tab` what it
/// gives, once the UI thread is woken for it.
fn off(key: &str, tab: u64, seq: u64, work: impl FnOnce() -> Value + Send + 'static) {
    let owned = key.to_string();
    let started = std::thread::Builder::new()
        .name("horadric-forge-home".into())
        .spawn(move || {
            let reply = work();
            if let Ok(mut done) = DONE.lock() {
                done.push((owned, tab, reply));
            }
            super::wake();
        });
    if started.is_err() {
        let text = "Horadric could not start on that, try again.";
        super::post(key, tab, &json!({"forge": "home-did", "seq": seq, "ok": false, "text": text}));
    }
}

/// On the UI thread: sends the pages what their operations came to.
pub(super) fn drain() {
    let done = DONE.lock().map(|mut d| std::mem::take(&mut *d)).unwrap_or_default();
    for (key, tab, reply) in done {
        super::post(&key, tab, &reply);
    }
}

/// What an operation came to: the name to select after it, in the folder
/// shown, and words for the human.
type Outcome = io::Result<(Option<PathBuf>, String)>;

/// The reply to the operation `seq`: whether it worked, what to say, and
/// the folder's listing as it is after it.
fn reply(seq: u64, root: &Path, dir: &Path, flat: bool, done: Outcome) -> Value {
    let written = super::recent_written();
    let listing = read(root, dir, flat, &written).map(|s| listing(&s));
    match done {
        Ok((made, text)) => {
            let name = made
                .filter(|p| p.parent().is_some_and(|p| super::same(p, dir)))
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
            json!({"forge": "home-did", "seq": seq, "ok": true, "text": text, "name": name, "listing": listing})
        }
        Err(e) => json!({"forge": "home-did", "seq": seq, "ok": false, "text": e.to_string(), "listing": listing}),
    }
}

/// Something done to the files the page lists. Every name in it is the
/// page's say and checked in [`files`]; the folder is always the one the
/// tab shows, never one the page names, but for where a move goes.
fn act(key: &str, tab: u64, msg: &Value) {
    let seq = msg.get("seq").and_then(Value::as_u64).unwrap_or(0);
    let text = |k: &str| msg.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let op = text("op");
    let Some((root, host, dir)) = folder_tab(key, tab) else {
        let text = "This folder is not the project's.";
        return super::post(key, tab, &json!({"forge": "home-did", "seq": seq, "ok": false, "text": text}));
    };
    let flat = super::serve::is_flat(&host);
    if op == "upload" {
        return upload(key, tab, seq, (root, dir, flat), msg);
    }
    let (name, to) = (text("name"), text("to"));
    let names: Vec<String> = msg
        .get("names")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    off(key, tab, seq, move || {
        let done: Outcome = match op.as_str() {
            "list" => Ok((None, String::new())),
            "folders" => {
                let folders = if flat { Vec::new() } else { files::folders(&root) };
                return json!({"forge": "home-did", "seq": seq, "ok": true, "folders": folders});
            }
            "rename" => files::rename(&root, &dir, &name, &to).map(|p| (Some(p), String::new())),
            "duplicate" => files::duplicate(&root, &dir, &name).map(|p| (Some(p), String::new())),
            // A flat host serves one folder: there is nowhere to move to.
            "move" if !flat => files::folder_at(&root, &to)
                .and_then(|target| files::move_into(&root, &dir, &name, &target))
                .map(|p| {
                    let into = if to.is_empty() { "the top folder" } else { to.as_str() };
                    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    (None, format!("Moved {name} to {into}."))
                }),
            "delete" => files::recycle(&root, &dir, &names).map(|()| {
                let what = match names.as_slice() {
                    [one] => one.clone(),
                    many => format!("{} things", many.len()),
                };
                (None, format!("{what} went to the Recycle Bin, to be had back from there."))
            }),
            "mkdir" if !flat => files::new_folder(&root, &dir).map(|p| (Some(p), String::new())),
            _ => Err(io::Error::other("That is not something this page can do here.")),
        };
        reply(seq, &root, &dir, flat, done)
    });
}

/// The most one dropped file may hold, as the page also says; past it a
/// copy in Explorer is the better way.
const UPLOAD_LARGEST: usize = 100 << 20;

/// A dropped file arriving in parts, kept until its last.
struct Upload {
    name: String,
    bytes: Vec<u8>,
    next: u64,
    parts: u64,
    started: Instant,
}

thread_local! {
    /// Dropped files part way there, by project, tab and the page's id.
    static UPLOADS: RefCell<HashMap<(String, u64, String), Upload>> = RefCell::new(HashMap::new());
}

/// A part of a file dropped on the page. Gathered here in order, then
/// written on a worker thread under the first name free in the folder the
/// tab shows. The page cannot say where it goes, only what it is called.
fn upload(key: &str, tab: u64, seq: u64, (root, dir, flat): (PathBuf, PathBuf, bool), msg: &Value) {
    let field = |k: &str| msg.get(k).and_then(Value::as_str).unwrap_or_default();
    let number = |k: &str| msg.get(k).and_then(Value::as_u64).unwrap_or(u64::MAX);
    let (id, name, part, parts) = (field("id").to_string(), field("name").to_string(), number("part"), number("parts"));
    let fail = |text: String| {
        super::post(key, tab, &json!({"forge": "home-did", "seq": seq, "ok": false, "text": text}));
    };
    if id.is_empty() || id.len() > 64 || !files::name_ok(&name) {
        return fail(format!("\"{name}\" is not a name a file can have."));
    }
    let most_parts = (UPLOAD_LARGEST as u64).div_ceil(3 << 20) + 1;
    if parts == 0 || parts > most_parts || part >= parts {
        return fail(format!("{name} came in a shape it cannot be kept in."));
    }
    let slot = (key.to_string(), tab, id);
    let Some(bytes) = horadric_core::release::base64_decode(field("data")) else {
        UPLOADS.with(|u| u.borrow_mut().remove(&slot));
        return fail(format!("{name} arrived broken."));
    };
    let finished = UPLOADS.with(|u| {
        let mut u = u.borrow_mut();
        // A drop left half way (the page went away) is let go in time.
        u.retain(|_, up| up.started.elapsed().as_secs() < 600);
        if part == 0 {
            u.insert(
                slot.clone(),
                Upload {
                    name: name.clone(),
                    bytes: Vec::new(),
                    next: 0,
                    parts,
                    started: Instant::now(),
                },
            );
        }
        let Some(up) = u.get_mut(&slot) else {
            return Err(format!("{name} arrived out of order."));
        };
        if up.next != part || up.parts != parts || up.name != name {
            u.remove(&slot);
            return Err(format!("{name} arrived out of order."));
        }
        if up.bytes.len() + bytes.len() > UPLOAD_LARGEST {
            u.remove(&slot);
            return Err(format!("{name} is over 100 MB, too large to copy in here."));
        }
        up.bytes.extend_from_slice(&bytes);
        up.next += 1;
        Ok((up.next == up.parts).then(|| u.remove(&slot)).flatten())
    });
    let up = match finished {
        Ok(Some(up)) => up,
        Ok(None) => return,
        Err(text) => return fail(text),
    };
    off(key, tab, seq, move || {
        let done = files::keep(&root, &dir, &up.name, &up.bytes).map(|p| (Some(p), String::new()));
        reply(seq, &root, &dir, flat, done)
    });
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
        let page = page(&root, &dir, false, &[], theme::ACCENTS[0].0);
        assert_eq!(page.status, 404);
        // A flat host has only its top folder's page.
        assert_eq!(super::page(&root, &root.join("docs"), true, &[], theme::ACCENTS[0].0).status, 404);
        assert_eq!(super::page(&root, &root, true, &[], theme::ACCENTS[0].0).status, 200);
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
        let shown = Shown {
            top: "site".into(),
            segs: vec!["docs".into()],
            flat: false,
            entries,
            cut: false,
            recent: Vec::new(),
            trouble: String::new(),
            changes: Some(vec![
                git::Change {
                    state: git::State::Modified,
                    rel: "docs/my deck #1.html".into(),
                    path: "docs/my deck #1.html".into(),
                    from: None,
                },
                git::Change {
                    state: git::State::Untracked,
                    rel: "docs/a <b>/new.md".into(),
                    path: "docs/a <b>/new.md".into(),
                    from: None,
                },
                git::Change {
                    state: git::State::Deleted,
                    rel: "elsewhere.md".into(),
                    path: "elsewhere.md".into(),
                    from: None,
                },
            ]),
        };
        let page = html(&vars(theme::ACCENTS[0].0), &shown);
        // Changes under the folder only, named from it, linked from the top.
        assert!(page.contains("<h2>Changes</h2><span class=\"count\">2</span>"));
        assert!(page.contains("href=\"/?diff=docs/my%20deck%20%231.html\""));
        assert!(!page.contains("elsewhere.md"));
        // The file is marked with how it changed, the folder as changed inside.
        assert!(page.contains("<span class=\"lamp st-m\"></span><a href=\"my%20deck%20%231.html\""));
        assert!(page.contains("a &lt;b&gt;/</a><span class=\"inside\""));
        assert!(page.contains("data-name=\"a &lt;b&gt;\" data-kind=\"dir\""));
        assert!(page.contains("<a href=\"a%20%3Cb%3E/\" draggable=\"false\">a &lt;b&gt;/</a>"));
        assert!(page.contains("href=\"my%20deck%20%231.html\""));
        assert!(page.contains("<a href=\"/docs/\" data-rel=\"docs\">docs</a>"));
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

    /// A repository with one of each change, or None where git is missing.
    fn repo(name: &str) -> Option<PathBuf> {
        let root = scratch(name);
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&root)
                .output()
                .is_ok_and(|o| o.status.success())
        };
        if !git(&["init", "-q"]) {
            return None;
        }
        std::fs::create_dir_all(root.join("docs")).unwrap();
        let plan: String = (1..=40).map(|i| format!("Line {i} of the plan.\n")).collect();
        std::fs::write(root.join("docs").join("plan.md"), &plan).unwrap();
        std::fs::write(root.join("old name.md"), "kept\n").unwrap();
        std::fs::write(root.join("gone.md"), "soon gone\n").unwrap();
        assert!(git(&["add", "-A"]) && git(&["commit", "-q", "-m", "first"]));
        std::fs::write(
            root.join("docs").join("plan.md"),
            plan.replace("Line 12 of the plan.", "Line twelve, rewritten.")
                .replace("Line 30 of the plan.\n", ""),
        )
        .unwrap();
        std::fs::remove_file(root.join("gone.md")).unwrap();
        assert!(git(&["mv", "old name.md", "new name's.md"]));
        std::fs::write(root.join("docs").join("fresh note.md"), "# Fresh\n\nNew here.\n").unwrap();
        Some(root)
    }

    #[test]
    fn git_names_every_change_under_the_folder() {
        let Some(root) = repo("git") else { return };
        let (_, changes) = git::changes(&root, false).unwrap();
        let got: Vec<(git::State, &str)> = changes.iter().map(|c| (c.state, c.rel.as_str())).collect();
        assert_eq!(
            got,
            [
                (git::State::Untracked, "docs/fresh note.md"),
                (git::State::Modified, "docs/plan.md"),
                (git::State::Deleted, "gone.md"),
                (git::State::Renamed, "new name's.md"),
            ]
        );
        assert_eq!(changes[3].from.as_deref(), Some("old name.md"));
        // From a folder inside, only what is under it, named from there.
        let (_, inner) = git::changes(&root.join("docs"), false).unwrap();
        let rels: Vec<&str> = inner.iter().map(|c| c.rel.as_str()).collect();
        assert_eq!(rels, ["fresh note.md", "plan.md"]);
        let accent = theme::ACCENTS[0].0;
        let diff = page_asked(&root, &root, false, &[], accent, "diff=docs/plan.md");
        let text = String::from_utf8_lossy(match &diff.body {
            super::super::serve::Body::Bytes(b) => b,
            _ => panic!("a page"),
        })
        .into_owned();
        assert!(text.contains("Line twelve, rewritten."));
        assert!(text.contains("lines the same"));
        assert!(text.contains("href=\"/docs/plan.md\">Open file"));
        assert_eq!(page_asked(&root, &root, false, &[], accent, "diff=../x.md").status, 404);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Writes the home of a repository with changes, a diff page and the
    /// page of every change, to look at in the browser pane:
    /// `FORGE_SAMPLES=<dir> cargo test -p horadric-ui --lib -- --ignored write_home`.
    #[test]
    #[ignore]
    fn write_home_samples() {
        let out = PathBuf::from(std::env::var("FORGE_SAMPLES").expect("FORGE_SAMPLES"));
        std::fs::create_dir_all(&out).unwrap();
        let root = repo("samples").expect("git");
        std::fs::write(root.join("deck.html"), "<section class=\"slide\"></section>").unwrap();
        std::fs::write(root.join("table.csv"), "a,b\n").unwrap();
        std::fs::write(root.join("build.rs"), "fn main() {}").unwrap();
        let accent = theme::ACCENTS[0].0;
        let body = |r: Reply| match r.body {
            super::super::serve::Body::Bytes(b) => b.into_owned(),
            _ => Vec::new(),
        };
        for (name, query) in [("home.html", ""), ("diff.html", "diff=docs/plan.md"), ("all.html", "changes")] {
            std::fs::write(out.join(name), body(page_asked(&root, &root, false, &[], accent, query))).unwrap();
        }
        std::fs::write(
            out.join("docs.html"),
            body(page_asked(&root, &root.join("docs"), false, &[], accent, "")),
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }
}
