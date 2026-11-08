//! Git's view of the home's folder: what changed since the last commit,
//! and each change as a page of the lines that changed.
//!
//! Git is asked as a program, the way the checkpoints ask it, never
//! linked. No git, no repository, or a git slower than [`STATUS_WAIT`],
//! and the home shows no changes at all, saying nothing about it: a
//! folder that is not a repository is not a fault.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use horadric_core::forge::{diff_lines, DiffLine};

use super::super::serve::{encode, styled, Reply};

/// The longest the listing waits on `git status`: the page is held until
/// it answers, and a page that hangs is worse than one without changes.
const STATUS_WAIT: Duration = Duration::from_secs(3);

/// The longest one file's last committed text may take to read.
const SHOW_WAIT: Duration = Duration::from_secs(5);

/// The most changes read. Past it the folder holds a build's output or a
/// checkout gone wrong, and a list of thousands helps no one.
const MOST: usize = 2000;

/// Unchanged lines shown round each change; longer runs fold.
const CONTEXT: usize = 3;

/// Text larger than this is not diffed line by line: the table would be
/// too long to read and too slow to lay out.
const LARGEST: usize = 4 << 20;

/// What the page of every change reads at most, all files together.
const ALL_LARGEST: usize = 24 << 20;

/// How a path changed, in the words of `git status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Modified,
    Added,
    Deleted,
    Untracked,
    Renamed,
    /// A merge left it with both sides in it.
    Conflict,
}

impl State {
    /// The letter on its tag, as editors mark changes.
    pub fn letter(self) -> &'static str {
        match self {
            State::Modified => "M",
            State::Added => "A",
            State::Deleted => "D",
            State::Untracked => "U",
            State::Renamed => "R",
            State::Conflict => "C",
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            State::Modified => "modified",
            State::Added => "added",
            State::Deleted => "deleted",
            State::Untracked => "untracked",
            State::Renamed => "renamed",
            State::Conflict => "in conflict",
        }
    }

    /// The class its lamp and tag are coloured by.
    pub fn class(self) -> &'static str {
        match self {
            State::Modified => "st-m",
            State::Added => "st-a",
            State::Deleted => "st-d",
            State::Untracked => "st-u",
            State::Renamed => "st-r",
            State::Conflict => "st-c",
        }
    }
}

/// One line of `git status`: paths from the repository's top, with `/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raw {
    pub state: State,
    pub path: String,
    /// Where a renamed file was.
    pub from: Option<String>,
}

/// The two status letters as one state; None for an ignored path.
fn state_of(x: u8, y: u8) -> Option<State> {
    Some(match (x, y) {
        (b'?', b'?') => State::Untracked,
        (b'!', _) | (_, b'!') => return None,
        (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => State::Conflict,
        (b'R', _) | (_, b'R') => State::Renamed,
        (b'D', _) | (_, b'D') => State::Deleted,
        (b'A' | b'C', _) | (_, b'A' | b'C') => State::Added,
        _ => State::Modified,
    })
}

/// `git status --porcelain=v1 -z`: each entry `XY path` ended by a NUL,
/// a rename or copy followed by the path it came from, also NUL ended.
/// With `-z` git never quotes, so a name is taken as it stands, spaces,
/// quotes and all.
pub fn parse(bytes: &[u8]) -> Vec<Raw> {
    let mut out = Vec::new();
    let mut parts = bytes.split(|&b| b == 0);
    while let Some(entry) = parts.next() {
        if entry.len() < 4 || entry[2] != b' ' {
            continue;
        }
        let (x, y) = (entry[0], entry[1]);
        let path = String::from_utf8_lossy(&entry[3..]).into_owned();
        // The source path is there whenever either letter says R or C,
        // and must be taken even for an entry skipped, or it would be read
        // as an entry of its own.
        let from = (matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C'))
            .then(|| {
                parts
                    .next()
                    .map(|p| String::from_utf8_lossy(p).into_owned())
            })
            .flatten();
        let Some(state) = state_of(x, y) else {
            continue;
        };
        out.push(Raw {
            state,
            path,
            from: from.filter(|_| state == State::Renamed),
        });
    }
    out
}

/// A change under the home's folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub state: State,
    /// The path under the served folder, with `/`.
    pub rel: String,
    /// The path from the repository's top, which git names it by.
    pub path: String,
    /// Where a renamed file was, from the repository's top.
    pub from: Option<String>,
}

/// The repository a folder is in.
#[derive(Debug, Clone)]
pub struct Repo {
    pub top: PathBuf,
}

/// Runs git in `dir` and gives what it printed, None when it is missing,
/// fails, or is still at it after `wait` (then it is stopped).
fn git(dir: &Path, args: &[&str], wait: Duration) -> Option<Vec<u8>> {
    let mut cmd = Command::new("git");
    horadric_hooks::no_window(&mut cmd)
        .arg("--no-optional-locks")
        .args(["-c", "core.quotepath=off"])
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let mut pipe = child.stdout.take()?;
    // Read meanwhile, so a full pipe never holds git up.
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        bytes
    });
    let until = Instant::now() + wait;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let bytes = reader.join().ok()?;
                return status.success().then_some(bytes);
            }
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(8)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// What changed in `root` since the last commit, None when it is in no
/// repository or git could not say in time. A `flat` folder is only its
/// own files, none in its subfolders.
pub fn changes(root: &Path, flat: bool) -> Option<(Repo, Vec<Change>)> {
    let found = git(
        root,
        &["rev-parse", "--show-toplevel", "--show-prefix"],
        STATUS_WAIT,
    )?;
    let found = String::from_utf8_lossy(&found);
    let mut lines = found.lines();
    let top = PathBuf::from(lines.next()?.trim());
    let prefix = lines.next().unwrap_or_default().trim().to_string();
    let status = git(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            ".",
        ],
        STATUS_WAIT,
    )?;
    Some((Repo { top }, under(parse(&status), &prefix, flat)))
}

/// The changes of `raw` under the folder at `prefix` in the repository,
/// named from that folder, in path order.
fn under(raw: Vec<Raw>, prefix: &str, flat: bool) -> Vec<Change> {
    let mut out: Vec<Change> = raw
        .into_iter()
        .filter_map(|r| {
            let rel = r.path.strip_prefix(prefix)?.to_string();
            if rel.is_empty() || rel.ends_with('/') || (flat && rel.contains('/')) {
                return None;
            }
            Some(Change {
                state: r.state,
                rel,
                path: r.path,
                from: r.from,
            })
        })
        .take(MOST)
        .collect();
    out.sort_by_key(|a| a.rel.to_lowercase());
    out
}

/// The address of the page of `rel`'s change, on the host's own top
/// folder page so the tab stays the home's.
pub fn diff_href(rel: &str) -> String {
    let path: Vec<String> = rel.split('/').map(encode).collect();
    format!("/?diff={}", path.join("/"))
}

/// The address of the page of every change.
pub const ALL_HREF: &str = "/?changes";

/// What a page of changes was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    One(String),
    All,
}

/// What the query of a folder's address asks, None for its listing.
pub fn asked(query: &str) -> Option<Asked> {
    for q in query.split('&') {
        if q == "changes" {
            return Some(Asked::All);
        }
        if let Some(v) = q.strip_prefix("diff=") {
            return Some(Asked::One(unescape(v)));
        }
    }
    None
}

/// A query value as it was before the address encoded it.
fn unescape(s: &str) -> String {
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
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The text of `path` (from the repository's top) at the last commit,
/// None when it was not in it.
fn committed(repo: &Repo, path: &str) -> Option<Vec<u8>> {
    git(
        &repo.top,
        &["cat-file", "blob", &format!("HEAD:{path}")],
        SHOW_WAIT,
    )
}

/// The two sides of a change: as committed and as it is now.
fn sides(repo: &Repo, root: &Path, c: &Change) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let before = match c.state {
        State::Added | State::Untracked => None,
        _ => committed(repo, c.from.as_deref().unwrap_or(&c.path)),
    };
    let now = match c.state {
        State::Deleted => None,
        _ => {
            let file = root.join(c.rel.replace('/', "\\"));
            // Never through a link: it could lead anywhere on the disk.
            let plain = std::fs::symlink_metadata(&file).is_ok_and(|m| m.is_file());
            plain.then(|| std::fs::read(&file).ok()).flatten()
        }
    };
    (before, now)
}

fn esc(s: &str) -> String {
    super::super::runtime::attr(s)
}

/// The page of the change asked for in `root`, in Horadric's look. Runs
/// git and reads the disk, so it is for a worker thread.
pub fn page(root: &Path, flat: bool, asked: &Asked, look: &str) -> Reply {
    let found = changes(root, flat);
    let (title, body) = match (&found, asked) {
        (None, _) => (
            "Changes".to_string(),
            quiet("This folder is in no git repository, or git did not answer."),
        ),
        (Some((repo, all)), Asked::One(rel)) => match all.iter().find(|c| c.rel == *rel) {
            Some(c) => (rel.clone(), one(repo, root, c, &mut 0)),
            None => (rel.clone(), quiet("No changes here since the last commit.")),
        },
        (Some((repo, all)), Asked::All) => ("All changes".to_string(), every(repo, root, all)),
    };
    let one_file = matches!(asked, Asked::One(_));
    let mut head = String::new();
    head.push_str("<nav class=\"top\"><a class=\"key\" href=\"/\">Home</a>");
    if one_file && found.as_ref().is_some_and(|(_, all)| all.len() > 1) {
        head.push_str(&format!(
            "<a class=\"key\" href=\"{ALL_HREF}\">All changes</a>"
        ));
    }
    head.push_str("</nav>");
    let page = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{t}</title><style>{}{CSS}</style></head><body><div class=\"changes\">{head}\
         {body}</div><script>{SCRIPT}</script></body></html>",
        horadric_core::forge::LOOK,
        t = esc(&title),
    );
    Reply::html(styled(page, look))
}

fn quiet(text: &str) -> String {
    format!(
        "<div class=\"hd-screen\"><p class=\"same\">{}</p></div>",
        esc(text)
    )
}

/// Every change, one after another, until [`ALL_LARGEST`] is read.
fn every(repo: &Repo, root: &Path, all: &[Change]) -> String {
    if all.is_empty() {
        return quiet("No changes here since the last commit.");
    }
    let mut out = format!(
        "<header class=\"sum\"><h1>All changes</h1><span class=\"hd-legend\">{} {}</span></header>",
        all.len(),
        if all.len() == 1 { "file" } else { "files" }
    );
    let mut read = 0usize;
    for c in all {
        if read > ALL_LARGEST {
            out.push_str(&format!(
                "<section class=\"file\">{}<div class=\"hd-screen\"><p class=\"same\">Not shown here, \
                 the page is long enough. <a href=\"{}\">Show this one</a>.</p></div></section>",
                header(c, false),
                esc(&diff_href(&c.rel))
            ));
            continue;
        }
        out.push_str(&one(repo, root, c, &mut read));
    }
    out
}

/// The name of a change over its diff: its lamp and tag, its path linked
/// to its own page on the page of every change, and a key to open it.
fn header(c: &Change, linked: bool) -> String {
    let name = if linked {
        format!(
            "<a href=\"{}\">{}</a>",
            esc(&diff_href(&c.rel)),
            esc(&c.rel)
        )
    } else {
        esc(&c.rel)
    };
    let from = c
        .from
        .as_deref()
        .map(|f| format!("<span class=\"from\">from {}</span>", esc(f)))
        .unwrap_or_default();
    let open = if c.state != State::Deleted
        && super::label(c.rel.rsplit('/').next().unwrap_or_default()).is_some()
    {
        let path: Vec<String> = c.rel.split('/').map(encode).collect();
        format!(
            "<a class=\"key small\" href=\"/{}\">Open file</a>",
            esc(&path.join("/"))
        )
    } else {
        String::new()
    };
    format!(
        "<header><span class=\"lamp {cl}\"></span><span class=\"tag {cl}\" title=\"{w}\">{l}</span>\
         <h1>{name}</h1>{from}<span class=\"grow\"></span>{open}</header>",
        cl = c.state.class(),
        w = c.state.word(),
        l = c.state.letter(),
    )
}

/// One change: its header and its lines. `read` counts what was read,
/// for the page of every change to stop in time.
fn one(repo: &Repo, root: &Path, c: &Change, read: &mut usize) -> String {
    let (before, now) = sides(repo, root, c);
    let size = |s: &Option<Vec<u8>>| s.as_ref().map_or(0, Vec::len);
    *read += size(&before) + size(&now);
    let body = if size(&before) > LARGEST || size(&now) > LARGEST {
        format!(
            "<p class=\"same\">Too large to compare line by line, over {} MB.</p>",
            LARGEST >> 20
        )
    } else if binary(&before) || binary(&now) {
        "<p class=\"same\">Not text, so not shown line by line.</p>".to_string()
    } else {
        let text = |s: Option<Vec<u8>>| {
            s.map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default()
        };
        let lines = diff_lines(&text(before), &text(now));
        if c.state == State::Renamed && lines.iter().all(|l| matches!(l, DiffLine::Same(_))) {
            "<p class=\"same\">Only its name changed.</p>".to_string()
        } else {
            table(&lines)
        }
    };
    format!(
        "<section class=\"file\">{}<div class=\"hd-screen diff\">{body}</div></section>",
        header(c, true)
    )
}

/// Whether bytes are not text: git's own test, a NUL near the start.
fn binary(b: &Option<Vec<u8>>) -> bool {
    b.as_ref()
        .is_some_and(|b| b[..b.len().min(8000)].contains(&0))
}

/// The lines as a table numbered on both sides. Unchanged runs longer
/// than the context round the changes fold behind a count, which opens
/// them where they are.
pub fn table(lines: &[DiffLine]) -> String {
    if !lines.iter().any(|l| !matches!(l, DiffLine::Same(_))) {
        return "<p class=\"same\">The text is the same, only how it is stored changed.</p>"
            .to_string();
    }
    let near = |i: usize| {
        let lo = i.saturating_sub(CONTEXT);
        let hi = (i + CONTEXT + 1).min(lines.len());
        lines[lo..hi]
            .iter()
            .any(|l| !matches!(l, DiffLine::Same(_)))
    };
    let mut out = String::from("<table>");
    let mut folded = String::new();
    let mut count = 0usize;
    let flush = |out: &mut String, folded: &mut String, count: &mut usize| {
        if *count > 0 {
            out.push_str(&format!(
                "<tbody class=\"fold\"><tr><td colspan=\"4\"><button type=\"button\">{count} {} the same</button></td></tr></tbody>\
                 <tbody hidden>{folded}</tbody>",
                if *count == 1 { "line" } else { "lines" }
            ));
            folded.clear();
            *count = 0;
        }
    };
    out.push_str("<tbody>");
    let (mut a, mut b) = (0usize, 0usize);
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
        let row = format!(
            "<tr class=\"{class}\"><td class=\"n\">{na}</td><td class=\"n\">{nb}</td>\
             <td class=\"m\">{mark}</td><td>{}</td></tr>",
            esc(text)
        );
        if class.is_empty() && !near(i) {
            folded.push_str(&row);
            count += 1;
            continue;
        }
        if count > 0 {
            out.push_str("</tbody>");
            flush(&mut out, &mut folded, &mut count);
            out.push_str("<tbody>");
        }
        out.push_str(&row);
    }
    out.push_str("</tbody>");
    flush(&mut out, &mut folded, &mut count);
    out.push_str("</table>");
    out
}

/// A fold opens where it is.
const SCRIPT: &str = r#"
document.addEventListener("click", (e) => {
  const b = e.target.closest("tbody.fold button");
  if (!b) return;
  const fold = b.closest("tbody.fold");
  const rows = fold.nextElementSibling;
  if (rows) rows.hidden = false;
  fold.remove();
});
"#;

/// Drawn as the Folio draws a diff and the history's compare page does:
/// removed rows tinted in the error colour, added in the done colour,
/// numbers in the legends' ink, unchanged runs folded behind an engraved
/// count. Each file's name is on the plate over its screen, with a lamp
/// lit in the colour of how it changed.
const CSS: &str = r#"
body { padding: 30px 26px 48px; }
.changes { max-width: 1120px; margin: 0 auto; }
a { color: inherit; text-decoration: none; }
nav.top { display: flex; gap: 10px; margin: 0 2px 22px; }
.key { display: inline-block; font: 500 12px/1 var(--hd-ui); color: var(--hd-text); cursor: pointer;
  border: 0; border-radius: 6px; padding: 7px 12px 8px; margin: 0 0 3px;
  background: linear-gradient(var(--hd-key-top), var(--hd-key-low));
  box-shadow: inset 0 1px 0 var(--hd-bevel-light), inset 0 -1px 0 var(--hd-bevel-shade),
    0 3px 0 var(--hd-key-side), 0 4px 8px var(--hd-cast); }
.key:hover { background: linear-gradient(var(--hd-hover), var(--hd-hover)),
  linear-gradient(var(--hd-key-top), var(--hd-key-low)); }
.key:active { transform: translateY(2px); box-shadow: inset 0 1px 0 var(--hd-bevel-light),
  0 1px 0 var(--hd-key-side), 0 1px 3px var(--hd-cast); }
.key.small { font-size: 11px; padding: 5px 10px 6px; }
header.sum { display: flex; align-items: baseline; gap: 16px; margin: 0 4px 18px; }
section.file { margin: 0 0 26px; }
section.file header { display: flex; align-items: center; gap: 10px; margin: 0 4px 10px; min-height: 30px; }
h1 { margin: 0; font: 600 15px/1.3 var(--hd-mono); color: var(--hd-text); overflow-wrap: anywhere; }
h1 a:hover { color: var(--hd-accent); }
.from { color: var(--hd-dim); font: 12px/1.3 var(--hd-mono); overflow-wrap: anywhere; }
.grow { flex: 1; }
.lamp { flex: none; width: 7px; height: 7px; border-radius: 50%; background: var(--hd-lamp-off); }
.tag { flex: none; font: 700 10px/1 var(--hd-ui); padding: 3px 5px 2px; border-radius: 4px;
  border: 1px solid currentColor; }
.st-m { color: var(--hd-waiting); } .lamp.st-m { background: var(--hd-waiting); }
.st-a { color: var(--hd-done); } .lamp.st-a { background: var(--hd-done); }
.st-u { color: var(--hd-done); } .lamp.st-u { background: var(--hd-done); }
.st-d { color: var(--hd-error); } .lamp.st-d { background: var(--hd-error); }
.st-r { color: var(--hd-accent); } .lamp.st-r { background: var(--hd-accent); }
.st-c { color: var(--hd-error); } .lamp.st-c { background: var(--hd-error); }
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
tbody.fold td { padding: 0; border-block: 1px solid color-mix(in srgb, var(--hd-read) 6%, transparent); }
tbody.fold button { display: block; width: 100%; padding: 6px; border: 0; background: none; cursor: pointer;
  font: 600 10.5px/1.2 var(--hd-ui); letter-spacing: 0.12em; text-transform: uppercase;
  color: var(--hd-legend); }
tbody.fold button:hover { color: var(--hd-text); background: var(--hd-hover); }
.same { margin: 0; padding: 30px 20px; text-align: center; color: var(--hd-dim); font-size: 14px; }
.same a { color: var(--hd-accent); }
@media print {
  body { padding: 0; }
  nav.top, .key { display: none; }
  .hd-screen { background: none; box-shadow: none; }
  td { color: #1d1e21; }
  tr.del td { background: #fbe3e4; }
  tr.add td { background: #e0f5e9; }
  h1 { color: #000; }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_reads_every_state() {
        let raw = b" M src/main.rs\0A  new.md\0 D gone.md\0?? notes/todo list.md\0UU both.md\0!! target/\0";
        let got = parse(raw);
        let states: Vec<(State, &str)> = got.iter().map(|r| (r.state, r.path.as_str())).collect();
        assert_eq!(
            states,
            [
                (State::Modified, "src/main.rs"),
                (State::Added, "new.md"),
                (State::Deleted, "gone.md"),
                (State::Untracked, "notes/todo list.md"),
                (State::Conflict, "both.md"),
            ]
        );
    }

    #[test]
    fn a_rename_takes_its_source_from_the_next_field() {
        // With -z a rename is `R  to\0from\0`: the new name first.
        let raw = b"R  docs/new name.md\0docs/old name.md\0 M after.md\0RM a.md\0b.md\0";
        let got = parse(raw);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].state, State::Renamed);
        assert_eq!(got[0].path, "docs/new name.md");
        assert_eq!(got[0].from.as_deref(), Some("docs/old name.md"));
        assert_eq!(
            got[1].path, "after.md",
            "the source is never read as an entry"
        );
        assert_eq!(got[2].from.as_deref(), Some("b.md"));
        // A copy is a new file; its source is still taken.
        let copy = parse(b"C  c2.md\0c.md\0?? x.md\0");
        assert_eq!(copy[0].state, State::Added);
        assert_eq!(copy[0].from, None);
        assert_eq!(copy[1].path, "x.md");
    }

    #[test]
    fn names_with_quotes_and_odd_letters_stand_as_they_are() {
        let raw = "?? say \"hi\".md\0 M caf\u{e9} \u{1f600}.md\0?? a\\b\0".as_bytes();
        let got = parse(raw);
        assert_eq!(got[0].path, "say \"hi\".md");
        assert_eq!(got[1].path, "caf\u{e9} \u{1f600}.md");
        assert_eq!(got[2].path, "a\\b");
        assert!(parse(b"").is_empty());
        assert!(parse(b"garbage").is_empty());
    }

    #[test]
    fn changes_are_named_from_the_served_folder() {
        let raw = parse(b" M site/a.md\0 M other/b.md\0?? site/sub/c.md\0R  site/n.md\0old/n.md\0");
        let deep = under(raw.clone(), "site/", false);
        let rels: Vec<&str> = deep.iter().map(|c| c.rel.as_str()).collect();
        assert_eq!(rels, ["a.md", "n.md", "sub/c.md"]);
        assert_eq!(deep[1].from.as_deref(), Some("old/n.md"));
        let flat = under(raw, "site/", true);
        assert!(flat.iter().all(|c| !c.rel.contains('/')));
    }

    #[test]
    fn the_query_names_a_change() {
        assert_eq!(
            asked("diff=docs/my%20plan.md"),
            Some(Asked::One("docs/my plan.md".into()))
        );
        assert_eq!(asked("changes"), Some(Asked::All));
        assert_eq!(asked(""), None);
        assert_eq!(diff_href("docs/my plan.md"), "/?diff=docs/my%20plan.md");
        assert_eq!(
            asked(&diff_href("a b/c#d.md")[2..]),
            Some(Asked::One("a b/c#d.md".into()))
        );
    }

    #[test]
    fn the_table_folds_long_unchanged_runs() {
        let a: String = (1..=20).map(|i| format!("{i}\n")).collect();
        let b = a.replace("10\n", "ten\n");
        let t = table(&diff_lines(&a, &b));
        assert!(t.contains("6 lines the same"));
        assert!(t.contains("7 lines the same"));
        assert!(t.contains("<td class=\"n\">10</td><td class=\"n\"></td>"));
        assert!(t.contains("<td class=\"n\"></td><td class=\"n\">10</td>"));
        for bad in ['\u{2014}', '\u{2013}'] {
            assert!(!t.contains(bad));
        }
    }
}
