//! The Settings window: every setting of the app in one place, sections
//! down the left and the rows of the one picked on the right.
//!
//! A window of its own that closes, not a tile in the columns: settings
//! are visited, not watched. Drawn like the usage window, its lists drop
//! the same list. There is no Save button. A row changes the setting the
//! moment it is clicked or picked, as the tray's lines used to, so the
//! window and the app never disagree. The app owns the values and hands
//! them in again after every change.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;

use horadric_core::saved::Discord;
use horadric_core::{Agent, Defaults, Setting};
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MonitorFromPoint, ScreenToClient, ValidateRect, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_DOWN, VK_ESCAPE,
    VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetWindowLongPtrW, GetWindowRect,
    IsIconic, LoadCursorW, RegisterClassW, SendMessageW, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HICON,
    HTCAPTION, HTCLIENT, ICON_BIG, ICON_SMALL, IDC_ARROW, SWP_NOACTIVATE, SWP_NOZORDER, SW_RESTORE,
    SW_SHOW, WM_CAPTURECHANGED, WM_CLOSE, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY, WM_NCHITTEST, WM_PAINT, WM_SETICON,
    WM_SIZE, WNDCLASSW, WS_EX_APPWINDOW, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU,
};

use crate::app::{self, Input};
use crate::backdrop;
use crate::layout::{self, SettingsHit, SettingsLayout};
use crate::render::{SettingsLook, SettingsScene, Target};
use crate::theme::Theme;
use crate::window::Shared;

pub(crate) const CLASS: PCWSTR = w!("HoradricSettings");
/// The windows crate files this under `Win32_UI_Controls`.
const WM_MOUSELEAVE: u32 = 0x02A3;

/// The window's sections, in the order they stand down the left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Section {
    #[default]
    Appearance,
    Notifications,
    Sessions,
    Startup,
    Privacy,
}

impl Section {
    pub const ALL: [Section; 5] = [
        Section::Appearance,
        Section::Notifications,
        Section::Sessions,
        Section::Startup,
        Section::Privacy,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Notifications => "Notifications",
            Section::Sessions => "Sessions",
            Section::Startup => "Startup and updates",
            Section::Privacy => "Privacy",
        }
    }
}

/// One setting the window shows, which is what a click on its row or a
/// pick from its list changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Theme,
    Font,
    Screen,
    Notify,
    Sounds,
    /// Whose session defaults the rows under it show.
    Agent,
    /// A default for the sessions Horadric starts, the same value the
    /// usage window's rows show.
    Default(Setting),
    /// Whether a project's menu offers to start the agent.
    Offered,
    Autostart,
    /// Look for a newer release now, or install the one found.
    Updates,
    Version,
    Discord,
}

/// What a row does when clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// Drops a list to pick from.
    List,
    /// Turns on or off, and shows which it is.
    Switch(bool),
    /// Does something once.
    Button,
    /// Only says, a click does nothing.
    Fixed,
}

/// A row as the window shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub field: Field,
    pub label: &'static str,
    pub value: String,
    pub control: Control,
}

/// The settings as the app has them now, which is all the window shows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Values {
    pub theme: Theme,
    pub font: String,
    /// The screens' names in the order offered, and the one the tiles
    /// stand on.
    pub screens: Vec<String>,
    pub screen: Option<usize>,
    pub notify: bool,
    pub sounds: bool,
    /// The agent the Sessions section shows, and its defaults.
    pub agent: Agent,
    pub defaults: Defaults,
    /// Whether its project menu line is on. None where it can not be
    /// turned off, Claude Code, and Some(None) where it is not installed.
    pub offered: Option<Option<bool>>,
    /// None where the switch is not offered, a dev instance.
    pub autostart: Option<bool>,
    pub discord: Discord,
    /// A newer release a check found.
    pub update: Option<String>,
    pub checking: bool,
    pub version: String,
}

/// The rows of `section`, given the settings in `v`.
pub fn lines(section: Section, v: &Values) -> Vec<Line> {
    let line = |field, label, value: &str, control| Line {
        field,
        label,
        value: value.to_string(),
        control,
    };
    let switch = |on: bool| (if on { "On" } else { "Off" }, Control::Switch(on));
    match section {
        Section::Appearance => {
            let screen = v.screen.and_then(|i| v.screens.get(i));
            let (name, control) = match (screen, v.screens.len()) {
                (Some(s), n) if n > 1 => (s.as_str(), Control::List),
                (Some(s), _) => (s.as_str(), Control::Fixed),
                (None, _) => ("Primary", Control::Fixed),
            };
            vec![
                line(Field::Theme, "Theme", v.theme.label(), Control::List),
                line(Field::Font, "Terminal font", &v.font, Control::List),
                line(Field::Screen, "Tiles on screen", name, control),
            ]
        }
        Section::Notifications => {
            let (notify, n) = switch(v.notify);
            let (sounds, s) = switch(v.sounds);
            vec![
                line(Field::Notify, "Notify when a session waits", notify, n),
                line(Field::Sounds, "Loot sounds", sounds, s),
            ]
        }
        Section::Sessions => {
            let mut rows = vec![line(Field::Agent, "Agent", v.agent.label(), Control::List)];
            rows.extend(v.agent.settings().iter().map(|s| {
                let value = s.name_of(v.agent, v.defaults.get(*s));
                line(Field::Default(*s), s.label(), value, Control::List)
            }));
            let offered = match v.offered {
                None => line(
                    Field::Offered,
                    "In the project menu",
                    "Always",
                    Control::Fixed,
                ),
                Some(None) => line(
                    Field::Offered,
                    "In the project menu",
                    "Not installed",
                    Control::Fixed,
                ),
                Some(Some(on)) => {
                    let (word, c) = switch(on);
                    line(Field::Offered, "In the project menu", word, c)
                }
            };
            rows.push(offered);
            rows
        }
        Section::Startup => {
            let autostart = match v.autostart {
                Some(on) => {
                    let (word, c) = switch(on);
                    line(Field::Autostart, "Start with Windows", word, c)
                }
                None => line(
                    Field::Autostart,
                    "Start with Windows",
                    "Not in a dev instance",
                    Control::Fixed,
                ),
            };
            let updates = match (&v.update, v.checking) {
                (Some(version), _) => format!("Update to {version}"),
                (None, true) => "Checking\u{2026}".to_string(),
                (None, false) => "Check now".to_string(),
            };
            let control = if v.checking && v.update.is_none() {
                Control::Fixed
            } else {
                Control::Button
            };
            vec![
                autostart,
                line(Field::Updates, "Updates", &updates, control),
                line(Field::Version, "Version", &v.version, Control::Fixed),
            ]
        }
        Section::Privacy => vec![line(
            Field::Discord,
            "Show on Discord",
            v.discord.label(),
            Control::List,
        )],
    }
}

/// The most rows any section has, which sets the window's height. The
/// Sessions section counts with whichever agent has the most, so picking
/// another does not leave the window a size its rows no longer fill.
fn tallest(v: &Values) -> usize {
    let sessions = Agent::ALL.iter().map(|a| {
        let v = Values {
            agent: *a,
            ..v.clone()
        };
        lines(Section::Sessions, &v).len()
    });
    Section::ALL
        .iter()
        .map(|s| lines(*s, v).len())
        .chain(sessions)
        .max()
        .unwrap_or(1)
}

/// The section `step` places after `at`, round at either end, for the
/// arrow keys.
fn step(at: Section, step: isize) -> Section {
    let n = Section::ALL.len() as isize;
    let i = Section::ALL.iter().position(|s| *s == at).unwrap_or(0) as isize;
    Section::ALL[(i + step).rem_euclid(n) as usize]
}

/// Brings the window back to the front, for a second "Settings..." while
/// it is open. Given its handle rather than the window, so the app need
/// not be borrowed while it takes the focus.
pub fn bring_back(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

pub fn register_class() -> Result<()> {
    unsafe {
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: GetModuleHandleW(None)?.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        Ok(())
    }
}

pub struct SettingsWindow {
    pub hwnd: HWND,
    shared: Rc<Shared>,
    target: RefCell<Option<Target>>,
    layout: RefCell<SettingsLayout>,
    values: RefCell<Values>,
    section: Cell<Section>,
    hot: Cell<SettingsHit>,
    pressed: Cell<Option<SettingsHit>>,
    tracking: Cell<bool>,
    /// The row whose list is dropped down, set by the app.
    open: Cell<Option<Field>>,
}

impl SettingsWindow {
    /// Opens the window in the middle of the screen the cursor is on,
    /// showing the settings in `values`, and gives it the keyboard.
    pub fn create(shared: Rc<Shared>, values: Values, icon: HICON) -> Result<Box<Self>> {
        let section = Section::default();
        let l = layout::settings(
            &shared.metrics,
            Section::ALL.len(),
            lines(section, &values).len(),
            tallest(&values),
        );
        let mut cursor = POINT::default();
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let (mut dpi, mut dy) = (96u32, 96u32);
        unsafe {
            let _ = GetCursorPos(&mut cursor);
            let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
            let _ = GetMonitorInfoW(monitor, &mut info);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dy);
        }
        // Born on the right screen and at its size: a window that moves
        // after it is made has been seen not to paint.
        let work = info.rcWork;
        let s = dpi.max(96) as f32 / 96.0;
        let (w, h) = ((l.size.0 * s).round() as i32, (l.size.1 * s).round() as i32);
        let x = work.left + (work.right - work.left - w) / 2;
        let y = work.top + (work.bottom - work.top - h) / 2;
        let mut win = Box::new(SettingsWindow {
            hwnd: HWND::default(),
            shared,
            target: RefCell::new(None),
            layout: RefCell::new(l),
            values: RefCell::new(values),
            section: Cell::new(section),
            hot: Cell::new(SettingsHit::Nothing),
            pressed: Cell::new(None),
            tracking: Cell::new(false),
            open: Cell::new(None),
        });
        unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_APPWINDOW,
                CLASS,
                w!("Horadric settings"),
                WS_POPUP | WS_SYSMENU | WS_MINIMIZEBOX,
                x,
                y,
                w,
                h,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                Some(&*win as *const SettingsWindow as *const c_void),
            )?;
            win.hwnd = hwnd;
            let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const c_void,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            backdrop::border(hwnd, None);
            for size in [ICON_BIG, ICON_SMALL] {
                SendMessageW(
                    hwnd,
                    WM_SETICON,
                    Some(WPARAM(size as usize)),
                    Some(LPARAM(icon.0 as isize)),
                );
            }
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
        Ok(win)
    }

    pub fn destroy(&self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    /// Lights the row whose list is dropped down, None once it closes.
    pub fn set_open(&self, field: Option<Field>) {
        if self.open.replace(field) != field {
            self.invalidate();
        }
    }

    /// Takes the settings as they are now and draws them.
    pub fn set_values(&self, values: Values) {
        if *self.values.borrow() == values {
            return;
        }
        *self.values.borrow_mut() = values;
        self.relayout();
    }

    pub fn dpi(&self) -> u32 {
        unsafe { GetDpiForWindow(self.hwnd) }.max(96)
    }

    fn scale(&self) -> f32 {
        self.dpi() as f32 / 96.0
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn lines(&self) -> Vec<Line> {
        lines(self.section.get(), &self.values.borrow())
    }

    /// Lays out again for the section shown. The window keeps its size,
    /// which is the tallest section's.
    fn relayout(&self) {
        let v = self.values.borrow();
        let l = layout::settings(
            &self.shared.metrics,
            Section::ALL.len(),
            lines(self.section.get(), &v).len(),
            tallest(&v),
        );
        drop(v);
        *self.layout.borrow_mut() = l;
        self.refresh_hover();
        self.invalidate();
    }

    fn pick_section(&self, section: Section) {
        if self.section.replace(section) != section {
            self.relayout();
        }
    }

    fn paint(&self) {
        let l = self.layout.borrow();
        let s = self.scale();
        let (w, h) = ((l.size.0 * s).round() as u32, (l.size.1 * s).round() as u32);
        let mut slot = self.target.borrow_mut();
        if slot.is_none() {
            match Target::new(&self.shared.gpu, self.hwnd, w, h, self.dpi()) {
                Ok(t) => *slot = Some(t),
                Err(e) => {
                    eprintln!("horadric: render target for the Settings window: {e}");
                    return;
                }
            }
        }
        let open = self.open.get();
        let rows: Vec<SettingsLook> = self
            .lines()
            .into_iter()
            .map(|line| SettingsLook {
                open: open == Some(line.field),
                label: line.label,
                value: line.value,
                control: line.control,
            })
            .collect();
        let scene = SettingsScene {
            layout: &l,
            sections: &Section::ALL.map(Section::label),
            section: Section::ALL
                .iter()
                .position(|x| *x == self.section.get())
                .unwrap_or(0),
            heading: self.section.get().label(),
            rows: &rows,
            hot: self.hot.get(),
            pressed: self.pressed.get(),
        };
        let result = slot
            .as_ref()
            .map(|t| t.draw_settings(&self.shared.gpu, &self.shared.metrics, &scene));
        if let Some(Err(_)) = result {
            *slot = None;
        }
    }

    fn hit_at(&self, x: i32, y: i32) -> SettingsHit {
        let s = self.scale();
        layout::settings_hit(&self.layout.borrow(), x as f32 / s, y as f32 / s)
    }

    fn hit(&self, lparam: LPARAM) -> SettingsHit {
        let x = (lparam.0 & 0xffff) as i16 as i32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
        self.hit_at(x, y)
    }

    fn hover(&self, hot: SettingsHit) {
        if self.hot.replace(hot) != hot {
            self.invalidate();
        }
    }

    fn press(&self, pressed: Option<SettingsHit>) {
        if self.pressed.replace(pressed) != pressed {
            self.invalidate();
        }
    }

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

    fn refresh_hover(&self) {
        if !self.tracking.get() {
            return;
        }
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
            let _ = ScreenToClient(self.hwnd, &mut p);
        }
        self.hover(self.hit_at(p.x, p.y));
    }

    fn click(&self, hit: SettingsHit) {
        match hit {
            SettingsHit::Close => app::push(Input::SettingsClosed),
            SettingsHit::Section(i) => {
                if let Some(s) = Section::ALL.get(i) {
                    self.pick_section(*s);
                }
            }
            SettingsHit::Row(i) => {
                let Some(line) = self.lines().into_iter().nth(i) else {
                    return;
                };
                if line.control == Control::Fixed {
                    return;
                }
                if let Some(row) = self.row_on_screen(i) {
                    app::push(Input::SettingsClick(line.field, row));
                }
            }
            SettingsHit::Title | SettingsHit::Nothing => {}
        }
    }

    /// Row `i` in screen pixels, for its list to drop from.
    fn row_on_screen(&self, i: usize) -> Option<RECT> {
        let r = self.layout.borrow().rows.get(i)?.rect;
        let mut w = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut w);
        }
        let s = self.scale();
        let px = |v: f32| (v * s).round() as i32;
        Some(RECT {
            left: w.left + px(r.x),
            top: w.top + px(r.y),
            right: w.left + px(r.right()),
            bottom: w.top + px(r.bottom()),
        })
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
            // The title bar moves the window the way a caption does, snap
            // and all, with nothing of ours to get wrong.
            WM_NCHITTEST => {
                let mut p = POINT {
                    x: (lparam.0 & 0xffff) as i16 as i32,
                    y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
                };
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut p);
                }
                let at = if self.hit_at(p.x, p.y) == SettingsHit::Title {
                    HTCAPTION
                } else {
                    HTCLIENT
                };
                Some(LRESULT(at as isize))
            }
            WM_CLOSE => {
                app::push(Input::SettingsClosed);
                Some(LRESULT(0))
            }
            WM_SIZE => {
                let w = (lparam.0 & 0xffff) as u32;
                let h = ((lparam.0 >> 16) & 0xffff) as u32;
                if let Some(t) = self.target.borrow().as_ref() {
                    let _ = t.resize(w, h);
                }
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                let dpi = (wparam.0 & 0xffff) as u32;
                if let Some(t) = self.target.borrow().as_ref() {
                    t.set_dpi(dpi);
                }
                let r = unsafe { *(lparam.0 as *const RECT) };
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOACTIVATE | SWP_NOZORDER,
                    );
                }
                Some(LRESULT(0))
            }
            WM_KEYDOWN => {
                match wparam.0 as u16 {
                    k if k == VK_ESCAPE.0 => app::push(Input::SettingsClosed),
                    k if k == VK_UP.0 => self.pick_section(step(self.section.get(), -1)),
                    k if k == VK_DOWN.0 => self.pick_section(step(self.section.get(), 1)),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                unsafe {
                    SetCapture(self.hwnd);
                }
                self.press(Some(self.hit(lparam)));
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                self.track();
                self.hover(self.hit(lparam));
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                // Before letting go: releasing capture sends
                // WM_CAPTURECHANGED at once, and that clears the press.
                let pressed = self.pressed.get();
                unsafe {
                    let _ = ReleaseCapture();
                }
                self.press(None);
                let hit = self.hit(lparam);
                // Only where the press began, as a button does.
                if pressed == Some(hit) {
                    self.click(hit);
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking.set(false);
                self.hover(SettingsHit::Nothing);
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                self.press(None);
                None
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
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const SettingsWindow;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    // The Box lives in the app for as long as the window exists, and the app
    // destroys the window before dropping the Box.
    let win = &*ptr;
    match win.handle(msg, wparam, lparam) {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> Values {
        Values {
            theme: Theme::default(),
            font: "Cascadia Mono".into(),
            screens: vec!["Screen 1".into(), "Screen 2".into()],
            screen: Some(1),
            notify: true,
            sounds: false,
            agent: Agent::Claude,
            defaults: Defaults {
                effort: Some("xhigh".into()),
                ..Default::default()
            },
            offered: None,
            autostart: Some(true),
            discord: Discord::Unnamed,
            update: None,
            checking: false,
            version: "0.9.0".into(),
        }
    }

    fn fields(section: Section, v: &Values) -> Vec<Field> {
        lines(section, v).iter().map(|l| l.field).collect()
    }

    #[test]
    fn each_section_has_its_rows() {
        let v = values();
        assert_eq!(
            fields(Section::Appearance, &v),
            [Field::Theme, Field::Font, Field::Screen]
        );
        assert_eq!(
            fields(Section::Notifications, &v),
            [Field::Notify, Field::Sounds]
        );
        assert_eq!(
            fields(Section::Startup, &v),
            [Field::Autostart, Field::Updates, Field::Version]
        );
        assert_eq!(
            fields(Section::Sessions, &v),
            [
                Field::Agent,
                Field::Default(Setting::Model),
                Field::Default(Setting::Effort),
                Field::Default(Setting::Permissions),
                Field::Offered,
            ]
        );
        assert_eq!(fields(Section::Privacy, &v), [Field::Discord]);
        assert_eq!(tallest(&v), 5);
    }

    #[test]
    fn a_row_says_what_its_setting_is_now() {
        let v = values();
        let look = lines(Section::Appearance, &v);
        assert_eq!(look[0].value, Theme::default().label());
        assert_eq!(look[1].value, "Cascadia Mono");
        assert_eq!(look[2].value, "Screen 2");
        let n = lines(Section::Notifications, &v);
        assert_eq!(n[0].control, Control::Switch(true));
        assert_eq!(n[0].value, "On");
        assert_eq!(n[1].control, Control::Switch(false));
        assert_eq!(
            lines(Section::Privacy, &v)[0].value,
            "Without project names"
        );
    }

    #[test]
    fn the_sessions_rows_show_the_picked_agents_defaults() {
        let mut v = values();
        let rows = lines(Section::Sessions, &v);
        let values: Vec<&str> = rows.iter().map(|l| l.value.as_str()).collect();
        assert_eq!(
            values,
            ["Claude Code", "Default", "Extra high", "Default", "Always"]
        );
        assert_eq!(rows[4].control, Control::Fixed);
        v.agent = Agent::Codex;
        v.defaults = Defaults {
            model: Some("gpt-5.5".into()),
            ..Default::default()
        };
        v.offered = Some(Some(false));
        let rows = lines(Section::Sessions, &v);
        assert_eq!(
            rows.iter().map(|l| l.field).collect::<Vec<_>>(),
            [
                Field::Agent,
                Field::Default(Setting::Model),
                Field::Default(Setting::Effort),
                Field::Offered,
            ]
        );
        assert_eq!(rows[1].value, "GPT-5.5");
        assert_eq!(tallest(&v), 5);
        assert_eq!(rows[3].control, Control::Switch(false));
        v.offered = Some(None);
        let rows = lines(Section::Sessions, &v);
        assert_eq!(
            (rows[3].value.as_str(), rows[3].control),
            ("Not installed", Control::Fixed)
        );
    }

    #[test]
    fn one_screen_is_said_not_offered() {
        let mut v = values();
        assert_eq!(lines(Section::Appearance, &v)[2].control, Control::List);
        v.screens.truncate(1);
        v.screen = Some(0);
        let row = &lines(Section::Appearance, &v)[2];
        assert_eq!(
            (row.value.as_str(), row.control),
            ("Screen 1", Control::Fixed)
        );
    }

    #[test]
    fn the_updates_row_checks_then_offers_what_it_found() {
        let mut v = values();
        let row = |v: &Values| lines(Section::Startup, v)[1].clone();
        assert_eq!(row(&v).value, "Check now");
        assert_eq!(row(&v).control, Control::Button);
        v.checking = true;
        assert_eq!(row(&v).control, Control::Fixed);
        v.update = Some("1.0.0".into());
        assert_eq!(row(&v).value, "Update to 1.0.0");
        assert_eq!(row(&v).control, Control::Button);
    }

    #[test]
    fn a_dev_instance_does_not_offer_start_with_windows() {
        let mut v = values();
        v.autostart = None;
        assert_eq!(lines(Section::Startup, &v)[0].control, Control::Fixed);
    }

    #[test]
    fn the_arrow_keys_go_round_the_sections() {
        assert_eq!(step(Section::Appearance, 1), Section::Notifications);
        assert_eq!(step(Section::Notifications, 1), Section::Sessions);
        assert_eq!(step(Section::Appearance, -1), Section::Privacy);
        assert_eq!(step(Section::Privacy, 1), Section::Appearance);
    }
}
