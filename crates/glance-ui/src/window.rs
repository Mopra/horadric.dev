//! The cluster window: a frameless, rounded Win32 window that never takes
//! focus. It stacks like any other window: a click brings it forward and an
//! editor can cover it.
//!
//! The rules that make it feel like part of Windows rather than an app:
//! `WS_EX_NOACTIVATE` so clicking it does not steal focus from the editor,
//! `WS_EX_TOOLWINDOW` so it stays out of alt-tab and the taskbar, DWM
//! rounded corners so it matches Windows 11, and every move or resize done
//! with `SWP_NOACTIVATE`. Dragging is handled by hand for the same reason:
//! the system move loop would activate the window.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::c_void;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use glance_core::{Defaults, Registry, Session, Usage};
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MonitorFromWindow, ScreenToClient, ValidateRect, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    VK_CONTROL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetWindowLongPtrW, GetWindowRect,
    KillTimer, LoadCursorW, RegisterClassW, SetCursor, SetTimer, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HTCLIENT, HWND_NOTOPMOST,
    HWND_TOPMOST, IDC_ARROW, IDC_SIZENS, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SW_SHOWNOACTIVATE, WM_APP, WM_CAPTURECHANGED, WM_DPICHANGED, WM_ERASEBKGND,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE,
    WM_NCDESTROY, WM_PAINT, WM_RBUTTONUP, WM_SETCURSOR, WM_SIZE, WM_TIMER, WNDCLASSW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::anim::{self, TileIn};
use crate::app::{self, Input, MARGIN_DIP};
use crate::backdrop::{self, Material};
use crate::files::{Change, Expansion, Row, Tree};
use crate::glyphs::Font;
use crate::layout::{self, ClusterLayout, Hit, Metrics};
use crate::render::{FilesScene, Gpu, Scene, Target};
use crate::watch::{self, Slot, Watcher};
use crate::{snapping, theme};

pub(crate) const CLASS: PCWSTR = w!("GlanceCluster");
const DRAG_THRESHOLD: i32 = 4;
/// The watcher left a fresh file tree in the slot.
const WM_CLUSTER_FILES: u32 = WM_APP + 20;
/// The windows crate files this under `Win32_UI_Controls`, a large feature
/// to turn on for one number.
const WM_MOUSELEAVE: u32 = 0x02A3;
/// Rows the files tile scrolls per notch of the wheel.
const WHEEL_ROWS: i32 = 3;
/// Asks for the next frame while something moves.
const ANIM_TIMER: usize = 7;

/// What the windows share with the app: GPU objects, the terminal font,
/// the session store, which sessions are on the stage and which have a
/// browser open, the account's usage and the defaults for new sessions.
pub struct Shared {
    pub gpu: Gpu,
    pub font: Font,
    pub metrics: Metrics,
    pub registry: Arc<Mutex<Registry>>,
    pub staged: RefCell<HashSet<String>>,
    pub browsing: RefCell<HashSet<String>>,
    /// Written by the feeder thread as status lines arrive.
    pub usage: Arc<Mutex<Option<Usage>>>,
    pub defaults: RefCell<Defaults>,
}

/// One project cluster on screen.
pub struct Cluster {
    pub hwnd: HWND,
    /// Project key: sessions whose project key matches are shown here.
    pub key: String,
    pub name: String,
    pub collapsed: bool,
    /// Once the user has dragged it, auto layout leaves it alone.
    pub pinned: bool,
    /// The project folder, as a session spelled it.
    dir: Option<PathBuf>,
    files: RefCell<Files>,
    watcher: Option<Watcher>,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    drag: RefCell<Option<Drag>>,
    resize: RefCell<Option<Resize>>,
    layout: RefCell<ClusterLayout>,
    /// What the cursor is over, for the buttons to light up.
    hot: Cell<Hit>,
    /// What the left button went down on, until it comes up or a drag
    /// starts.
    pressed: Cell<Option<Hit>>,
    /// A WM_MOUSELEAVE has been asked for and not yet sent.
    tracking: Cell<bool>,
    /// Acrylic is behind the window, so it draws with transparency.
    glass: Cell<bool>,
    /// Where each tile has got to on its way somewhere.
    tiles: RefCell<anim::Tiles>,
    /// The frame interval the animation timer runs at, if it runs.
    frames: Cell<Option<Duration>>,
    /// Something besides the moving light changed, so the kept layer of
    /// what holds still has to be drawn again.
    dirty: Cell<bool>,
}

/// The files tile. It only shows once git has answered with a tree.
#[derive(Default)]
struct Files {
    slot: Slot,
    tree: Option<Tree>,
    view: Expansion,
    /// Every row, open folders expanded, rebuilt when the tree or the view
    /// changes rather than on every paint.
    rows: Vec<Row>,
    /// Rows above the top of the tile.
    scroll: usize,
    collapsed: bool,
    /// How tall it may grow below its header before it scrolls, in DIPs,
    /// set by dragging its bottom edge. None for the default.
    cap: Option<f32>,
}

impl Files {
    fn rebuild(&mut self) {
        self.rows = self
            .tree
            .as_ref()
            .map(|t| t.rows(&self.view))
            .unwrap_or_default();
    }

    /// How tall the layout should make the tile below its header, none for
    /// no tile, with the window `base` DIPs tall without it and `room` DIPs
    /// of screen to grow into.
    fn wanted(&self, m: &Metrics, base: f32, room: f32) -> Option<f32> {
        self.tree.as_ref().map(|_| {
            if self.collapsed {
                0.0
            } else {
                layout::files_body(m, self.rows.len(), self.cap, base, room)
            }
        })
    }
}

struct Drag {
    start_cursor: POINT,
    start_window: POINT,
    moved: bool,
    /// The other windows, read once: they cannot move during this drag.
    others: Vec<snapping::Edges>,
}

/// The files tile's bottom edge being dragged.
struct Resize {
    start_y: i32,
    /// The window's bottom edge when the drag began, in physical pixels.
    start_bottom: i32,
    /// The window's height in DIPs with the tile folded.
    base: f32,
    /// The tallest the tile gets: down to the bottom of the monitor.
    max: f32,
    /// Past the drag threshold, so a click leaves the height alone.
    moved: bool,
    /// The other windows, read once: they cannot move during this drag.
    others: Vec<snapping::Edges>,
}

pub fn register_class() -> Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        // Zero means failure. Registering twice is the only realistic cause
        // and is harmless, so no error is surfaced.
        RegisterClassW(&wc);
        Ok(())
    }
}

impl Cluster {
    /// Creates the window hidden at `(x, y)` in physical pixels, then shows it
    /// without activating it.
    pub fn create(
        shared: Rc<Shared>,
        key: String,
        name: String,
        dir: Option<PathBuf>,
        x: i32,
        y: i32,
    ) -> Result<Box<Self>> {
        let n = shared
            .registry
            .lock()
            .map(|r| r.all().filter(|s| project_key(s) == key).count())
            .unwrap_or(0);
        let initial = layout::cluster(&shared.metrics, n, false, None);

        let mut cluster = Box::new(Cluster {
            hwnd: HWND::default(),
            key,
            name,
            collapsed: false,
            pinned: false,
            dir,
            files: RefCell::new(Files::default()),
            watcher: None,
            shared,
            target: RefCell::new(None),
            drag: RefCell::new(None),
            resize: RefCell::new(None),
            layout: RefCell::new(initial),
            hot: Cell::new(Hit::Nothing),
            pressed: Cell::new(None),
            tracking: Cell::new(false),
            glass: Cell::new(false),
            tiles: RefCell::new(anim::Tiles::default()),
            frames: Cell::new(None),
            dirty: Cell::new(true),
        });

        unsafe {
            let instance = GetModuleHandleW(None)?;
            // Size is corrected for DPI right after creation, once the
            // window knows which monitor it is on.
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS,
                w!("Glance"),
                WS_POPUP,
                x,
                y,
                10,
                10,
                None,
                None,
                Some(instance.into()),
                Some(&*cluster as *const Cluster as *const c_void),
            )?;
            cluster.hwnd = hwnd;
            if let Some(dir) = cluster.dir.clone() {
                let slot = cluster.files.borrow().slot.clone();
                cluster.watcher = watch::start(dir, hwnd, WM_CLUSTER_FILES, slot);
            }

            let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            // Hide the 1px system border, the glass draws its own edge.
            backdrop::border(hwnd, None);
            cluster.glass.set(backdrop::apply(hwnd, Material::Acrylic));

            cluster.fit();
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        Ok(cluster)
    }

    pub fn destroy(&self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    pub fn dpi(&self) -> u32 {
        unsafe { GetDpiForWindow(self.hwnd) }.max(96)
    }

    fn scale(&self) -> f32 {
        self.dpi() as f32 / 96.0
    }

    /// Window size in physical pixels for the current layout.
    pub fn size_px(&self) -> (i32, i32) {
        let l = self.layout.borrow();
        let s = self.scale();
        ((l.size.0 * s).round() as i32, (l.size.1 * s).round() as i32)
    }

    pub fn position(&self) -> (i32, i32) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut r);
        }
        (r.left, r.top)
    }

    pub fn move_to(&self, x: i32, y: i32) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                x,
                y,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOSIZE,
            );
        }
    }

    /// Brings the window above other windows without activating it. The
    /// window never activates, so nothing else would bring it forward.
    /// `HWND_TOP` is not enough: Windows keeps a background process below the
    /// foreground window. Going topmost and straight back is allowed, and
    /// leaves the window first among the normal ones.
    pub fn raise(&self) {
        for after in [HWND_TOPMOST, HWND_NOTOPMOST] {
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(after),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
                );
            }
        }
    }

    pub fn invalidate(&self) {
        self.dirty.set(true);
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn sessions(&self) -> Vec<Session> {
        self.shared
            .registry
            .lock()
            .map(|r| {
                r.all()
                    .filter(|s| project_key(s) == self.key)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn files_collapsed(&self) -> bool {
        self.files.borrow().collapsed
    }

    pub fn set_files_collapsed(&self, collapsed: bool) {
        self.files.borrow_mut().collapsed = collapsed;
    }

    pub fn files_height(&self) -> Option<f32> {
        self.files.borrow().cap
    }

    pub fn set_files_height(&self, cap: Option<f32>) {
        self.files.borrow_mut().cap = cap;
    }

    /// The work area of the monitor the window is on, in physical pixels.
    fn work(&self) -> Option<RECT> {
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let monitor = unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST) };
        unsafe { GetMonitorInfoW(monitor, &mut info) }
            .as_bool()
            .then_some(info.rcWork)
    }

    /// The window's height in DIPs with the files tile folded.
    fn base_height(&self, n: usize) -> f32 {
        let folded = self.files.borrow().tree.as_ref().map(|_| 0.0);
        layout::cluster(&self.shared.metrics, n, self.collapsed, folded)
            .size
            .1
    }

    /// Recomputes layout from the registry and resizes the window to fit.
    /// True when the size changed, so the clusters need arranging again.
    pub fn fit(&self) -> bool {
        let sessions = self.sessions();
        let n = sessions.len();
        let m = &self.shared.metrics;
        // The whole height of the monitor rather than what is left below
        // the window: where an unpinned cluster goes depends on its height.
        let room = self.work().map_or(f32::MAX, |w| {
            (w.bottom - w.top) as f32 / self.scale() - 2.0 * MARGIN_DIP as f32
        });
        let wanted = self.files.borrow().wanted(m, self.base_height(n), room);
        let mut l = layout::cluster(m, n, self.collapsed, wanted);
        let marked: Vec<bool> = {
            let browsing = self.shared.browsing.borrow();
            sessions.iter().map(|s| browsing.contains(&s.id)).collect()
        };
        layout::mark(&mut l, m, &marked);
        {
            // Rows can go away under the view: a folder closed, files
            // committed. Never scroll past the last one.
            let mut f = self.files.borrow_mut();
            let shown = l.files.as_ref().map_or(0, |fl| fl.rows.len());
            f.scroll = f.scroll.min(f.rows.len().saturating_sub(shown));
        }
        *self.layout.borrow_mut() = l;
        // The layout can move a button out from under a cursor that has
        // not moved: the plus below the tiles, once a tile is added.
        if self.tracking.get() {
            self.hover(self.cursor_hit());
        }
        // Compare with the real window, not the previous layout: a window is
        // born 10 by 10 and must grow even when its layout never changes.
        let (w, h) = self.size_px();
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut r);
        }
        let changed = (r.right - r.left, r.bottom - r.top) != (w, h);
        if changed {
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    0,
                    0,
                    w,
                    h,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOMOVE,
                );
            }
        }
        self.invalidate();
        changed
    }

    /// Fits, and has the app arrange the clusters when that changed a size.
    fn refit(&self) {
        if self.fit() {
            app::push(Input::Arrange);
        }
    }

    fn paint(&self) {
        let (w, h) = self.size_px();
        let dpi = self.dpi();
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            match Target::with_glass(
                &self.shared.gpu,
                self.hwnd,
                w as u32,
                h as u32,
                dpi,
                self.glass.get(),
            ) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("glance: render target for {}: {e}", self.name);
                    return;
                }
            }
        }
        if std::env::var_os("GLANCE_DEBUG").is_some() {
            eprintln!("paint {} {}x{} dpi {dpi}", self.name, w, h);
        }
        let sessions = self.sessions();
        let refs: Vec<&Session> = sessions.iter().collect();
        let layout = self.layout.borrow();
        let on_stage = {
            let staged = self.shared.staged.borrow();
            refs.iter().any(|s| staged.contains(&s.id))
        };
        let files = self.files.borrow();
        let hot = self.hot.get();
        let pressed = self.pressed.get();
        let inputs: Vec<TileIn> = refs
            .iter()
            .zip(&layout.tiles)
            .enumerate()
            .map(|(i, (s, r))| TileIn {
                id: &s.id,
                phase: &s.phase,
                y: r.y,
                hot: pressed.is_none() && hot == Hit::Tile(i),
            })
            .collect();
        let looks = self.tiles.borrow_mut().step(Instant::now(), &inputs);
        let ambient = backdrop::animations_on();
        // A tile on its way somewhere changes what holds still.
        let moving = looks.iter().zip(&inputs).any(|(l, t)| {
            l.enter < 1.0 || l.arrival > 0.0 || (l.hover > 0.0 && l.hover < 1.0) || l.y != t.y
        });
        let rebuild = self.dirty.replace(false) || moving;
        let scene = Scene {
            layout: &layout,
            name: &self.name,
            collapsed: self.collapsed,
            sessions: &refs,
            looks: &looks,
            on_stage,
            accent: theme::accent(&self.key),
            ambient,
            rebuild,
            now: SystemTime::now(),
            files: files.tree.as_ref().map(|tree| FilesScene {
                tree,
                rows: &files.rows,
                scroll: files.scroll,
                collapsed: files.collapsed,
            }),
            hot: self.hot.get(),
            pressed: self.pressed.get(),
        };
        let result = slot
            .as_ref()
            .map(|t| t.draw(&self.shared.gpu, &self.shared.metrics, &scene));
        // Any EndDraw failure, including D2DERR_RECREATE_TARGET, drops the
        // target. The next paint makes a fresh one.
        if let Some(Err(_)) = result {
            *slot = None;
        }
        let phases: Vec<&glance_core::Phase> = refs.iter().map(|s| &s.phase).collect();
        let targets: Vec<f32> = inputs.iter().map(|t| t.y).collect();
        self.schedule(anim::Tiles::next_frame(&looks, &phases, &targets, ambient));
    }

    /// Keeps the animation timer at the rate the next frame needs, or stops
    /// it, so a cluster with nothing moving costs nothing.
    fn schedule(&self, every: Option<Duration>) {
        if self.frames.replace(every) == every {
            return;
        }
        unsafe {
            match every {
                Some(d) => {
                    SetTimer(Some(self.hwnd), ANIM_TIMER, d.as_millis() as u32, None);
                }
                None => {
                    let _ = KillTimer(Some(self.hwnd), ANIM_TIMER);
                }
            }
        }
    }

    fn handle(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                self.paint();
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd), None);
                }
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            // Only the light moved: the kept layer stays.
            WM_TIMER if wparam.0 == ANIM_TIMER => {
                unsafe {
                    let _ = InvalidateRect(Some(self.hwnd), None, false);
                }
                Some(LRESULT(0))
            }
            WM_CLUSTER_FILES => {
                let fresh = self
                    .files
                    .borrow()
                    .slot
                    .lock()
                    .ok()
                    .and_then(|mut s| s.take());
                if let Some(tree) = fresh {
                    let mut f = self.files.borrow_mut();
                    f.tree = Some(tree);
                    f.rebuild();
                }
                self.refit();
                // A file shown on the stage may be one that changed.
                app::push(Input::FilesChanged(self.key.clone()));
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                // Screen coordinates, unlike every other mouse message.
                let mut p = POINT {
                    x: (lparam.0 & 0xffff) as i16 as i32,
                    y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
                };
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut p);
                }
                let s = self.scale();
                let (x, y) = (p.x as f32 / s, p.y as f32 / s);
                let shown = match &self.layout.borrow().files {
                    Some(fl) if fl.body().contains(x, y) => fl.rows.len(),
                    _ => return Some(LRESULT(0)),
                };
                let notches = ((wparam.0 >> 16) & 0xffff) as i16 as i32 / 120;
                let mut f = self.files.borrow_mut();
                let max = f.rows.len().saturating_sub(shown) as i32;
                let scroll = (f.scroll as i32 - notches * WHEEL_ROWS).clamp(0, max) as usize;
                if scroll != f.scroll {
                    f.scroll = scroll;
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            // The style alone does not stop a click from making the cluster
            // the foreground window, seen on Windows 11. Then the terminal
            // it was meant to leave in front is no longer there.
            WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
            WM_SIZE => {
                let w = (lparam.0 & 0xffff) as u32;
                let h = ((lparam.0 >> 16) & 0xffff) as u32;
                if let Some(t) = self.target.borrow().as_ref() {
                    let _ = t.resize(w, h);
                }
                self.dirty.set(true);
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                let dpi = (wparam.0 & 0xffff) as u32;
                if let Some(t) = self.target.borrow().as_ref() {
                    t.set_dpi(dpi);
                }
                self.dirty.set(true);
                let suggested = unsafe { *(lparam.0 as *const RECT) };
                let (w, h) = self.size_px();
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        suggested.left,
                        suggested.top,
                        w,
                        h,
                        SWP_NOACTIVATE | SWP_NOZORDER,
                    );
                }
                Some(LRESULT(0))
            }
            WM_SETCURSOR if (lparam.0 & 0xffff) as u32 == HTCLIENT => {
                let hit = self.cursor_hit();
                let cursor = if self.resize.borrow().is_some() || hit == Hit::FilesGrip {
                    IDC_SIZENS
                } else {
                    IDC_ARROW
                };
                unsafe {
                    SetCursor(LoadCursorW(None, cursor).ok());
                }
                Some(LRESULT(1))
            }
            WM_LBUTTONDOWN => {
                self.raise();
                let mut cursor = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut cursor);
                    SetCapture(self.hwnd);
                }
                let hit = self.hit(lparam);
                self.press(Some(hit));
                if hit == Hit::FilesGrip {
                    self.start_resize(cursor.y);
                    return Some(LRESULT(0));
                }
                let (x, y) = self.position();
                *self.drag.borrow_mut() = Some(Drag {
                    start_cursor: cursor,
                    start_window: POINT { x, y },
                    moved: false,
                    others: snapping::others(self.hwnd),
                });
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                self.track();
                self.hover(self.hit(lparam));
                if self.resize.borrow().is_some() {
                    self.resizing();
                    return Some(LRESULT(0));
                }
                let mut drag = self.drag.borrow_mut();
                if let Some(d) = drag.as_mut() {
                    let mut cursor = POINT::default();
                    unsafe {
                        let _ = GetCursorPos(&mut cursor);
                    }
                    let dx = cursor.x - d.start_cursor.x;
                    let dy = cursor.y - d.start_cursor.y;
                    if d.moved || dx.abs() > DRAG_THRESHOLD || dy.abs() > DRAG_THRESHOLD {
                        d.moved = true;
                        // A drag that began on a button moves the window
                        // instead, so the button lets go.
                        self.press(None);
                        let (x, y) =
                            self.snapped((d.start_window.x + dx, d.start_window.y + dy), &d.others);
                        self.move_to(x, y);
                    }
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                unsafe {
                    let _ = ReleaseCapture();
                }
                self.press(None);
                if self.resize.borrow_mut().take().is_some() {
                    return Some(LRESULT(0));
                }
                let drag = self.drag.borrow_mut().take();
                match drag {
                    Some(d) if d.moved => app::push(Input::Pin(self.hwnd.0 as isize)),
                    Some(_) => self.click(lparam),
                    None => {}
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking.set(false);
                self.hover(Hit::Nothing);
                Some(LRESULT(0))
            }
            // Capture taken away mid press, by alt tab or a menu: no button
            // up comes, so nothing else would let go of the button.
            WM_CAPTURECHANGED => {
                self.press(None);
                None
            }
            WM_RBUTTONUP => {
                let s = self.scale();
                let x = (lparam.0 & 0xffff) as i16 as f32 / s;
                let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
                match layout::hit(&self.layout.borrow(), x, y) {
                    Hit::Tile(i) | Hit::Browser(i) => {
                        if let Some(s) = self.sessions().get(i) {
                            app::push(Input::TileMenu(s.id.clone()));
                        }
                    }
                    Hit::Header | Hit::Add => app::push(Input::ProjectMenu(self.key.clone())),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    /// Where a drag to `pos` lands once snapped to the edges of the monitor
    /// under the cursor and to the other Glance windows.
    fn snapped(&self, pos: (i32, i32), others: &[snapping::Edges]) -> (i32, i32) {
        // Read each time: crossing onto another monitor can change the DPI.
        match snapping::frame(self.dpi()) {
            Some((work, spacing)) => layout::snap(pos, self.size_px(), work, others, spacing),
            None => pos,
        }
    }

    /// What a mouse message's client coordinates land on.
    fn hit(&self, lparam: LPARAM) -> Hit {
        let s = self.scale();
        let x = (lparam.0 & 0xffff) as i16 as f32 / s;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
        layout::hit(&self.layout.borrow(), x, y)
    }

    /// What the cursor is over right now, wherever it is.
    fn cursor_hit(&self) -> Hit {
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
            let _ = ScreenToClient(self.hwnd, &mut p);
        }
        let s = self.scale();
        layout::hit(&self.layout.borrow(), p.x as f32 / s, p.y as f32 / s)
    }

    /// Asks for a WM_MOUSELEAVE, which Windows sends once per asking.
    fn track(&self) {
        if self.tracking.replace(true) {
            return;
        }
        let mut t = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: self.hwnd,
            dwHoverTime: 0,
        };
        unsafe {
            let _ = TrackMouseEvent(&mut t);
        }
    }

    /// Notes what the cursor is over, repainting when a button changes.
    fn hover(&self, hot: Hit) {
        let old = self.hot.replace(hot);
        if old != hot && (old.lights() || hot.lights()) {
            self.invalidate();
        }
    }

    fn press(&self, pressed: Option<Hit>) {
        if self.pressed.replace(pressed) != pressed {
            self.invalidate();
        }
    }

    fn start_resize(&self, cursor_y: i32) {
        let Some(body) = self.layout.borrow().files.as_ref().map(|f| f.body().h) else {
            return;
        };
        let (_, top) = self.position();
        let base = self.base_height(self.sessions().len());
        let room = self.work().map_or(f32::MAX, |w| {
            (w.bottom - top) as f32 / self.scale() - MARGIN_DIP as f32
        });
        *self.resize.borrow_mut() = Some(Resize {
            start_y: cursor_y,
            start_bottom: top + self.size_px().1,
            base,
            max: (room - base).max(body),
            moved: false,
            others: snapping::others(self.hwnd),
        });
    }

    /// Follows the cursor with the tile's height while its bottom edge is
    /// dragged. The edge snaps like a dragged window does, so it can line
    /// up with the bottom of the stage or the screen.
    fn resizing(&self) {
        let mut cursor = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut cursor);
        }
        let s = self.scale();
        let cap = {
            let mut r = self.resize.borrow_mut();
            let Some(r) = r.as_mut() else {
                return;
            };
            let dy = cursor.y - r.start_y;
            r.moved |= dy.abs() > DRAG_THRESHOLD;
            if !r.moved {
                return;
            }
            let mut rect = [0; 4];
            let mut w = RECT::default();
            unsafe {
                let _ = GetWindowRect(self.hwnd, &mut w);
            }
            rect[0] = w.left;
            rect[1] = w.top;
            rect[2] = w.right;
            rect[3] = r.start_bottom + dy;
            if let Some((work, spacing)) = snapping::frame(self.dpi()) {
                rect =
                    layout::snap_edges(rect, [false, false, false, true], work, &r.others, spacing);
            }
            let body = (rect[3] - rect[1]) as f32 / s - r.base;
            body.clamp(layout::min_files_body(&self.shared.metrics), r.max.max(0.0))
        };
        if self.files.borrow().cap != Some(cap) {
            self.set_files_height(Some(cap));
            self.refit();
        }
    }

    fn click(&self, lparam: LPARAM) {
        match self.hit(lparam) {
            Hit::New => app::push(Input::New(self.key.clone())),
            Hit::Add => app::push(Input::Add(self.key.clone())),
            Hit::Header => app::push(Input::Toggle(self.hwnd.0 as isize)),
            Hit::FilesHeader => {
                let collapsed = !self.files_collapsed();
                self.set_files_collapsed(collapsed);
                self.refit();
            }
            Hit::File(i) => self.file_clicked(i),
            Hit::FilesGrip => {}
            Hit::Tile(i) => {
                // Tiles are laid out in registry order, the same order
                // `sessions` returns.
                if let Some(s) = self.sessions().get(i) {
                    app::push(Input::Expand(s.id.clone()));
                }
            }
            Hit::Browser(i) => {
                if let Some(s) = self.sessions().get(i) {
                    app::push(Input::Browser(s.id.clone()));
                }
            }
            Hit::Nothing => {}
        }
    }

    /// A folder opens or closes; a file opens on the stage, or with Ctrl
    /// held in the editor.
    fn file_clicked(&self, i: usize) {
        {
            let mut f = self.files.borrow_mut();
            let f = &mut *f;
            let (Some(tree), Some(row)) = (&f.tree, f.rows.get(f.scroll + i)) else {
                return;
            };
            let node = tree.node(row.node);
            if !node.dir {
                // A deleted file has nothing to open.
                if node.change != Some(Change::Deleted) {
                    if let Some(dir) = &self.dir {
                        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
                        if ctrl {
                            watch::open(dir, &node.path);
                        } else {
                            app::push(Input::View(
                                self.key.clone(),
                                dir.clone(),
                                node.path.clone(),
                            ));
                        }
                    }
                }
                return;
            }
            f.view.toggle(node);
            f.rebuild();
        }
        self.refit();
    }
}

/// The project a session belongs to. For now its working directory,
/// normalised. Worktrees will map back to their repository in step 4.
pub fn project_key(s: &Session) -> String {
    s.cwd
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

/// A readable name for a project key.
pub fn project_name(key: &str) -> String {
    key.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(key)
        .to_string()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Cluster;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The Box lives in the app for as long as the window exists, and the app
    // destroys the window before dropping the Box.
    let cluster = &*ptr;
    match cluster.handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
