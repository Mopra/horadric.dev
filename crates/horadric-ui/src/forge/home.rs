//! The Forge's home: the page of a folder of the project, to find a file
//! to work on or start a new one. HTML in Horadric's own look, its theme
//! and the project's colour, made by `serve` on a worker thread for any
//! page asked of a folder.
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
/// `accent` is the project's colour, read on the UI thread, where the
/// colours projects were given live.
pub fn page(root: &Path, dir: &Path, flat: bool, written: &[PathBuf], accent: Color) -> Reply {
    let segs = segments(root, dir).filter(|_| within(root, dir) && dir.is_dir());
    let Some(segs) = segs.filter(|s| !flat || s.is_empty()) else {
        return Reply::html("<!doctype html><title>Not found</title><p>No such folder here.".into())
            .status(404);
    };
    let top = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    let look = vars(accent);
    let (entries, cut) = match list(dir, flat) {
        Ok(listed) => listed,
        Err(e) => {
            let text = format!("This folder cannot be read: {e}");
            return Reply::html(html(&look, &top, &segs, &[], false, &[], &text));
        }
    };
    let recent = if segs.is_empty() {
        recent(root, written)
    } else {
        Vec::new()
    };
    Reply::html(html(&look, &top, &segs, &entries, cut, &recent, ""))
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

/// The page itself, from what was read: the folder's place under `top`,
/// its names, and on the top folder its recent files. `look` is [`vars`].
/// `trouble` is said in place of the listing when the folder could not be
/// read.
fn html(
    look: &str,
    top: &str,
    segs: &[String],
    entries: &[Entry],
    cut: bool,
    recent: &[Recent],
    trouble: &str,
) -> String {
    let title = segs.last().map_or(top, String::as_str);
    let mut b = String::with_capacity(8192 + entries.len() * 160);
    b.push_str("<!doctype html>\n<html><head><meta charset=\"utf-8\">");
    b.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    b.push_str(&format!(
        "<title>{}</title>\n<style>:root {{ {look} }}\n{STYLE}</style></head>\n",
        esc(title)
    ));
    b.push_str("<body>");
    b.push_str(&rune_symbols());
    b.push_str("<main class=\"forge-home\">\n<nav class=\"crumbs\">");
    b.push_str(&rune(rune_of(top)));
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

    b.push_str("<section class=\"new\"><span class=\"legend\">New</span>");
    for (what, name) in [("md", "Markdown note"), ("deck", "HTML deck"), ("csv", "CSV table")] {
        b.push_str(&format!("<button type=\"button\" data-new=\"{what}\">{name}</button>"));
    }
    b.push_str("<span class=\"work\" id=\"work\">");
    for i in [3, 7, 10] {
        b.push_str(&rune(i));
    }
    b.push_str("</span><span class=\"said\" id=\"said\"></span></section>\n");

    if !recent.is_empty() {
        let now = SystemTime::now();
        b.push_str("<section><div class=\"bar\"><h2>Recent</h2><span class=\"groove\"></span></div>");
        b.push_str("<div class=\"screen\"><ul class=\"list\">\n");
        for r in recent {
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

    let quiet = entries.iter().filter(|e| e.quiet).count();
    b.push_str("<section><div class=\"bar\"><h2>Files</h2><span class=\"groove\"></span>");
    if quiet > 0 {
        b.push_str(&format!(
            "<button type=\"button\" class=\"small\" id=\"all\" data-show=\"Show {quiet} more\">Show {quiet} more</button>"
        ));
    }
    b.push_str("</div>\n<div class=\"screen\">\n");
    if !trouble.is_empty() {
        b.push_str(&format!(
            "<p class=\"note empty trouble\">{}<span>{}</span></p>\n",
            rune(2),
            esc(trouble)
        ));
    } else if entries.iter().all(|e| e.quiet) {
        b.push_str(&format!(
            "<p class=\"note empty none\">{}<span>Nothing here the Forge shows yet.</span></p>\n",
            rune(rune_of(title))
        ));
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
                "<li class=\"{class}\">{}<a href=\"{}/\">{}/</a></li>\n",
                rune(rune_of(&e.name)),
                esc(&link),
                esc(&e.name)
            ));
        } else if let Some(kind) = e.label {
            b.push_str(&format!(
                "<li class=\"{class}\"><span class=\"lamp\"></span><a href=\"{}\">{}</a><span class=\"kind\">{kind}</span></li>\n",
                esc(&link),
                esc(&e.name)
            ));
        } else {
            b.push_str(&format!(
                "<li class=\"{class} other\"><span class=\"lamp\"></span><span class=\"name\">{}</span></li>\n",
                esc(&e.name)
            ));
        }
    }
    b.push_str("</ul>\n");
    if cut {
        b.push_str(&format!("<p class=\"note\">Only the first {MOST} names are listed.</p>\n"));
    }
    b.push_str("</div></section></main>\n<script>");
    b.push_str(SCRIPT);
    b.push_str("</script>\n</body></html>\n");
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
button { font: 500 13px/1 var(--hd-ui); color: var(--hd-text); cursor: pointer;
  border: 0; border-radius: 6px; padding: 8px 14px 9px; margin: 0 0 3px;
  background: linear-gradient(var(--hd-key-top), var(--hd-key-low));
  box-shadow: inset 0 1px 0 var(--hd-bevel-light), inset 0 -1px 0 var(--hd-bevel-shade),
    0 3px 0 var(--hd-key-side), 0 4px 8px var(--hd-cast);
  transition: transform 70ms ease, box-shadow 70ms ease; }
button:hover { background: linear-gradient(var(--hd-hover), var(--hd-hover)),
  linear-gradient(var(--hd-key-top), var(--hd-key-low)); }
button:active:not(:disabled), button:disabled { transform: translateY(2px);
  box-shadow: inset 0 1px 0 var(--hd-bevel-light), 0 1px 0 var(--hd-key-side),
    0 1px 3px var(--hd-cast); }
button:disabled { color: var(--hd-dim); cursor: default; }
button.small { font-size: 11px; padding: 5px 10px 6px; }

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
ul.list li > a, ul.list li > .name { flex: 1; min-width: 0; overflow-wrap: anywhere; }
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

@media (prefers-reduced-motion: reduce) {
  .work svg.rune { animation: none; opacity: 1; }
  button { transition: none; }
}
"#;

/// The page's own script: the New buttons ask Horadric for a file, and
/// the toggle shows what is tucked away, remembered per project.
const SCRIPT: &str = r#"
(() => {
  "use strict";
  const webview = window.chrome && window.chrome.webview;
  const said = document.getElementById("said");
  const work = document.getElementById("work");
  const news = Array.from(document.querySelectorAll("button[data-new]"));
  for (const b of news) {
    b.addEventListener("click", () => {
      if (!webview) { said.textContent = "Only inside Horadric."; return; }
      for (const n of news) n.disabled = true;
      said.textContent = "";
      work.classList.add("on");
      webview.postMessage({ forge: "new", what: b.dataset.new });
    });
  }
  if (webview) {
    webview.addEventListener("message", (e) => {
      let m = e.data;
      if (typeof m === "string") { try { m = JSON.parse(m); } catch (_) { return; } }
      if (!m || m.forge !== "home-said") return;
      said.textContent = String(m.text || "");
      work.classList.remove("on");
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
        let page = html(&vars(theme::ACCENTS[0].0), "site", &["docs".into()], &entries, false, &[], "");
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
