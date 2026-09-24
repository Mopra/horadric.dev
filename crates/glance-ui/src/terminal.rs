//! The stage: the one terminal window, showing every session of one
//! project at a time.
//!
//! Each session is a [`Pane`] inside it, laid out in a grid: one fills the
//! window, two sit side by side, four are two by two. A click on a tile
//! switches the stage to that tile's project and gives its session the
//! keyboard. With more than one pane, dragging a pane's header onto another
//! swaps the two.
//!
//! Unlike a cluster, this window takes focus, because you type into it. It
//! is a normal window: resizable, in the taskbar and in alt-tab. Closing it
//! only collapses the sessions back into their tiles; the agents keep
//! running.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::Arc;

use windows::core::{w, Result, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, ScreenToClient};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos,
    GetForegroundWindow, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, IsIconic, IsZoomed,
    LoadCursorW, LoadIconW, RegisterClassW, SetCursor, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowPos, SetWindowTextW, ShowWindow, CREATESTRUCTW, CW_USEDEFAULT, GWLP_USERDATA,
    IDC_ARROW, IDC_SIZEALL, SM_CXMINTRACK, SM_CYMINTRACK, SWP_NOACTIVATE, SWP_NOZORDER, SW_RESTORE,
    SW_SHOWNOACTIVATE, SW_SHOWNORMAL, WINDOW_EX_STYLE, WMSZ_BOTTOM, WMSZ_BOTTOMLEFT,
    WMSZ_BOTTOMRIGHT, WMSZ_LEFT, WMSZ_RIGHT, WMSZ_TOP, WMSZ_TOPLEFT, WMSZ_TOPRIGHT,
    WM_CAPTURECHANGED, WM_CLOSE, WM_DPICHANGED, WM_ENTERSIZEMOVE, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_MOVING, WM_NCCREATE, WM_NCDESTROY, WM_SETFOCUS, WM_SIZE, WM_SIZING, WNDCLASSW,
    WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW,
};

use crate::app::{self, Input};
use crate::backdrop::{self, Material};
use crate::console::Console;
use crate::pane::{self, Pane, WM_PANE_FOCUS, WM_PANE_GRAB};
use crate::window::Shared;
use crate::{layout, palette, snapping, theme};

pub(crate) const CLASS: PCWSTR = w!("GlanceTerminal");
/// Between panes, and around them, in DIPs. Mica shows in the gaps, so the
/// panes float on the window rather than being cut out of it.
const PANE_GAP_DIP: f32 = 6.0;
/// How far a header has to move before a press becomes a drag.
const DRAG_THRESHOLD: i32 = 4;

/// Where the stage opens, in physical pixels, as left, top, right, bottom.
pub enum Place {
    /// The window rectangle it had before.
    Rect([i32; 4]),
    /// These visible edges (see [`TerminalWindow::set_visible_rect`]).
    Visible([i32; 4]),
}

#[derive(Clone, Copy)]
struct Drag {
    /// The pane whose header was pressed.
    serial: usize,
    start: POINT,
    moved: bool,
}

pub struct TerminalWindow {
    pub hwnd: HWND,
    shared: Rc<Shared>,
    /// The project key of the sessions shown, and its name.
    project: RefCell<String>,
    project_name: RefCell<String>,
    /// In grid order. Boxed on purpose: each pane's window procedure holds
    /// a raw pointer to it, so it must not move.
    #[allow(clippy::vec_box)]
    panes: RefCell<Vec<Box<Pane>>>,
    /// Where each pane is, in client pixels, in the same order.
    rects: RefCell<Vec<[i32; 4]>>,
    /// The session that has, or last had, the keyboard.
    active: RefCell<Option<String>>,
    title: RefCell<String>,
    drag: Cell<Option<Drag>>,
    /// The other Glance windows, read when a move or resize starts: they
    /// cannot move while this one does.
    others: RefCell<Vec<snapping::Edges>>,
    /// How far the cursor is from each edge, from the first message of a
    /// move or resize until it ends.
    grab: Cell<Option<[i32; 4]>>,
}

pub fn register_class() -> Result<()> {
    pane::register_class()?;
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let bg = palette::GUTTER;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            // Only the gaps between panes show it.
            hbrBackground: CreateSolidBrush(COLORREF(
                bg.r as u32 | (bg.g as u32) << 8 | (bg.b as u32) << 16,
            )),
            // The icon the build script put in the executable, so a terminal
            // shows as Glance on the taskbar. Resource 1 is the app icon.
            hIcon: LoadIconW(
                Some(instance.into()),
                PCWSTR(std::ptr::without_provenance(1)),
            )
            .unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        Ok(())
    }
}

impl TerminalWindow {
    /// Opens the stage, empty until [`show`](Self::show) fills it, and
    /// brings it to the front.
    pub fn open(shared: Rc<Shared>, place: Place) -> Result<Box<Self>> {
        let mut win = Box::new(TerminalWindow {
            hwnd: HWND::default(),
            shared,
            project: RefCell::new(String::new()),
            project_name: RefCell::new(String::new()),
            panes: RefCell::new(Vec::new()),
            rects: RefCell::new(Vec::new()),
            active: RefCell::new(None),
            title: RefCell::new(String::new()),
            drag: Cell::new(None),
            others: RefCell::new(Vec::new()),
            grab: Cell::new(None),
        });
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS,
                w!("Glance"),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                None,
                None,
                Some(instance.into()),
                Some(&*win as *const TerminalWindow as *const c_void),
            )?;
            win.hwnd = hwnd;

            let dark = BOOL(1);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark as *const _ as *const c_void,
                std::mem::size_of::<BOOL>() as u32,
            );
            // Mica in the title bar and the gaps between panes. The gaps
            // are painted black by the class brush, which DWM takes as
            // clear once the frame reaches into the client area.
            backdrop::apply(hwnd, Material::Mica);

            match place {
                Place::Rect(r) => win.set_rect(r),
                Place::Visible(r) => win.set_visible_rect(r),
            }
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(hwnd);
        }
        Ok(win)
    }

    /// Shows these sessions of a project, in this order, each with its
    /// name. A pane already showing a console stays, with its selection and
    /// scroll position; the rest are made or destroyed to match.
    pub fn show(&self, key: &str, name: &str, sessions: Vec<(Arc<Console>, String)>) {
        // Taken out while panes are made and destroyed: both send messages
        // to this window, whose handlers read the list.
        let mut old = std::mem::take(&mut *self.panes.borrow_mut());
        let before: Vec<usize> = old.iter().map(|p| p.serial()).collect();
        let mut new = Vec::with_capacity(sessions.len());
        for (console, label) in sessions {
            match old.iter().position(|p| p.serial() == console.serial) {
                Some(i) => {
                    let p = old.remove(i);
                    p.set_name(label);
                    new.push(p);
                }
                None => match Pane::create(Rc::clone(&self.shared), console, label, self.hwnd) {
                    Ok(p) => new.push(p),
                    Err(e) => eprintln!("glance: cannot open a pane: {e}"),
                },
            }
        }
        let changed = new.iter().map(|p| p.serial()).ne(before);
        *self.panes.borrow_mut() = new;
        for p in old {
            p.destroy();
        }
        let switched = *self.project.borrow() != key;
        *self.project.borrow_mut() = key.to_string();
        *self.project_name.borrow_mut() = name.to_string();
        let accent = theme::accent(key);
        // The window's edge in the project's colour, like its cluster's mark.
        backdrop::border(self.hwnd, Some(accent));
        for p in self.panes.borrow().iter() {
            p.set_accent(accent);
            if switched {
                p.reveal();
            }
        }
        let keep = self
            .active
            .borrow()
            .as_ref()
            .is_some_and(|a| self.panes.borrow().iter().any(|p| p.session() == a));
        if !keep {
            *self.active.borrow_mut() = self.panes.borrow().first().map(|p| p.session().into());
        }
        if changed {
            self.layout();
        }
        // A pane that had the keyboard may be gone.
        if self.is_foreground() && !self.pane_has_focus() {
            self.focus_active();
        }
        self.spotlight();
        self.refresh_title();
    }

    /// Lights the pane with the keyboard and steps the others back.
    fn spotlight(&self) {
        let active = self.active.borrow().clone();
        let panes = self.panes.borrow();
        let many = panes.len() > 1;
        for p in panes.iter() {
            p.set_dimmed(many && Some(p.session()) != active.as_deref());
        }
    }

    /// Puts each pane in its place in the grid.
    fn layout(&self) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        let panes = self.panes.borrow();
        let gap = (PANE_GAP_DIP * self.dpi() as f32 / 96.0).round() as i32;
        let area = (gap, 0, r.right - gap, r.bottom - gap);
        let rects = layout::grid(panes.len(), area, gap);
        let header = panes.len() > 1;
        for (p, rect) in panes.iter().zip(&rects) {
            p.set_header(header);
            p.set_rect(*rect);
        }
        *self.rects.borrow_mut() = rects;
    }

    fn pane_has_focus(&self) -> bool {
        let focus = unsafe { GetFocus() };
        self.panes.borrow().iter().any(|p| p.hwnd == focus)
    }

    /// Gives the keyboard to the active session's pane.
    fn focus_active(&self) {
        let active = self.active.borrow().clone();
        let panes = self.panes.borrow();
        let pane = panes
            .iter()
            .find(|p| Some(p.session()) == active.as_deref())
            .or(panes.first());
        if let Some(p) = pane {
            p.focus();
        }
    }

    /// Brings the stage forward with the keyboard in this session's pane.
    pub fn focus_session(&self, id: &str) {
        *self.active.borrow_mut() = Some(id.to_string());
        self.bring_to_front();
        self.focus_active();
        self.spotlight();
        self.refresh_title();
    }

    /// The project key of the sessions shown.
    pub fn project(&self) -> String {
        self.project.borrow().clone()
    }

    /// The sessions shown, in grid order.
    pub fn sessions(&self) -> Vec<String> {
        self.panes
            .borrow()
            .iter()
            .map(|p| p.session().to_string())
            .collect()
    }

    /// The session that has the keyboard, or last had it.
    pub fn active(&self) -> Option<String> {
        self.active.borrow().clone()
    }

    pub fn shows(&self, serial: usize) -> bool {
        self.panes.borrow().iter().any(|p| p.serial() == serial)
    }

    /// A console has news: its pane redraws, and the title follows the
    /// active one.
    pub fn refresh(&self, serial: usize) {
        if let Some(p) = self.panes.borrow().iter().find(|p| p.serial() == serial) {
            p.invalidate();
        }
        self.refresh_title();
    }

    /// Redraws every pane, for a change the headers show, such as a phase.
    pub fn invalidate(&self) {
        for p in self.panes.borrow().iter() {
            p.invalidate();
        }
    }

    /// "what the agent says it is doing · session name, project", for the
    /// session with the keyboard.
    pub fn refresh_title(&self) {
        let project = self.project_name.borrow().clone();
        let active = self.active.borrow().clone();
        let title = {
            let panes = self.panes.borrow();
            match panes
                .iter()
                .find(|p| Some(p.session()) == active.as_deref())
            {
                Some(p) => {
                    let name = self
                        .shared
                        .registry
                        .lock()
                        .ok()
                        .and_then(|r| r.get(p.session()).map(|s| s.label().to_string()))
                        .unwrap_or_else(|| p.name());
                    let label = format!("{name}, {project}");
                    let title = match p.console().title() {
                        Some(t) if !t.trim().is_empty() => format!("{} \u{00B7} {label}", t.trim()),
                        _ => label,
                    };
                    match p.console().exit_code() {
                        Some(code) => format!("{title} (exited {code})"),
                        None => title,
                    }
                }
                None => project,
            }
        };
        if *self.title.borrow() != title {
            unsafe {
                let _ = SetWindowTextW(self.hwnd, &HSTRING::from(title.as_str()));
            }
            *self.title.borrow_mut() = title;
        }
    }

    /// Moves and sizes the window without taking the focus, restoring it
    /// first when minimised or maximised.
    pub fn set_rect(&self, [l, t, r, b]: [i32; 4]) {
        unsafe {
            if IsIconic(self.hwnd).as_bool() || IsZoomed(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
            let _ = SetWindowPos(
                self.hwnd,
                None,
                l,
                t,
                r - l,
                b - t,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    /// Like [`set_rect`](Self::set_rect), but for the edges you can see.
    /// A Windows 11 window has an invisible resize border outside its
    /// visible frame, which would otherwise double every gap between
    /// windows laid out side by side.
    pub fn set_visible_rect(&self, [l, t, r, b]: [i32; 4]) {
        self.set_rect([l, t, r, b]);
        let [bl, bt, br, bb] = snapping::border(self.hwnd);
        if [bl, bt, br, bb] != [0; 4] {
            self.set_rect([l - bl, t - bt, r + br, b + bb]);
        }
    }

    /// Where the window is now, unless minimised, when it has no useful
    /// position.
    pub fn rect(&self) -> Option<[i32; 4]> {
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                return None;
            }
            let mut r = RECT::default();
            GetWindowRect(self.hwnd, &mut r).ok()?;
            Some([r.left, r.top, r.right, r.bottom])
        }
    }

    /// Destroys the window and its panes. The consoles live on.
    pub fn destroy(&self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    pub fn bring_to_front(&self) {
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    pub fn is_foreground(&self) -> bool {
        unsafe { GetForegroundWindow() == self.hwnd }
    }

    pub fn dpi(&self) -> u32 {
        unsafe { GetDpiForWindow(self.hwnd) }.max(96)
    }

    /// The pane under the cursor, as an index into the grid.
    fn slot_under_cursor(&self) -> Option<usize> {
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
            let _ = ScreenToClient(self.hwnd, &mut p);
        }
        layout::slot_at(&self.rects.borrow(), p.x, p.y)
    }

    /// A header was pressed. The stage takes the mouse until it is let go.
    fn start_drag(&self, serial: usize) {
        let mut start = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut start);
        }
        self.drag.set(Some(Drag {
            serial,
            start,
            moved: false,
        }));
        unsafe {
            SetCapture(self.hwnd);
        }
    }

    /// Lifts the dragged pane once the cursor has moved far enough, and
    /// marks the pane it would swap with.
    fn drag_to(&self) {
        let Some(mut d) = self.drag.get() else {
            return;
        };
        if !d.moved {
            let mut p = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut p);
            }
            if (p.x - d.start.x).abs() < DRAG_THRESHOLD && (p.y - d.start.y).abs() < DRAG_THRESHOLD
            {
                return;
            }
            d.moved = true;
            self.drag.set(Some(d));
        }
        unsafe {
            SetCursor(LoadCursorW(None, IDC_SIZEALL).ok());
        }
        let slot = self.slot_under_cursor();
        for (i, p) in self.panes.borrow().iter().enumerate() {
            p.set_lifted(p.serial() == d.serial);
            p.set_drop_target(Some(i) == slot && p.serial() != d.serial);
        }
    }

    /// Swaps the dragged pane with the one it was let go on.
    fn drop_drag(&self) {
        let Some(d) = self.drag.take() else {
            return;
        };
        if d.moved {
            let slot = self.slot_under_cursor();
            let panes = self.panes.borrow();
            let from = panes.iter().find(|p| p.serial() == d.serial);
            let onto = slot.and_then(|i| panes.get(i));
            if let (Some(a), Some(b)) = (from, onto) {
                if a.serial() != b.serial() {
                    app::push(Input::Swap(a.session().into(), b.session().into()));
                }
            }
        }
        self.end_drag();
    }

    fn end_drag(&self) {
        self.drag.set(None);
        for p in self.panes.borrow().iter() {
            p.set_lifted(false);
            p.set_drop_target(false);
        }
    }

    /// Where the drag alone would put the window, as left, top, right,
    /// bottom. Windows builds each rect in the move and size loops from the
    /// one returned last, so a snapped rect would snap again on every small
    /// step and never let go. The cursor is the honest measure: the first
    /// message of a drag records how far it is from each edge, and every
    /// edge being dragged keeps that distance.
    fn free_rect(&self, r: &RECT, edges: [bool; 4]) -> [i32; 4] {
        let mut c = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut c);
        }
        let at = [c.x, c.y, c.x, c.y];
        let now = [r.left, r.top, r.right, r.bottom];
        let grab = self.grab.get().unwrap_or_else(|| {
            let g = std::array::from_fn(|i| at[i] - now[i]);
            self.grab.set(Some(g));
            g
        });
        std::array::from_fn(|i| if edges[i] { at[i] - grab[i] } else { now[i] })
    }

    /// Pulls a move in progress onto the lines its visible edges can snap to.
    fn snap_move(&self, r: &mut RECT) {
        let [l, t, rr, b] = self.free_rect(r, [true; 4]);
        let (mut dx, mut dy) = (0, 0);
        if let Some((work, spacing)) = snapping::frame(self.dpi()) {
            // Read each time: a restore from maximised or a new monitor's
            // DPI changes the invisible border mid drag.
            let [bl, bt, br, bb] = snapping::border(self.hwnd);
            let pos = (l + bl, t + bt);
            let size = (rr - br - pos.0, b - bb - pos.1);
            let (x, y) = layout::snap(pos, size, work, &self.others.borrow(), spacing);
            (dx, dy) = (x - pos.0, y - pos.1);
        }
        *r = RECT {
            left: l + dx,
            top: t + dy,
            right: rr + dx,
            bottom: b + dy,
        };
    }

    /// Pulls the edges a resize is dragging onto the lines they can snap to.
    fn snap_size(&self, r: &mut RECT, side: u32) {
        let edges = match side {
            WMSZ_LEFT => [true, false, false, false],
            WMSZ_RIGHT => [false, false, true, false],
            WMSZ_TOP => [false, true, false, false],
            WMSZ_BOTTOM => [false, false, false, true],
            WMSZ_TOPLEFT => [true, true, false, false],
            WMSZ_TOPRIGHT => [false, true, true, false],
            WMSZ_BOTTOMLEFT => [true, false, false, true],
            WMSZ_BOTTOMRIGHT => [false, false, true, true],
            _ => return,
        };
        let mut out = self.free_rect(r, edges);
        if let Some((work, spacing)) = snapping::frame(self.dpi()) {
            let [bl, bt, br, bb] = snapping::border(self.hwnd);
            let [l, t, rr, b] = out;
            let seen = [l + bl, t + bt, rr - br, b - bb];
            let [l, t, rr, b] =
                layout::snap_edges(seen, edges, work, &self.others.borrow(), spacing);
            out = [l - bl, t - bt, rr + br, b + bb];
        }
        // Windows keeps its own rect above the smallest size, the free one
        // has to be kept there by hand.
        let min = unsafe {
            [
                GetSystemMetrics(SM_CXMINTRACK),
                GetSystemMetrics(SM_CYMINTRACK),
            ]
        };
        for (lo, least) in min.into_iter().enumerate() {
            let hi = lo + 2;
            if out[hi] - out[lo] < least {
                if edges[lo] {
                    out[lo] = out[hi] - least;
                } else {
                    out[hi] = out[lo] + least;
                }
            }
        }
        let [l, t, rr, b] = out;
        *r = RECT {
            left: l,
            top: t,
            right: rr,
            bottom: b,
        };
    }

    fn handle(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_SIZE => {
                self.layout();
                None
            }
            WM_DPICHANGED => {
                let r = unsafe { *(lparam.0 as *const RECT) };
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                Some(LRESULT(0))
            }
            WM_ENTERSIZEMOVE => {
                *self.others.borrow_mut() = snapping::others(self.hwnd);
                self.grab.set(None);
                None
            }
            WM_MOVING => {
                self.snap_move(unsafe { &mut *(lparam.0 as *mut RECT) });
                Some(LRESULT(1))
            }
            WM_SIZING => {
                self.snap_size(unsafe { &mut *(lparam.0 as *mut RECT) }, wparam.0 as u32);
                Some(LRESULT(1))
            }
            WM_CLOSE => {
                app::push(Input::Close(self.hwnd.0 as isize));
                Some(LRESULT(0))
            }
            // The window was activated: the keyboard goes back to the pane
            // that had it.
            WM_SETFOCUS => {
                self.focus_active();
                Some(LRESULT(0))
            }
            WM_PANE_FOCUS => {
                let panes = self.panes.borrow();
                if let Some(p) = panes.iter().find(|p| p.serial() == wparam.0) {
                    *self.active.borrow_mut() = Some(p.session().to_string());
                }
                // Headers show which pane has the keyboard.
                for p in panes.iter() {
                    p.invalidate();
                }
                drop(panes);
                self.spotlight();
                self.refresh_title();
                Some(LRESULT(0))
            }
            WM_PANE_GRAB => {
                self.start_drag(wparam.0);
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                self.drag_to();
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                self.drop_drag();
                unsafe {
                    let _ = ReleaseCapture();
                }
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                self.end_drag();
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TerminalWindow;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The app owns the Box and destroys the window before dropping it.
    let win = &*ptr;
    match win.handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
