//! The composer at the bottom of a Folio: the text being written, images
//! and folded pastes, the `/` and `@` menus, and the prompts queued while
//! Claude works, above it. The box holds only the text; the settings
//! (permission mode, model, effort, the context meter and stop) sit in a
//! quiet strip at the top of the glass.
//!
//! What it does to text is in [`super::editor`], its menus' matching in
//! [`super::picker`], what the controls say in [`super::controls`]. Here
//! is the state per session, the keys and clicks, and the drawing.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use horadric_core::folio::{effort_line, prompt_line, ImageData, Priority};
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_BITMAP_PROPERTIES,
};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{InvalidateRect, MapWindowPoints, LOGFONTW};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Input::Ime::{
    ImmGetContext, ImmReleaseContext, ImmSetCandidateWindow, ImmSetCompositionFontW,
    ImmSetCompositionWindow, CANDIDATEFORM, CFS_EXCLUDE, CFS_POINT, COMPOSITIONFORM,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_DELETE, VK_DOWN, VK_END, VK_HOME, VK_LEFT,
    VK_RIGHT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::GetParent;
use windows_numerics::Vector2;

use super::controls;
use super::editor::{Editor, History};
use super::picker::{self, Item, Menu, MenuKind};
use super::view::{d2d, rounded, scroll_by, view_of, Gfx, Hit, Palette, View};
use super::{redraw, with, Chat};
use crate::clipboard;
use crate::field;
use crate::keys::Mods;
use crate::layout::Rect;
use crate::render::{self, Gpu};

const PAD: f32 = 16.0;
const BOTTOM: f32 = 16.0;
const LINES: f32 = 8.0;
/// The composer's plate and the field sunk into it, which sits this far
/// inside, so the text stays `PAD` from the plate's edge.
const PLATE_R: f32 = 14.0;
const WELL_INSET: f32 = 6.0;
const WELL_R: f32 = PLATE_R - WELL_INSET;
/// The height of the buttons in the pane's header row.
pub(super) const STRIP_H: f32 = 22.0;
/// The round key at the right of the field that sends, or stops Claude.
const KEY: f32 = 26.0;
const THUMB: f32 = 52.0;
const ROW_H: f32 = 26.0;
const MENU_ROW: f32 = 30.0;
/// A thumbnail is decoded this big, in pixels, and drawn smaller.
const THUMB_PX: u32 = 104;
/// How long a file list is trusted before the next `@` lists again.
const FILES_FRESH: Duration = Duration::from_secs(20);
/// Files a project without git lists at most.
const WALK_MAX: usize = 20_000;
/// The biggest image file sent as an image: the API takes 5 MB of it in
/// base64, which is four thirds of the file.
const IMAGE_MAX: u64 = 5 * 1024 * 1024 / 4 * 3;
const TOO_BIG: &str = "That image is too big to send whole (the most is 3.75 MB).";
/// History entries longer than this are not kept, so the file stays small.
const HISTORY_ENTRY_MAX: usize = 20_000;

const PLACEHOLDER: &str = "Message Claude";
const PLACEHOLDER_BUSY: &str = "Claude is working. Enter queues, Esc stops it.";
const PLACEHOLDER_ENDED: &str = "This session has ended.";

/// What a click on the composer does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Act {
    Text,
    Stop,
    Send,
    Settings,
    Pick(usize),
    Unattach(usize),
    Unqueue(usize),
}

/// An image waiting to go with the prompt.
struct Attached {
    data: ImageData,
    thumb: Option<Thumb>,
}

struct Thumb {
    w: u32,
    h: u32,
    /// Premultiplied BGRA.
    pixels: Vec<u8>,
}

/// A prompt sent while Claude works, held here until the turn ends so it
/// can still be taken back.
struct Waiting {
    text: String,
    images: Vec<Attached>,
}

#[derive(Default)]
struct Listing {
    files: Vec<String>,
    at: Option<Instant>,
    running: bool,
    /// Counts lists made, so an open menu knows to filter again.
    made: u64,
}

/// What is kept of a composer across reloads.
#[derive(Serialize, Deserialize, Default)]
struct Saved {
    #[serde(default)]
    draft: String,
    #[serde(default)]
    pastes: Vec<String>,
    #[serde(default)]
    history: Vec<String>,
    /// Prompts queued while Claude worked, so a reload loses none.
    #[serde(default)]
    queue: Vec<SavedPrompt>,
}

/// A queued prompt as it is kept, its images as the API takes them.
#[derive(Serialize, Deserialize, Default)]
struct SavedPrompt {
    text: String,
    #[serde(default)]
    images: Vec<(String, String)>,
}

/// One session's composer.
pub(super) struct Input {
    /// The session's id, once known: what its draft and history are kept
    /// under.
    key: Option<String>,
    pub(super) ed: Editor,
    history: History,
    scroll: f32,
    menu: Option<Menu>,
    /// A menu closed with Esc stays closed for the word it was for.
    quiet: Option<(MenuKind, usize)>,
    images: Vec<Attached>,
    queue: Vec<Waiting>,
    notice: Option<String>,
    files: Arc<Mutex<Listing>>,
    files_seen: u64,
    hover: Option<Act>,
    tracking: bool,
    /// The level last picked; the CLI does not say.
    effort: Option<String>,
    /// The model last picked, by what `set_model` took.
    model: Option<String>,
    /// Whether this session may bypass permissions: it was in that mode.
    bypass: bool,
    busy_was: bool,
    /// Stopped by the human: the queue comes back at the turn's end.
    held: bool,
    /// The queue came back from disk: once the session is live it waits
    /// for the turn running, or with none it goes back into the text.
    restored: bool,
    dirty: bool,
    /// Where the box's top was last drawn, for the menu above it.
    top: f32,
    /// Where the header's button was last drawn, to open its menus under
    /// it.
    anchor: f32,
    /// Where the open menu was last drawn, for the wheel over it.
    pub(super) menu_at: Option<Rect>,
    ime: Option<[i32; 4]>,
}

impl Input {
    pub(super) fn new() -> Input {
        Input {
            key: None,
            ed: Editor::default(),
            history: History::default(),
            scroll: 0.0,
            menu: None,
            quiet: None,
            images: Vec::new(),
            queue: Vec::new(),
            notice: None,
            files: Arc::default(),
            files_seen: 0,
            hover: None,
            tracking: false,
            effort: None,
            model: None,
            bypass: false,
            busy_was: false,
            held: false,
            restored: false,
            dirty: false,
            top: 0.0,
            anchor: 0.0,
            menu_at: None,
            ime: None,
        }
    }

    /// Scrolls the open menu by `rows`, as the wheel over it does.
    pub(super) fn scroll_menu(&mut self, rows: isize) {
        if let Some(m) = &mut self.menu {
            m.scroll(rows);
        }
    }

    /// Reads what was kept for session `id` the first time it is known.
    fn ensure(&mut self, id: &str) {
        if self.key.is_some() {
            return;
        }
        self.key = Some(id.to_string());
        let saved: Saved = path_of(id)
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if self.ed.is_empty() {
            self.ed = Editor::from_parts(&saved.draft, saved.pastes);
        }
        self.history = History::new(saved.history);
        if self.queue.is_empty() && !saved.queue.is_empty() {
            self.restored = true;
            self.queue = (saved.queue.into_iter())
                .map(|p| Waiting {
                    text: p.text,
                    images: (p.images.into_iter())
                        .map(|(media_type, base64)| Attached {
                            data: ImageData { media_type, base64 },
                            thumb: None,
                        })
                        .collect(),
                })
                .collect();
        }
    }

    pub(super) fn save(&mut self) {
        self.dirty = false;
        let Some(path) = self.key.as_deref().and_then(path_of) else {
            return;
        };
        let saved = Saved {
            draft: self.ed.text().to_string(),
            pastes: self.ed.pastes().to_vec(),
            history: self.history.entries.clone(),
            queue: (self.queue.iter())
                .map(|w| SavedPrompt {
                    text: w.text.clone(),
                    images: (w.images.iter())
                        .map(|a| (a.data.media_type.clone(), a.data.base64.clone()))
                        .collect(),
                })
                .collect(),
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(bytes) = serde_json::to_vec(&saved) {
            let _ = std::fs::write(path, bytes);
        }
    }

    /// The text changed: menus follow it, the history walk stops.
    fn edited(&mut self) {
        self.changed();
        self.notice = None;
        self.history.stop();
    }

    /// What is kept changed: it is written a moment after the last change,
    /// away from painting.
    fn changed(&mut self) {
        self.dirty = true;
        super::save_soon();
    }

    /// Whether what is kept waits to be written.
    pub(super) fn dirty(&self) -> bool {
        self.dirty
    }

    /// The session is gone for good: so is what was kept of it.
    pub(super) fn forget(&mut self) {
        self.dirty = false;
        if let Some(path) = self.key.take().as_deref().and_then(path_of) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// What waits goes back into the text, as when Claude is stopped.
    pub(super) fn fold_queue(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        let waiting = std::mem::take(&mut self.queue);
        let mut texts: Vec<String> = Vec::new();
        for w in waiting {
            texts.push(w.text);
            self.images.extend(w.images);
        }
        let now = self.ed.text().to_string();
        if !now.trim().is_empty() {
            texts.push(now);
        }
        self.ed.set_text(&texts.join("\n\n"));
        self.edited();
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        if self.dirty {
            self.save();
        }
    }
}

/// Where a project's composer is kept: its draft and its history.
fn path_of(key: &str) -> Option<PathBuf> {
    let safe: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let safe = if safe.len() > 120 {
        safe[safe.len() - 120..].to_string()
    } else {
        safe
    };
    Some(
        crate::store::dir()?
            .join("folio")
            .join(format!("{safe}.json")),
    )
}

/// Writes a line to the CLI. False when it did not go.
fn write(chat: &mut Chat, line: &str) -> bool {
    let Some(h) = &chat.hosted else {
        return false;
    };
    match h.send_line(line) {
        Ok(()) => true,
        Err(e) => {
            chat.error = Some(format!("Claude did not get that: {e}"));
            false
        }
    }
}

/// Keeps the session's side of the composer up to date: asks for the
/// commands and models once, and at a turn's end the context's size and
/// the next queued prompt. Called as lines are read and as it paints.
pub(super) fn tend(id: &str, chat: &mut Chat) {
    chat.input.ensure(id);
    if chat.hosted.is_none() || chat.ended {
        return;
    }
    // A replay's turns ended long ago: nothing is sent for them.
    if !chat.live || chat.loading.is_some() {
        chat.input.busy_was = chat.folio.busy;
        return;
    }
    // Once per claude.exe: a reattach's replay says whether it went.
    if chat.live && !chat.folio.initialized {
        let line = chat.folio.initialize();
        write(chat, &line);
    }
    if chat.folio.permission_mode() == "bypassPermissions" {
        chat.input.bypass = true;
    }
    let busy = chat.folio.busy;
    if chat.input.restored && chat.live && chat.loading.is_none() {
        // A turn that ended while no one listened would not send it, and
        // one hours later should not either: it goes back into the text.
        chat.input.restored = false;
        if !busy {
            chat.input.fold_queue();
        }
    }
    if chat.input.busy_was && !busy {
        let line = chat.folio.get_context_usage();
        write(chat, &line);
        // Claude was stopped: what waited goes back into the composer
        // rather than run, as the CLI does.
        if std::mem::take(&mut chat.input.held) {
            chat.input.fold_queue();
        } else if !chat.input.queue.is_empty() {
            let next = chat.input.queue.remove(0);
            chat.input.changed();
            let images: Vec<ImageData> = next.images.into_iter().map(|a| a.data).collect();
            deliver(chat, &next.text, &images);
        }
    }
    chat.input.busy_was = chat.folio.busy;
}

/// Stops Claude. What was queued comes back into the composer once the
/// turn has ended.
pub(super) fn hold(chat: &mut Chat) {
    if chat.folio.busy {
        chat.input.held = true;
    }
}

/// Starts a turn with `text` and `images`.
fn deliver(chat: &mut Chat, text: &str, images: &[ImageData]) {
    let line = if images.is_empty() {
        chat.folio.push_prompt(text);
        prompt_line(text)
    } else {
        chat.folio.push_prompt_with_images(text, images)
    };
    if !write(chat, &line) {
        // No turn started, so none will end with a result.
        chat.folio.end();
    }
    chat.input.busy_was = chat.folio.busy;
    chat.follow = true;
    chat.note_busy();
}

/// Sends what the composer holds. While Claude works it waits for the
/// turn's end, or with `now` cuts in.
pub(super) fn send(chat: &mut Chat, now: bool) {
    // While the history loads, a prompt would land above it.
    if chat.ended || chat.to_terminal || chat.hosted.is_none() || chat.loading.is_some() {
        return;
    }
    let text = chat.input.ed.expanded().trim().to_string();
    if text.is_empty() && chat.input.images.is_empty() {
        return;
    }
    if let Some(name) = terminal_command(&text) {
        to_terminal(chat, &format!("/{name}"));
        return;
    }
    if text.len() <= HISTORY_ENTRY_MAX {
        chat.input.history.push(&text);
    }
    let images = std::mem::take(&mut chat.input.images);
    chat.input.ed.clear();
    chat.input.scroll = 0.0;
    chat.input.menu = None;
    chat.input.notice = None;
    chat.input.save();
    if !chat.folio.busy {
        let data: Vec<ImageData> = images.into_iter().map(|a| a.data).collect();
        deliver(chat, &text, &data);
    } else if now && images.is_empty() {
        let line = chat.folio.queue_prompt(&text, Priority::Now);
        write(chat, &line);
        chat.follow = true;
    } else {
        chat.input.queue.push(Waiting { text, images });
        chat.input.changed();
    }
}

/// A command only the terminal runs: the session goes on in a terminal,
/// where it can be typed again, once Claude rests.
fn to_terminal(chat: &mut Chat, command: &str) {
    let idle = !chat.folio.busy && chat.folio.pending_asks().is_empty();
    match chat.input.key.clone() {
        Some(id) if idle && chat.folio.session_id.is_some() => {
            chat.input.notice = Some(format!(
                "{command} runs in the terminal: the session goes on there."
            ));
            crate::app::push(crate::app::Input::FolioToTerminal(id));
        }
        _ => {
            chat.input.notice = Some(format!(
                "{command} runs in the terminal. Open it there once Claude rests."
            ));
        }
    }
}

/// The command a prompt runs, when it is one only the terminal can.
fn terminal_command(text: &str) -> Option<&'static str> {
    let name = text.strip_prefix('/')?.split_whitespace().next()?;
    picker::TERMINAL_ONLY.iter().copied().find(|t| *t == name)
}

/// Opens or follows the `/` and `@` menus for the word at the caret.
fn follow_menu(chat: &mut Chat) {
    let inp = &mut chat.input;
    if inp.menu.as_ref().is_some_and(|m| m.kind.is_picker()) {
        return;
    }
    let (start, word) = inp.ed.word_at_caret();
    let word = word.to_string();
    let kind = if start == 0 && word.starts_with('/') {
        Some(MenuKind::Slash)
    } else if word.starts_with('@') {
        Some(MenuKind::Files)
    } else {
        None
    };
    let Some(kind) = kind else {
        inp.menu = None;
        inp.quiet = None;
        return;
    };
    if inp.quiet == Some((kind, start)) {
        return;
    }
    inp.quiet = None;
    let items = match kind {
        MenuKind::Slash => picker::slash_items(&chat.folio.info, &word[1..]),
        _ => {
            list_files(chat);
            let inp = &mut chat.input;
            inp.files_seen = inp.files.lock().map(|l| l.made).unwrap_or(0);
            let typed = word[1..].trim_start_matches('"');
            match inp.files.lock() {
                Ok(l) => picker::file_items(&l.files, typed),
                Err(_) => Vec::new(),
            }
        }
    };
    let inp = &mut chat.input;
    if kind == MenuKind::Slash && items.is_empty() {
        inp.menu = None;
        return;
    }
    inp.menu = Some(Menu::new(kind, items, start));
}

/// Lists the project's files in the background, unless a list is fresh.
fn list_files(chat: &Chat) {
    let Ok(mut l) = chat.input.files.lock() else {
        return;
    };
    if l.running || l.at.is_some_and(|t| t.elapsed() < FILES_FRESH) {
        return;
    }
    l.running = true;
    drop(l);
    let (dir, files) = (chat.dir.clone(), Arc::clone(&chat.input.files));
    let view = chat.view.map_or(0, |v| v.0 as isize);
    std::thread::spawn(move || {
        let found = picker::with_folders(git_files(&dir).unwrap_or_else(|| walk(&dir)));
        if let Ok(mut l) = files.lock() {
            l.files = found;
            l.at = Some(Instant::now());
            l.running = false;
            l.made += 1;
        }
        if view != 0 {
            unsafe {
                let _ = InvalidateRect(Some(HWND(view as *mut _)), None, false);
            }
        }
    });
}

fn git_files(dir: &Path) -> Option<Vec<String>> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = Command::new("git")
        .args([
            "--no-optional-locks",
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| crate::files::parse_list(&out.stdout))
}

/// A project without git: its files, past hidden and build folders.
fn walk(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || matches!(name.as_str(), "node_modules" | "target") {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                todo.push(p);
            } else if let Ok(rel) = p.strip_prefix(dir) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
                if out.len() >= WALK_MAX {
                    return out;
                }
            }
        }
    }
    out
}

/// How a menu item was taken.
#[derive(Clone, Copy, PartialEq, Eq)]
enum How {
    Enter,
    Tab,
    Click,
}

/// Takes the selected item of the open menu. False when there was none.
fn pick(chat: &mut Chat, how: How) -> bool {
    let Some(menu) = chat.input.menu.take() else {
        return false;
    };
    let Some(item) = menu.picked().cloned() else {
        chat.input.menu = Some(menu);
        return false;
    };
    match menu.kind {
        MenuKind::Slash => {
            let caret = chat.input.ed.field.caret;
            if item.terminal {
                chat.input.ed.replace(menu.start, caret, "");
                to_terminal(chat, &item.label);
                return true;
            }
            let put = if item.value.starts_with('@') {
                format!("{} ", item.value)
            } else {
                format!("/{} ", item.value)
            };
            chat.input.ed.replace(menu.start, caret, &put);
            chat.input.edited();
            if how == How::Enter && item.hint.is_empty() && !item.value.starts_with('@') {
                send(chat, false);
            }
        }
        MenuKind::Files => {
            let caret = chat.input.ed.field.caret;
            let folder = item.value.ends_with('/');
            let put = picker::mention(&item.value);
            let put = if folder { put } else { format!("{put} ") };
            chat.input.ed.replace(menu.start, caret, &put);
            chat.input.edited();
            if folder {
                follow_menu(chat);
            }
        }
        MenuKind::Mode => set_mode(chat, &item.value),
        MenuKind::Model => {
            let line = chat.folio.set_model(&item.value);
            if write(chat, &line) {
                chat.input.model = Some(item.value);
            }
        }
        MenuKind::Effort => set_effort(chat, &item.value),
        MenuKind::Tasks => {}
        MenuKind::Settings => match item.value.as_str() {
            "mode" => open_picker(chat, MenuKind::Mode),
            "model" => open_picker(chat, MenuKind::Model),
            "effort" => open_picker(chat, MenuKind::Effort),
            "tasks" => open_picker(chat, MenuKind::Tasks),
            "context" => {
                let line = chat.folio.get_context_usage();
                write(chat, &line);
            }
            "terminal" => {
                if let Some(id) = chat.input.key.clone() {
                    crate::app::push(crate::app::Input::FolioToTerminal(id));
                }
            }
            _ => {}
        },
    }
    true
}

fn set_mode(chat: &mut Chat, mode: &str) {
    let line = chat.folio.set_permission_mode(mode);
    write(chat, &line);
}

/// `/effort` is a prompt: it runs now when Claude rests, else after the
/// turn.
fn set_effort(chat: &mut Chat, level: &str) {
    chat.input.effort = Some(level.to_string());
    let text = format!("/effort {level}");
    if chat.folio.busy {
        chat.input.queue.push(Waiting {
            text,
            images: Vec::new(),
        });
        chat.input.changed();
        return;
    }
    let line = effort_line(level);
    chat.folio.push_prompt(&text);
    if !write(chat, &line) {
        chat.folio.end();
    }
    chat.input.busy_was = chat.folio.busy;
    chat.note_busy();
}

/// The model's effort levels, from what `initialize` listed.
fn effort_levels(chat: &Chat) -> Vec<String> {
    let models = &chat.folio.info.models;
    let current = chat.input.model.as_deref();
    let m = current
        .and_then(|p| models.iter().find(|m| m.value == p))
        .or_else(|| {
            let id = chat.folio.model.as_deref()?;
            models.iter().find(|m| m.resolved == id)
        })
        .or_else(|| models.iter().find(|m| m.value == "default"));
    m.map(|m| m.effort_levels.clone()).unwrap_or_default()
}

/// Whether the session can go on in a terminal now: only between turns,
/// since claude.exe ends first, and once there is a conversation to resume.
fn terminal_ready(chat: &Chat) -> bool {
    !chat.folio.busy
        && !chat.ended
        && !chat.to_terminal
        && chat.folio.session_id.is_some()
        && chat.folio.pending_asks().is_empty()
}

/// The header's menu: a row per setting with what it is now.
fn settings_items(chat: &Chat) -> Vec<Item> {
    let row = |value: &str, label: &str, detail: String| Item {
        label: label.to_string(),
        detail,
        value: value.to_string(),
        ..Default::default()
    };
    let mode = chat.folio.permission_mode();
    let mode = if mode.is_empty() { "default" } else { mode };
    let model = controls::model_label(
        chat.folio.model.as_deref(),
        chat.input.model.as_deref(),
        &chat.folio.info.models,
    );
    let mut items = vec![
        row("mode", "Mode", controls::mode_label(mode).to_string()),
        row("model", "Model", model),
    ];
    if !effort_levels(chat).is_empty() {
        let effort = chat.input.effort.clone().unwrap_or("Default".to_string());
        items.push(row("effort", "Effort", effort));
    }
    if let Some(ctx) = chat.folio.context.as_ref().filter(|c| c.limit > 0) {
        items.push(row("context", "Context", controls::meter(ctx.used, ctx.limit).1));
    }
    let tasks = chat.folio.background.len();
    if tasks > 0 {
        items.push(row("tasks", "Tasks", controls::tasks_line(tasks)));
    }
    if terminal_ready(chat) {
        items.push(row("terminal", "Terminal", "Go on in a terminal".to_string()));
    }
    items
}

/// Opens one of the pickers under the header's button, or closes it when
/// open.
fn open_picker(chat: &mut Chat, kind: MenuKind) {
    if chat.input.menu.as_ref().is_some_and(|m| m.kind == kind) {
        chat.input.menu = None;
        return;
    }
    let items: Vec<Item> = match kind {
        MenuKind::Mode => {
            let now = chat.folio.permission_mode().to_string();
            controls::modes(chat.input.bypass)
                .map(|(m, label)| Item {
                    label: label.to_string(),
                    value: m.to_string(),
                    current: m == now,
                    detail: if m == "bypassPermissions" {
                        "Asks for nothing".to_string()
                    } else {
                        String::new()
                    },
                    ..Default::default()
                })
                .collect()
        }
        MenuKind::Model => {
            let picked = chat.input.model.clone();
            let id = chat.folio.model.clone();
            chat.folio
                .info
                .models
                .iter()
                .map(|m| Item {
                    label: m.display_name.clone(),
                    detail: m.description.clone(),
                    value: m.value.clone(),
                    current: match &picked {
                        Some(p) => *p == m.value,
                        None => id.as_deref() == Some(m.resolved.as_str()) && m.value != "default",
                    },
                    ..Default::default()
                })
                .collect()
        }
        MenuKind::Settings => settings_items(chat),
        MenuKind::Tasks => (chat.folio.background.iter())
            .map(|t| Item {
                label: controls::task_kind(&t.task_type).to_string(),
                detail: controls::task_label(t),
                value: t.task_id.clone(),
                ..Default::default()
            })
            .collect(),
        _ => {
            let now = chat.input.effort.clone();
            effort_levels(chat)
                .into_iter()
                .map(|l| Item {
                    label: l.clone(),
                    current: now.as_deref() == Some(l.as_str()),
                    value: l,
                    ..Default::default()
                })
                .collect()
        }
    };
    chat.input.menu = Some(Menu::new(kind, items, 0));
}

/// A key that makes a character, from the pane.
pub(super) fn on_char(key: &str, c: char, mods: Mods) {
    with(key, |chat| {
        chat.input.ensure(key);
        let mut edit = true;
        match c {
            // A pick says itself what it changed.
            '\r' if chat.input.menu.is_some() && pick(chat, How::Enter) => edit = false,
            '\r' if mods.shift => chat.input.ed.type_str("\n"),
            '\r' => {
                send(chat, false);
                edit = false;
            }
            // Ctrl+Enter.
            '\n' => {
                send(chat, true);
                edit = false;
            }
            '\t' if mods.shift => {
                let now = chat.folio.permission_mode().to_string();
                let next = controls::next_mode(&now, chat.input.bypass);
                set_mode(chat, next);
                edit = false;
            }
            '\t' => {
                pick(chat, How::Tab);
                edit = false;
            }
            '\u{1b}' => {
                edit = false;
                if let Some(m) = chat.input.menu.take() {
                    if matches!(m.kind, MenuKind::Slash | MenuKind::Files) {
                        chat.input.quiet = Some((m.kind, m.start));
                    }
                } else if !chat.input.ed.field.selected().is_empty() {
                    // A selection goes first; a second Esc stops Claude.
                    let f = &mut chat.input.ed.field;
                    f.set_caret(f.caret, false);
                } else if chat.folio.busy {
                    hold(chat);
                    if let Some(h) = &chat.hosted {
                        let _ = h.interrupt();
                    }
                } else {
                    let f = &mut chat.input.ed.field;
                    f.set_caret(f.caret, false);
                }
            }
            '\u{8}' => chat.input.ed.backspace(false),
            '\u{7f}' => chat.input.ed.backspace(true),
            '\u{1}' => {
                chat.input.ed.field.select_all();
                edit = false;
            }
            '\u{3}' => {
                let s = chat.input.ed.field.selected();
                if !s.is_empty() {
                    clipboard::set_text(s);
                }
                edit = false;
            }
            '\u{18}' => {
                if !chat.input.ed.field.selected().is_empty() {
                    clipboard::set_text(&chat.input.ed.cut());
                }
            }
            '\u{16}' => paste(chat),
            '\u{1a}' => {
                chat.input.ed.undo();
            }
            '\u{19}' => {
                chat.input.ed.redo();
            }
            c if (c as u32) < 0x20 => edit = false,
            c => chat.input.ed.type_str(c.encode_utf8(&mut [0; 4])),
        }
        if edit {
            chat.input.edited();
        }
        follow_menu(chat);
    });
    redraw(key);
}

/// Ctrl+V: an image becomes a thumbnail, copied files their `@` paths or
/// thumbnails, and text goes in, folded when big.
fn paste(chat: &mut Chat) {
    if let Some(text) = clipboard::get_text() {
        chat.input.ed.paste(&text);
        return;
    }
    if clipboard::has_image() {
        let Some(path) = clipboard::save_image() else {
            chat.input.notice = Some("That image could not be read.".to_string());
            return;
        };
        if !small_enough(&path) {
            chat.input.notice = Some(TOO_BIG.to_string());
            return;
        }
        let added = std::fs::read(&path)
            .ok()
            .is_some_and(|bytes| attach(chat, &bytes));
        if !added {
            chat.input.notice = Some("That image could not be read.".to_string());
        }
        return;
    }
    if clipboard::has_files() {
        let paths: Vec<PathBuf> = clipboard::get_files()
            .into_iter()
            .map(PathBuf::from)
            .collect();
        add_paths(chat, &paths);
    }
}

/// Whether the file at `path` is small enough to send as an image, known
/// before it is read.
fn small_enough(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.len() <= IMAGE_MAX)
}

/// Adds an image from a file's bytes. False when it is not one the API
/// takes.
fn attach(chat: &mut Chat, bytes: &[u8]) -> bool {
    let Some(data) = ImageData::from_bytes(bytes) else {
        return false;
    };
    let thumb = unsafe { decode_thumb(bytes) };
    chat.input.images.push(Attached { data, thumb });
    true
}

/// Files dropped or pasted: images become thumbnails, the rest `@` paths,
/// relative to the project when inside it.
fn add_paths(chat: &mut Chat, paths: &[PathBuf]) {
    let mut mentions = Vec::new();
    let mut too_big = false;
    for p in paths {
        let image = p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp"
            )
        });
        // One too big to send goes as its path, for Claude to read.
        if image && !small_enough(p) {
            too_big = true;
        } else if image && std::fs::read(p).ok().is_some_and(|b| attach(chat, &b)) {
            continue;
        }
        let shown = p
            .strip_prefix(&chat.dir)
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| p.to_string_lossy().into_owned());
        mentions.push(picker::mention(&shown));
    }
    if !mentions.is_empty() {
        let ed = &mut chat.input.ed;
        let before = ed.field.caret;
        let space = before > 0 && !ed.text()[..before].ends_with(char::is_whitespace);
        let lead = if space { " " } else { "" };
        ed.insert(&format!("{lead}{} ", mentions.join(" ")));
    }
    chat.input.edited();
    if too_big {
        chat.input.notice = Some(TOO_BIG.to_string());
    }
}

/// Files dropped on the conversation.
pub(super) fn drop_files(key: &str, paths: Vec<String>) {
    with(key, |chat| {
        let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        add_paths(chat, &paths);
    });
    redraw(key);
}

/// A key that makes no character, from the pane. False leaves it to the
/// pane.
pub(super) fn on_key(key: &str, vk: u16, mods: Mods) -> bool {
    let (word, extend) = (mods.ctrl, mods.shift);
    let moved = |f: fn(&mut Chat, bool, bool)| {
        with(key, |c| {
            f(c, word, extend);
            c.input.ed.moved();
            follow_menu(c);
        })
        .is_some()
    };
    let done = match vk {
        v if v == VK_LEFT.0 => moved(|c, w, e| c.input.ed.field.left(w, e)),
        v if v == VK_RIGHT.0 => moved(|c, w, e| c.input.ed.field.right(w, e)),
        v if v == VK_HOME.0 => moved(|c, w, e| c.input.ed.field.home(w, e)),
        v if v == VK_END.0 => moved(|c, w, e| c.input.ed.field.end(w, e)),
        v if v == VK_DELETE.0 => with(key, |c| {
            c.input.ed.delete(word);
            c.input.edited();
            follow_menu(c);
        })
        .is_some(),
        v if v == VK_UP.0 => vertical(key, true, extend),
        v if v == VK_DOWN.0 => vertical(key, false, extend),
        _ => false,
    };
    if done {
        redraw(key);
    }
    done
}

/// Up or Down: through an open menu, else a line of the text, else past
/// its first or last line through the prompts sent before, else the
/// conversation scrolls.
fn vertical(key: &str, up: bool, extend: bool) -> bool {
    let walk = with(key, |c| {
        if let Some(m) = &mut c.input.menu {
            if up {
                m.up()
            } else {
                m.down()
            }
            return Some(true);
        }
        c.input.history.walking().then_some(false)
    })
    .flatten();
    if walk == Some(true) {
        return true;
    }
    if walk.is_none() {
        let line = view_of(key).and_then(|v| v.caret_line(key, up, extend));
        if line.is_some() {
            with(key, |c| c.input.ed.moved());
            return true;
        }
    }
    let recalled = with(key, |c| {
        let inp = &mut c.input;
        let got = if up {
            let now = inp.ed.expanded();
            inp.history.older(&now)
        } else {
            inp.history.newer()
        };
        let text = got?;
        inp.ed.set_text(&text);
        inp.scroll = 0.0;
        Some(())
    })
    .flatten();
    if recalled.is_none() {
        scroll_by(key, if up { -54.0 } else { 54.0 });
    }
    true
}

/// Decodes an image into a small premultiplied BGRA thumbnail.
unsafe fn decode_thumb(bytes: &[u8]) -> Option<Thumb> {
    let factory: IWICImagingFactory =
        CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
    let stream = factory.CreateStream().ok()?;
    stream.InitializeFromMemory(bytes).ok()?;
    let decoder = factory
        .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
        .ok()?;
    let frame = decoder.GetFrame(0).ok()?;
    let (mut w, mut h) = (0u32, 0u32);
    frame.GetSize(&mut w, &mut h).ok()?;
    if w == 0 || h == 0 {
        return None;
    }
    let s = (THUMB_PX as f32 / w.max(h) as f32).min(1.0);
    let (tw, th) = (
        ((w as f32 * s).round() as u32).max(1),
        ((h as f32 * s).round() as u32).max(1),
    );
    let scaler = factory.CreateBitmapScaler().ok()?;
    scaler
        .Initialize(&frame, tw, th, WICBitmapInterpolationModeFant)
        .ok()?;
    let converter = factory.CreateFormatConverter().ok()?;
    converter
        .Initialize(
            &scaler,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
        .ok()?;
    let mut pixels = vec![0u8; (tw * th * 4) as usize];
    converter
        .CopyPixels(std::ptr::null(), tw * 4, &mut pixels)
        .ok()?;
    Some(Thumb {
        w: tw,
        h: th,
        pixels,
    })
}

/// How wide `s` is set in `fmt`.
/// The composer as the rest of the app is built: a plate standing off the
/// glass, lit along its top and casting its shadow down, with the field
/// the text is written in sunk into it like the pane's own glass, its rim
/// lit in the project's colour while it has the keys.
unsafe fn faceplate(gfx: &Gfx, r: &Rect, pal: &Palette, focused: bool) {
    let p = crate::theme::palette();
    let black = crate::theme::Color::rgb(0);
    let white = crate::theme::Color::rgb(0xFFFFFF);
    for i in 1..=4 {
        let s = i as f32 * 1.5;
        let fall = Rect::new(r.x - s, r.y + 2.0 + s, r.w + 2.0 * s, r.h);
        gfx.fill_round(&fall, PLATE_R + s, p.cast.fade(0.12));
    }
    let side = Rect::new(r.x, r.y + 2.0, r.w, r.h);
    gfx.fill_round(&side, PLATE_R, p.surface.mix(black, 0.6));
    gfx.fill_round(r, PLATE_R, p.surface.mix(white, 0.02));
    let half = r.h / 2.0;
    edge(gfx, r, PLATE_R, (r.y - 1.0, r.y + half), p.bevel_light);
    edge(gfx, r, PLATE_R, (r.y + half, r.bottom() + 1.0), p.bevel_shade.fade(0.6));

    let well = r.inset(WELL_INSET);
    let lip = Rect::new(well.x - 0.5, well.y + 0.5, well.w + 1.0, well.h + 1.0);
    gfx.stroke_round(&lip, WELL_R + 0.5, p.engrave_light.fade(1.6), 1.0);
    gfx.fill_round(&well, WELL_R, pal.bg);
    gfx.shade.SetStartPoint(Vector2 { X: 0.0, Y: well.y });
    gfx.shade.SetEndPoint(Vector2 {
        X: 0.0,
        Y: well.y + 10.0,
    });
    let band = Rect::new(well.x - 1.0, well.y - 1.0, well.w + 2.0, 11.0);
    gfx.rt
        .PushAxisAlignedClip(&d2d(&band), D2D1_ANTIALIAS_MODE_ALIASED);
    gfx.rt
        .FillRoundedRectangle(&rounded(&well, WELL_R), &gfx.shade);
    gfx.rt.PopAxisAlignedClip();
    let rim = if focused {
        pal.accent.with_alpha(0.55)
    } else {
        p.engrave_dark
    };
    gfx.stroke_round(&well, WELL_R, rim, 1.0);
}

/// The outline of `r` between `ys` only: a bevel's lit top or shaded
/// bottom.
unsafe fn edge(
    gfx: &Gfx,
    r: &Rect,
    radius: f32,
    (top, bottom): (f32, f32),
    c: crate::theme::Color,
) {
    let band = Rect::new(r.x - 1.0, top, r.w + 2.0, bottom - top);
    gfx.rt
        .PushAxisAlignedClip(&d2d(&band), D2D1_ANTIALIAS_MODE_ALIASED);
    gfx.stroke_round(r, radius, c, 1.0);
    gfx.rt.PopAxisAlignedClip();
}

/// A small round key raised out of the field, cut from the same stuff as
/// the plate: a soft cast below, a lit top rim and a shaded bottom one.
/// Pressed while the mouse is on it, so it sinks a little.
unsafe fn round_key(gfx: &Gfx, b: &Rect, lit: bool) {
    let p = crate::theme::palette();
    let white = crate::theme::Color::rgb(0xFFFFFF);
    let rad = b.h / 2.0;
    let drop = if lit { 0.5 } else { 1.5 };
    for i in 1..=2 {
        let s = i as f32;
        let fall = Rect::new(b.x - s * 0.5, b.y + drop + s * 0.5, b.w + s, b.h);
        gfx.fill_round(&fall, rad + s * 0.5, p.cast.fade(0.18));
    }
    let face = p.surface.mix(white, if lit { 0.09 } else { 0.05 });
    gfx.fill_round(b, rad, face);
    let half = b.h / 2.0;
    edge(gfx, b, rad, (b.y - 1.0, b.y + half), p.bevel_light);
    edge(gfx, b, rad, (b.y + half, b.bottom() + 1.0), p.bevel_shade.fade(0.5));
}

fn width_of(gpu: &Gpu, fmt: &IDWriteTextFormat, s: &str) -> f32 {
    render::wrapped(gpu, fmt, s, 10_000.0)
        .map(|l| render::text_size(&l).0)
        .unwrap_or(0.0)
}


impl View {
    /// The composer at the bottom: queued prompts and a notice above it,
    /// then in its box images, the text with its caret and selection, and
    /// the controls. Returns all it covers.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn composer(
        &self,
        gfx: &Gfx,
        chat: &mut Chat,
        pal: &Palette,
        cx: f32,
        cw: f32,
        h: f32,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> Rect {
        tend(&self.key, chat);
        let gpu = &self.shared.gpu;
        // The text stops short of the key at the right.
        let inner_w = (cw - 2.0 * PAD - KEY - 8.0).max(10.0);
        let empty = chat.input.ed.is_empty();
        let shown = if empty { "M" } else { chat.input.ed.text() };
        let Ok(l) = render::wrapped(gpu, &gfx.text, shown, inner_w) else {
            return Rect::new(cx, h, cw, 0.0);
        };
        let (_, th) = render::text_size(&l);
        let line_h = render::wrapped(gpu, &gfx.text, "M", inner_w)
            .map(|l| render::text_size(&l).1)
            .unwrap_or(20.0);
        let view_h = th.clamp(line_h, line_h * LINES);
        let images_h = if chat.input.images.is_empty() {
            0.0
        } else {
            THUMB + 10.0
        };
        let bh = PAD + images_h + view_h + PAD;
        let r = Rect::new(cx, h - BOTTOM - bh, cw, bh);
        chat.input.top = r.y;
        // A card's field or the find bar may have the keys instead: one
        // caret on the glass, never two.
        let focused = self.has_keyboard() && chat.tr.state.focus.is_none();
        faceplate(gfx, &r, pal, focused);
        hits.push((r, Hit::Input(Act::Text)));

        // Images waiting to go, each with a cross to take it back.
        let mut x = r.x + PAD;
        for (i, a) in chat.input.images.iter().enumerate() {
            let tile = Rect::new(x, r.y + PAD, THUMB, THUMB);
            gfx.fill_round(&tile, 8.0, pal.fg.with_alpha(0.08));
            if let Some(t) = &a.thumb {
                let s = (THUMB / t.w.max(t.h) as f32).min(THUMB / 8.0);
                let (w, hh) = (t.w as f32 * s, t.h as f32 * s);
                let at = Rect::new(
                    tile.x + (THUMB - w) / 2.0,
                    tile.y + (THUMB - hh) / 2.0,
                    w,
                    hh,
                );
                self.bitmap(gfx, t, &at);
            }
            let close = Rect::new(tile.right() - 18.0, tile.y + 2.0, 16.0, 16.0);
            let hot = chat.input.hover == Some(Act::Unattach(i));
            gfx.fill_round(
                &close,
                8.0,
                pal.bg.with_alpha(if hot { 0.95 } else { 0.75 }),
            );
            gfx.text_in("\u{E711}", &gfx.icon, &close, pal.fg);
            hits.push((close, Hit::Input(Act::Unattach(i))));
            x += THUMB + 8.0;
        }

        let (ox, oy) = (r.x + PAD, r.y + PAD + images_h);
        let text_box = Rect::new(ox, oy, inner_w, view_h);
        if empty {
            let hint = if chat.ended {
                PLACEHOLDER_ENDED
            } else if chat.folio.busy {
                PLACEHOLDER_BUSY
            } else {
                PLACEHOLDER
            };
            gfx.text_in(
                hint,
                &gfx.line,
                // Clear of the caret standing at its start.
                &Rect::new(ox + 4.0, oy, inner_w - 4.0, line_h),
                pal.faint,
            );
            chat.input.scroll = 0.0;
        } else {
            let f = &chat.input.ed.field;
            let caret = render::caret_at(&l, field::utf16_at(&f.text, f.caret));
            chat.input.scroll = field::follow(chat.input.scroll, caret.y, caret.h, view_h, th);
        }
        let sy = oy - chat.input.scroll;
        gfx.rt
            .PushAxisAlignedClip(&d2d(&text_box), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        if !empty {
            let text = chat.input.ed.text();
            for (a, b) in chat.input.ed.chips() {
                let (a16, b16) = (field::utf16_at(text, a), field::utf16_at(text, b));
                for s in render::range_rects(&l, a16, b16 - a16) {
                    gfx.fill_round(
                        &Rect::new(ox + s.x - 2.0, sy + s.y + 1.0, s.w + 4.0, s.h - 2.0),
                        5.0,
                        pal.accent.with_alpha(0.22),
                    );
                }
            }
            let (a, b) = chat.input.ed.field.selection();
            let (a16, b16) = (field::utf16_at(text, a), field::utf16_at(text, b));
            for s in render::range_rects(&l, a16, b16 - a16) {
                gfx.fill(
                    &Rect::new(ox + s.x, sy + s.y, s.w, s.h),
                    pal.accent.with_alpha(0.35),
                );
            }
            gfx.layout_at(&l, ox, sy, pal.fg);
        }
        let caret = {
            let f = &chat.input.ed.field;
            render::caret_at(
                &l,
                if empty {
                    0
                } else {
                    field::utf16_at(&f.text, f.caret)
                },
            )
        };
        if focused {
            gfx.fill(
                &Rect::new(ox + caret.x, sy + caret.y, 1.5, caret.h),
                pal.accent.mix(pal.fg, 0.4),
            );
        }
        gfx.rt.PopAxisAlignedClip();
        if focused {
            self.place_ime(chat, ox + caret.x, sy + caret.y, caret.h);
        }
        *self.composer.borrow_mut() = (!empty).then_some((l, ox, sy));
        self.send_key(gfx, chat, pal, &r, line_h, empty, hits);

        let top = self.above(gfx, chat, pal, cx, cw, r.y, hits);
        Rect::new(cx, top, cw, r.bottom() - top)
    }

    /// The key at the right of the field, level with the last line: stop
    /// while Claude works, else send, its arrow only lit once there is
    /// something to send.
    #[allow(clippy::too_many_arguments)]
    unsafe fn send_key(
        &self,
        gfx: &Gfx,
        chat: &Chat,
        pal: &Palette,
        r: &Rect,
        line_h: f32,
        empty: bool,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        if chat.ended {
            return;
        }
        let y = (r.bottom() - PAD - line_h / 2.0 - KEY / 2.0).round();
        let margin = r.bottom() - WELL_INSET - (y + KEY);
        let b = Rect::new(r.right() - WELL_INSET - margin - KEY, y, KEY, KEY);
        let busy = chat.folio.busy;
        let act = if busy { Act::Stop } else { Act::Send };
        let ready = busy || !empty || !chat.input.images.is_empty();
        let lit = ready && chat.input.hover == Some(act);
        round_key(gfx, &b, lit);
        let ink = match (ready, lit) {
            (false, _) => pal.faint.fade(0.45),
            (true, true) => pal.accent.mix(pal.fg, 0.25),
            (true, false) => pal.accent,
        };
        if busy {
            let sq = Rect::new(b.x + KEY / 2.0 - 4.5, b.y + KEY / 2.0 - 4.5, 9.0, 9.0);
            gfx.fill_round(&sq, 2.0, ink);
        } else {
            gfx.text_in("\u{E74A}", &gfx.icon, &b, ink);
        }
        if ready {
            hits.push((b, Hit::Input(act)));
        }
    }

    /// The pane's header, kept to one button that opens the settings
    /// (mode, model, effort, context, tasks, Terminal) as a menu. Laid
    /// leftwards from `right` at `cy`.
    pub(super) unsafe fn strip(
        &self,
        gfx: &Gfx,
        chat: &mut Chat,
        pal: &Palette,
        right: f32,
        cy: f32,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let hover = chat.input.hover;
        let open = chat.input.menu.as_ref().is_some_and(|m| m.kind.is_picker());
        let b = Rect::new(right - STRIP_H - 4.0, cy, STRIP_H + 4.0, STRIP_H);
        let lit = open || hover == Some(Act::Settings);
        if lit {
            gfx.fill_round(&b, STRIP_H / 2.0, pal.fg.with_alpha(0.08));
        }
        // The one setting that still shows: a mode that asks for nothing,
        // or one that only plans, tints the button.
        let ink = match chat.folio.permission_mode() {
            "bypassPermissions" => crate::theme::Color::rgb(0xE0_8A_3C),
            "plan" => pal.accent,
            _ if lit => pal.fg,
            _ => pal.faint,
        };
        gfx.text_in("\u{E712}", &gfx.icon, &b, ink);
        hits.push((b, Hit::Input(Act::Settings)));
        // The menus open on the glass, so the anchor is in its DIPs.
        chat.input.anchor = b.x + chat.bar_shift;
    }

    /// The notice and the queued prompts, stacked up from `bottom`.
    /// Returns the top of what it drew.
    #[allow(clippy::too_many_arguments)]
    unsafe fn above(
        &self,
        gfx: &Gfx,
        chat: &Chat,
        pal: &Palette,
        cx: f32,
        cw: f32,
        bottom: f32,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f32 {
        let mut y = bottom - 4.0;
        let inset = cx + 14.0;
        let w = cw - 28.0;
        // The CLI's own queue: prompts sent now, which cannot be taken
        // back once sent.
        for q in chat.folio.queued.iter().rev() {
            y -= ROW_H + 4.0;
            let row = Rect::new(inset, y, w, ROW_H);
            gfx.fill_round(&row, 8.0, pal.fg.with_alpha(0.06));
            gfx.text_in(
                "\u{E8AB}",
                &gfx.icon,
                &Rect::new(row.x + 8.0, y, 14.0, ROW_H),
                pal.accent,
            );
            gfx.text_in(
                &one_line(&q.text),
                &gfx.small,
                &Rect::new(row.x + 28.0, y, row.w - 36.0, ROW_H),
                pal.dim,
            );
        }
        for (i, q) in chat.input.queue.iter().enumerate().rev() {
            y -= ROW_H + 4.0;
            let row = Rect::new(inset, y, w, ROW_H);
            gfx.fill_round(&row, 8.0, pal.fg.with_alpha(0.06));
            gfx.text_in(
                "\u{E823}",
                &gfx.icon,
                &Rect::new(row.x + 8.0, y, 14.0, ROW_H),
                pal.faint,
            );
            let mut said = one_line(&q.text);
            if !q.images.is_empty() {
                said = format!(
                    "{said}  (+{} image{})",
                    q.images.len(),
                    if q.images.len() == 1 { "" } else { "s" }
                );
            }
            gfx.text_in(
                &said,
                &gfx.small,
                &Rect::new(row.x + 28.0, y, row.w - 64.0, ROW_H),
                pal.dim,
            );
            let x = Rect::new(row.right() - 26.0, y + 3.0, 20.0, 20.0);
            if chat.input.hover == Some(Act::Unqueue(i)) {
                gfx.fill_round(&x, 10.0, pal.fg.with_alpha(0.1));
            }
            gfx.text_in("\u{E711}", &gfx.icon, &x, pal.dim);
            hits.push((x, Hit::Input(Act::Unqueue(i))));
        }
        if let Some(n) = &chat.input.notice {
            y -= ROW_H;
            gfx.text_in(n, &gfx.small, &Rect::new(inset, y, w, ROW_H), pal.dim);
        }
        y
    }

    /// The open menu, over the conversation above the composer. Drawn after
    /// the conversation so it lies on top.
    pub(super) unsafe fn composer_menu(
        &self,
        gfx: &Gfx,
        chat: &mut Chat,
        pal: &Palette,
        cx: f32,
        cw: f32,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        // A file list that came after the menu opened is filtered now.
        let made = chat.input.files.lock().map(|l| l.made).unwrap_or(0);
        if made != chat.input.files_seen
            && chat
                .input
                .menu
                .as_ref()
                .is_some_and(|m| m.kind == MenuKind::Files)
        {
            chat.input.menu = None;
            follow_menu(chat);
        }
        chat.input.menu_at = None;
        let Some(menu) = &chat.input.menu else {
            return;
        };
        let gpu = &self.shared.gpu;
        let picker = menu.kind.is_picker();
        let (x, w) = if picker {
            let a = chat.input.anchor;
            let w = match menu.kind {
                MenuKind::Model | MenuKind::Tasks => 340.0,
                MenuKind::Settings => 280.0,
                _ => 220.0,
            };
            (a.min(cx + cw - w).max(cx), w.min(cw))
        } else {
            (cx, cw)
        };
        let rows = menu.items.len().clamp(1, picker::SHOWN);
        let mh = rows as f32 * MENU_ROW + 8.0;
        let y = if picker {
            6.0
        } else {
            chat.input.top - 6.0 - mh
        };
        let r = Rect::new(x, y, w, mh);
        chat.input.menu_at = Some(r);
        let menu = chat.input.menu.as_ref().expect("checked above");
        gfx.fill_round(&r, 12.0, pal.bg.mix(pal.fg, 0.1));
        gfx.stroke_round(&r, 12.0, pal.fg.with_alpha(0.16), 1.0);
        hits.push((r, Hit::Input(Act::Text)));
        if menu.items.is_empty() {
            let listing = chat.input.files.lock().is_ok_and(|l| l.running);
            let says = if listing {
                "Listing files\u{2026}"
            } else {
                "No matches"
            };
            gfx.text_in(
                says,
                &gfx.small,
                &Rect::new(r.x + 14.0, r.y + 4.0, w - 28.0, MENU_ROW),
                pal.faint,
            );
            return;
        }
        for (row, (i, item)) in menu.shown().enumerate() {
            let y = r.y + 4.0 + row as f32 * MENU_ROW;
            let line = Rect::new(r.x + 4.0, y, w - 8.0, MENU_ROW);
            if i == menu.selected {
                gfx.fill_round(&line, 8.0, pal.accent.with_alpha(0.2));
            }
            let mut tx = line.x + 10.0;
            if item.current {
                gfx.text_in(
                    "\u{E73E}",
                    &gfx.icon,
                    &Rect::new(tx, y, 14.0, MENU_ROW),
                    pal.accent,
                );
                tx += 18.0;
            }
            let label_w = width_of(gpu, &gfx.title, &item.label).ceil().min(w * 0.45);
            let ink = if item.terminal { pal.dim } else { pal.fg };
            gfx.text_in(
                &item.label,
                &gfx.title,
                &Rect::new(tx, y, label_w + 2.0, MENU_ROW),
                ink,
            );
            tx += label_w + 8.0;
            if !item.hint.is_empty() {
                let hw = width_of(gpu, &gfx.small, &item.hint).ceil().min(w * 0.25);
                gfx.text_in(
                    &item.hint,
                    &gfx.small,
                    &Rect::new(tx, y, hw + 2.0, MENU_ROW),
                    pal.faint,
                );
                tx += hw + 10.0;
            }
            let room = line.right() - 10.0 - tx;
            if room > 20.0 && !item.detail.is_empty() {
                gfx.text_in(
                    &item.detail,
                    &gfx.small,
                    &Rect::new(tx, y, room, MENU_ROW),
                    pal.dim,
                );
            }
            hits.push((line, Hit::Input(Act::Pick(i))));
        }
    }

    unsafe fn bitmap(&self, gfx: &Gfx, t: &Thumb, at: &Rect) {
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        let size = D2D_SIZE_U {
            width: t.w,
            height: t.h,
        };
        let pixels = t.pixels.as_ptr() as *const std::ffi::c_void;
        if let Ok(b) = gfx.rt.CreateBitmap(size, Some(pixels), t.w * 4, &props) {
            gfx.rt.DrawBitmap(
                &b,
                Some(&d2d(at)),
                1.0,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            );
        }
    }

    /// Puts the input method's composition at the caret, in the pane that
    /// has the keyboard, and keeps its candidates off the line.
    unsafe fn place_ime(&self, chat: &mut Chat, x: f32, y: f32, h: f32) {
        let s = self.scale();
        let Ok(pane) = GetParent(self.hwnd()) else {
            return;
        };
        let mut pts = [
            POINT {
                x: (x * s).round() as i32,
                y: (y * s).round() as i32,
            },
            POINT {
                x: ((x + 1.0) * s).round() as i32,
                y: ((y + h) * s).round() as i32,
            },
        ];
        MapWindowPoints(Some(self.hwnd()), Some(pane), &mut pts);
        let rect = [pts[0].x, pts[0].y, pts[1].x, pts[1].y];
        if chat.input.ime == Some(rect) {
            return;
        }
        chat.input.ime = Some(rect);
        let mut font = LOGFONTW {
            lfHeight: -(super::view::READ * s).round() as i32,
            ..Default::default()
        };
        let family: Vec<u16> = "Segoe UI Variable Text".encode_utf16().collect();
        font.lfFaceName[..family.len()].copy_from_slice(&family);
        let composition = COMPOSITIONFORM {
            dwStyle: CFS_POINT,
            ptCurrentPos: pts[0],
            rcArea: RECT::default(),
        };
        let candidates = CANDIDATEFORM {
            dwIndex: 0,
            dwStyle: CFS_EXCLUDE,
            ptCurrentPos: POINT {
                x: rect[0],
                y: rect[3],
            },
            rcArea: RECT {
                left: rect[0],
                top: rect[1],
                right: rect[2],
                bottom: rect[3],
            },
        };
        let imc = ImmGetContext(pane);
        if imc.is_invalid() {
            return;
        }
        let _ = ImmSetCompositionFontW(imc, &font);
        let _ = ImmSetCompositionWindow(imc, &composition);
        let _ = ImmSetCandidateWindow(imc, &candidates);
        let _ = ImmReleaseContext(pane, imc);
    }

    /// A click on the composer.
    pub(super) fn press_input(&self, act: Act, x: f32, y: f32, double: bool, shift: bool) {
        let key = self.key.clone();
        let mut select = false;
        with(&key, |chat| {
            if !matches!(act, Act::Pick(_) | Act::Settings | Act::Text) {
                chat.input.menu = None;
            }
            match act {
                Act::Send => send(chat, false),
                Act::Stop => {
                    hold(chat);
                    if let Some(h) = &chat.hosted {
                        let _ = h.interrupt();
                    }
                }
                // A second click closes whichever picker it led to.
                Act::Settings => {
                    if chat.input.menu.as_ref().is_some_and(|m| m.kind.is_picker()) {
                        chat.input.menu = None;
                    } else {
                        chat.input.ensure(&key);
                        open_picker(chat, MenuKind::Settings);
                    }
                }
                Act::Pick(i) => {
                    if let Some(m) = &mut chat.input.menu {
                        m.selected = i;
                    }
                    pick(chat, How::Click);
                }
                Act::Unattach(i) => {
                    if i < chat.input.images.len() {
                        chat.input.images.remove(i);
                    }
                }
                Act::Unqueue(i) => {
                    if i < chat.input.queue.len() {
                        chat.input.queue.remove(i);
                        chat.input.changed();
                    }
                }
                Act::Text => {
                    if chat.input.menu.as_ref().is_some_and(|m| m.kind.is_picker()) {
                        chat.input.menu = None;
                    }
                    select = true;
                }
            }
        });
        if select {
            if let Some(at) = self.composer_at(x, y) {
                with(&key, |c| {
                    if double {
                        c.input.ed.field.select_word(at);
                    } else {
                        c.input.ed.field.set_caret(at, shift);
                    }
                    c.input.ed.moved();
                    follow_menu(c);
                });
                if !double {
                    self.selecting.set(true);
                    unsafe {
                        windows::Win32::UI::Input::KeyboardAndMouse::SetCapture(self.hwnd());
                    }
                }
            } else if self.composer.borrow().is_none() {
                with(&key, |c| c.input.ed.moved());
            }
        }
        redraw(&key);
    }

    /// The mouse moved over the view: what lies under it lights up, and a
    /// drag from the text selects.
    pub(super) fn hover_input(&self, x: f32, y: f32) {
        let key = self.key.clone();
        if self.selecting.get() {
            if let Some(at) = self.composer_at(x, y) {
                with(&key, |c| c.input.ed.field.set_caret(at, true));
                redraw(&key);
            }
            return;
        }
        let now = match self.hit_at(x, y) {
            Some(Hit::Input(a)) if a != Act::Text => Some(a),
            _ => None,
        };
        let track = with(&key, |c| {
            let mut changed = c.input.hover != now;
            c.input.hover = now;
            if let (Some(Act::Pick(i)), Some(m)) = (now, c.input.menu.as_mut()) {
                changed |= m.selected != i;
                m.selected = i;
            }
            (changed, !std::mem::replace(&mut c.input.tracking, true))
        });
        let Some((changed, track)) = track else {
            return;
        };
        if track {
            let mut t = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd(),
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut t);
            }
        }
        if changed {
            redraw(&key);
        }
    }

    /// The mouse left the view.
    pub(super) fn leave_input(&self) {
        let key = self.key.clone();
        let changed = with(&key, |c| {
            c.input.tracking = false;
            c.input.hover.take().is_some()
        });
        if changed == Some(true) {
            redraw(&key);
        }
    }

    /// Moves the composer's caret a line up or down. None when it is on
    /// the first or the last line already.
    pub(super) fn caret_line(&self, key: &str, up: bool, extend: bool) -> Option<()> {
        let held = self.composer.borrow();
        let (l, _, _) = held.as_ref()?;
        with(key, |c| {
            let f = &mut c.input.ed.field;
            let caret = render::caret_at(l, field::utf16_at(&f.text, f.caret));
            let y = if up {
                caret.y - caret.h / 2.0
            } else {
                caret.y + caret.h * 1.5
            };
            let (_, th) = render::text_size(l);
            if y < 0.0 || y > th {
                return None;
            }
            let at = render::offset_at(l, caret.x, y);
            f.set_caret(field::byte_at(&f.text, at), extend);
            Some(())
        })?
    }

    /// The composer's text under a point in the view, as a byte offset.
    pub(super) fn composer_at(&self, x: f32, y: f32) -> Option<usize> {
        let held = self.composer.borrow();
        let (l, ox, oy) = held.as_ref()?;
        let at = render::offset_at(l, x - ox, y - oy);
        with(&self.key, |c| field::byte_at(&c.input.ed.field.text, at))
    }
}

/// A queued prompt in one line.
fn one_line(s: &str) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(200).collect()
}
