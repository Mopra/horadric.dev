//! The files tile on the Mac: a project's files with their git changes,
//! as on Windows. The tree, its rows and which folders are open are the
//! shared `files`; git is read through `horadric_core::gitfiles`, as the
//! Windows watcher and a brother read it.
//!
//! What differs is how a change is heard. Windows sleeps a thread in
//! `ReadDirectoryChangesW`; the Mac has FSEvents, which watches a folder
//! and everything under it from the kernel's own log and calls back on the
//! main queue. Nothing polls, so a quiet project costs nothing, and a scan
//! runs on a thread of its own so git never holds the app.

use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_void, CStr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use horadric_core::gitfiles;
use objc2_foundation::{NSArray, NSString};

use crate::files::{self, Change, Expansion, Tree};
use crate::layout::{self, Metrics};

use super::app::{self, Input};
use super::queue;

/// How long the files must be quiet before git runs again.
const SETTLE: Duration = Duration::from_millis(300);
/// The least time between two scans, however busy the folder is.
const MIN_GAP: Duration = Duration::from_secs(1);
/// The most rows a tile shows before it scrolls. Windows lets the column
/// share its height out among the files tiles; the Mac stacks its
/// clusters plainly, so each tile gets a fixed most instead.
const MOST_ROWS: usize = 12;

/// What a scan thread or the watcher has to say, for the main thread.
pub enum Note {
    Scanned(String, Option<Scan>),
    /// Paths changed under the project, relative to it. `all` when FSEvents
    /// lost track and anything may have.
    Changed {
        key: String,
        paths: Vec<String>,
        all: bool,
    },
}

/// What one run of git found.
pub struct Scan {
    tree: Tree,
    ignored: HashSet<String>,
    prefix: String,
    /// The project folder with links resolved, as FSEvents names paths.
    root: PathBuf,
    git_dir: Option<PathBuf>,
}

/// What a click on the tile asks of the app.
pub enum Click {
    Nothing,
    /// The tile changed: draw it again.
    Redraw,
    /// Show this file read only on the stage.
    Open(PathBuf),
}

/// Every project's files tile.
#[derive(Default)]
pub struct Files {
    projects: HashMap<String, Project>,
    /// The projects whose tile is folded to its header, saved.
    pub folded: HashSet<String>,
}

struct Project {
    dir: PathBuf,
    tree: Option<Tree>,
    ignored: HashSet<String>,
    prefix: Option<String>,
    open: Expansion,
    scroll: usize,
    /// How many rows the tile shows, as last laid out.
    shown: usize,
    /// A wheel's travel not yet a whole row.
    wheel: f64,
    watch: Option<Watch>,
    scanning: bool,
    /// Something changed while git ran: scan again after.
    dirty: bool,
    booked: bool,
    last: Option<Instant>,
}

/// One visible row, owned, for a scene to keep.
#[derive(Clone)]
pub struct Line {
    pub label: String,
    pub depth: usize,
    pub dir: bool,
    pub open: bool,
    pub change: Option<Change>,
}

/// What the tile draws this frame.
#[derive(Clone)]
pub struct FilesView {
    pub lines: Vec<Line>,
    /// How many rows there are in all, and how many are above the top.
    pub total: usize,
    pub scroll: usize,
    pub changed: usize,
    pub folded: bool,
}

impl Files {
    /// No tiles yet, the projects in `folded` folded as saved.
    pub fn folded(folded: HashSet<String>) -> Files {
        Files {
            projects: HashMap::new(),
            folded,
        }
    }

    /// Keeps a tile for each project in `projects`, by key and folder, and
    /// lets the rest go. A new one is scanned at once.
    pub fn keep(&mut self, projects: &[(String, PathBuf)]) {
        self.projects
            .retain(|k, _| projects.iter().any(|(key, _)| key == k));
        for (key, dir) in projects {
            if self.projects.contains_key(key) {
                continue;
            }
            self.projects.insert(
                key.clone(),
                Project {
                    dir: dir.clone(),
                    tree: None,
                    ignored: HashSet::new(),
                    prefix: None,
                    open: Expansion::default(),
                    scroll: 0,
                    shown: 0,
                    wheel: 0.0,
                    watch: None,
                    scanning: false,
                    dirty: false,
                    booked: false,
                    last: None,
                },
            );
            self.scan(key);
        }
    }

    /// How tall the tile is below its header, as `layout::cluster` takes
    /// it: none before git answered or outside git, zero folded.
    pub fn body(&self, key: &str, m: &Metrics) -> Option<f32> {
        let p = self.projects.get(key)?;
        let tree = p.tree.as_ref()?;
        if self.folded.contains(key) {
            return Some(0.0);
        }
        let rows = tree.rows(&p.open).len().clamp(1, MOST_ROWS);
        Some((rows as f32 * m.file_row_h + m.file_foot).max(layout::min_files_body(m)))
    }

    /// What the tile shows with `shown` rows of room.
    pub fn view(&mut self, key: &str, shown: usize) -> Option<FilesView> {
        let folded = self.folded.contains(key);
        let p = self.projects.get_mut(key)?;
        let tree = p.tree.as_ref()?;
        let rows = tree.rows(&p.open);
        p.shown = shown;
        p.scroll = p.scroll.min(rows.len().saturating_sub(shown));
        let lines = rows
            .iter()
            .skip(p.scroll)
            .take(shown)
            .map(|r| {
                let node = tree.node(r.node);
                Line {
                    label: r.label.clone(),
                    depth: r.depth,
                    dir: node.dir,
                    open: r.open,
                    change: node.change,
                }
            })
            .collect();
        Some(FilesView {
            lines,
            total: rows.len(),
            scroll: p.scroll,
            changed: tree.changed,
            folded,
        })
    }

    /// The header folds the tile; a folder row opens or closes it; a file
    /// row shows the file.
    pub fn click(&mut self, key: &str, hit: layout::Hit) -> Click {
        match hit {
            layout::Hit::FilesHeader => {
                if !self.folded.remove(key) {
                    self.folded.insert(key.to_string());
                }
                Click::Redraw
            }
            layout::Hit::File(i) => {
                let Some(p) = self.projects.get_mut(key) else {
                    return Click::Nothing;
                };
                let Some(tree) = p.tree.as_ref() else {
                    return Click::Nothing;
                };
                let rows = tree.rows(&p.open);
                let Some(row) = rows.get(p.scroll + i) else {
                    return Click::Nothing;
                };
                let node = tree.node(row.node);
                if node.dir {
                    p.open.toggle(node);
                    Click::Redraw
                } else {
                    Click::Open(p.dir.join(&node.path))
                }
            }
            _ => Click::Nothing,
        }
    }

    /// Scrolls the tile by a wheel's `dy` points, or lines when not
    /// `precise`. True when it moved.
    pub fn wheel(&mut self, key: &str, dy: f64, precise: bool) -> bool {
        let Some(p) = self.projects.get_mut(key) else {
            return false;
        };
        let Some(tree) = p.tree.as_ref() else {
            return false;
        };
        let step = if precise {
            Metrics::default().file_row_h as f64
        } else {
            1.0
        };
        p.wheel -= dy / step;
        let rows = p.wheel.trunc();
        if rows == 0.0 {
            return false;
        }
        p.wheel -= rows;
        let most = tree.rows(&p.open).len().saturating_sub(p.shown);
        let to = (p.scroll as i64 + rows as i64).clamp(0, most as i64) as usize;
        std::mem::replace(&mut p.scroll, to) != to
    }

    /// Takes in what a scan or the watcher said. True when the tiles need
    /// laying out again.
    pub fn heard(&mut self, note: Note) -> bool {
        match note {
            Note::Scanned(key, scan) => {
                let Some(p) = self.projects.get_mut(&key) else {
                    return false;
                };
                p.scanning = false;
                p.last = Some(Instant::now());
                let Some(scan) = scan else {
                    // Not in git, or git failed: no tile, and nothing to
                    // watch for.
                    let had = p.tree.take().is_some();
                    p.watch = None;
                    return had;
                };
                if p.watch.is_none() {
                    p.watch = Watch::start(&key, &scan.root, scan.git_dir.as_deref());
                }
                p.tree = Some(scan.tree);
                p.ignored = scan.ignored;
                p.prefix = Some(scan.prefix);
                if std::mem::take(&mut p.dirty) {
                    self.book(&key);
                }
                true
            }
            Note::Changed { key, paths, all } => {
                let Some(p) = self.projects.get_mut(&key) else {
                    return false;
                };
                if all || paths.iter().any(|path| files::relevant(path, &p.ignored)) {
                    if p.scanning {
                        p.dirty = true;
                    } else {
                        self.book(&key);
                    }
                }
                false
            }
        }
    }

    /// A scan once the burst has settled, and no sooner than [`MIN_GAP`]
    /// after the last.
    fn book(&mut self, key: &str) {
        let Some(p) = self.projects.get_mut(key) else {
            return;
        };
        if p.booked {
            return;
        }
        p.booked = true;
        let since = p.last.map_or(MIN_GAP, |t| t.elapsed());
        let wait = SETTLE.max(MIN_GAP.saturating_sub(since));
        let key = key.to_string();
        queue::after_main(wait, move || {
            app::input(Input::FilesDue(key));
        });
    }

    /// The booked scan is due.
    pub fn due(&mut self, key: &str) {
        if let Some(p) = self.projects.get_mut(key) {
            p.booked = false;
            self.scan(key);
        }
    }

    fn scan(&mut self, key: &str) {
        let Some(p) = self.projects.get_mut(key) else {
            return;
        };
        if p.scanning {
            p.dirty = true;
            return;
        }
        p.scanning = true;
        let (key, dir, prefix) = (key.to_string(), p.dir.clone(), p.prefix.clone());
        let spawned = std::thread::Builder::new()
            .name("horadric-files".into())
            .spawn(move || {
                let scan = scan(&dir, prefix);
                queue::post(move || app::input(Input::Files(Note::Scanned(key, scan))));
            });
        if spawned.is_err() {
            p.scanning = false;
        }
    }
}

/// Reads the project's tree from git. The prefix and the git folder are
/// asked for once, on the first scan.
fn scan(dir: &Path, prefix: Option<String>) -> Option<Scan> {
    let prefix = match prefix {
        Some(p) => p,
        None => gitfiles::prefix(dir)?,
    };
    let listed = gitfiles::listed(dir)?;
    let status = gitfiles::status(dir, &prefix)?;
    Some(Scan {
        tree: Tree::build(&listed, &status),
        ignored: gitfiles::ignored(dir).into_iter().collect(),
        prefix,
        root: std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()),
        git_dir: gitfiles::outside_git_dir(dir),
    })
}

// FSEvents, from CoreServices, which every Mac has. Declared here rather
// than taken from a crate: five calls do not justify a dependency.

type StreamRef = *mut c_void;

#[repr(C)]
struct StreamContext {
    version: isize,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

type StreamCallback = extern "C" fn(
    stream: StreamRef,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
);

#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    fn FSEventStreamCreate(
        allocator: *const c_void,
        callback: StreamCallback,
        context: *const StreamContext,
        paths: *const c_void,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> StreamRef;
    fn FSEventStreamSetDispatchQueue(stream: StreamRef, queue: *const c_void);
    fn FSEventStreamStart(stream: StreamRef) -> u8;
    fn FSEventStreamStop(stream: StreamRef);
    fn FSEventStreamInvalidate(stream: StreamRef);
    fn FSEventStreamRelease(stream: StreamRef);
}

/// `kFSEventStreamEventIdSinceNow`.
const SINCE_NOW: u64 = u64::MAX;
/// `kFSEventStreamCreateFlagNoDefer`: the first change of a burst comes at
/// once; the settling is done here.
const NO_DEFER: u32 = 0x02;
/// `kFSEventStreamCreateFlagFileEvents`: each file's own path, not just its
/// folder's, so a change in an ignored folder can be told apart.
const FILE_EVENTS: u32 = 0x10;
/// `kFSEventStreamEventFlagMustScanSubDirs`: events were dropped.
const MUST_SCAN: u32 = 0x01;

/// What the callback needs to name the paths it hears.
struct Watched {
    key: String,
    root: String,
    git_dir: Option<String>,
}

impl Watched {
    /// `path` relative to the project, the outside git folder's named as
    /// if it were the project's own `.git`, which `files::relevant` reads.
    fn relative(&self, path: &str) -> Option<String> {
        let under = |base: &str| {
            path.strip_prefix(base)
                .and_then(|rest| rest.strip_prefix('/'))
                .map(str::to_string)
        };
        if let Some(git) = &self.git_dir {
            if let Some(rest) = under(git) {
                return Some(format!(".git/{rest}"));
            }
        }
        under(&self.root)
    }
}

/// An FSEvents stream on a project, calling back on the main queue.
/// Dropping it stops it.
struct Watch {
    stream: StreamRef,
    info: *mut Watched,
}

impl Watch {
    fn start(key: &str, root: &Path, git_dir: Option<&Path>) -> Option<Watch> {
        let mut dirs = vec![NSString::from_str(&root.to_string_lossy())];
        if let Some(g) = git_dir {
            dirs.push(NSString::from_str(&g.to_string_lossy()));
        }
        let refs: Vec<&NSString> = dirs.iter().map(|d| &**d).collect();
        let array = NSArray::from_slice(&refs);
        let info = Box::into_raw(Box::new(Watched {
            key: key.to_string(),
            root: root.to_string_lossy().trim_end_matches('/').to_string(),
            git_dir: git_dir.map(|g| g.to_string_lossy().trim_end_matches('/').to_string()),
        }));
        let context = StreamContext {
            version: 0,
            info: info.cast(),
            retain: std::ptr::null(),
            release: std::ptr::null(),
            copy_description: std::ptr::null(),
        };
        // SAFETY: an NSArray of NSStrings is toll-free bridged to the
        // CFArray of CFStrings FSEvents takes, and the stream copies it.
        let stream = unsafe {
            FSEventStreamCreate(
                std::ptr::null(),
                heard,
                &context,
                (&*array as *const NSArray<NSString>).cast(),
                SINCE_NOW,
                0.1,
                NO_DEFER | FILE_EVENTS,
            )
        };
        if stream.is_null() {
            // SAFETY: made by `Box::into_raw` above and never shared.
            drop(unsafe { Box::from_raw(info) });
            return None;
        }
        unsafe {
            FSEventStreamSetDispatchQueue(stream, queue::main_queue());
            if FSEventStreamStart(stream) == 0 {
                FSEventStreamInvalidate(stream);
                FSEventStreamRelease(stream);
                drop(Box::from_raw(info));
                return None;
            }
        }
        Some(Watch { stream, info })
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        // SAFETY: on the main thread, the queue the callback runs on, so
        // once invalidated no callback can still be using `info`.
        unsafe {
            FSEventStreamStop(self.stream);
            FSEventStreamInvalidate(self.stream);
            FSEventStreamRelease(self.stream);
            drop(Box::from_raw(self.info));
        }
    }
}

extern "C" fn heard(
    _stream: StreamRef,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    _ids: *const u64,
) {
    // SAFETY: `info` is the `Watched` the stream was made with, alive until
    // it is invalidated; without the CF types flag `paths` is an array of
    // C strings, and `flags` has one entry per path.
    let watched = unsafe { &*(info as *const Watched) };
    let paths = paths as *const *const c_char;
    let mut out = Vec::new();
    let mut all = false;
    for i in 0..count {
        let (path, flag) = unsafe { (CStr::from_ptr(*paths.add(i)), *flags.add(i)) };
        all |= flag & MUST_SCAN != 0;
        if let Some(rel) = watched.relative(&path.to_string_lossy()) {
            out.push(rel);
        }
    }
    if all || !out.is_empty() {
        app::input(Input::Files(Note::Changed {
            key: watched.key.clone(),
            paths: out,
            all,
        }));
    }
}
