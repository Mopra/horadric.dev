//! The notification area icon: the one piece of Glance that is always on
//! screen, even with no sessions and so no tiles. Its menu starts sessions
//! and quits the app.

use std::ffi::c_void;
use std::sync::Once;

use windows::core::{w, HSTRING, PCSTR};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos,
    GetSystemMetrics, PostMessageW, SetForegroundWindow, TrackPopupMenu, HICON, ICONINFO,
    MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, SM_CXSMICON, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    WM_NULL,
};

use crate::{icon, recent};

const ID: u32 = 1;

pub struct Tray {
    hwnd: HWND,
    callback: u32,
    icon: HICON,
    tip: String,
}

/// What the user picked from the menu.
pub enum Choice {
    /// Start a session in a folder chosen with the picker.
    New,
    /// Start a session in this recent project.
    Recent(String),
    /// Put every cluster back in the auto layout.
    Tidy,
    /// Bring every cluster in front of the other windows.
    Raise,
    /// Show the session that has waited on you longest.
    NextWaiting,
    /// Fill the space beside the clusters with the terminal.
    Arrange,
    ToggleAutostart,
    /// End every session in every project, after asking.
    EndAll,
    Quit,
}

impl Tray {
    /// Adds the icon. Clicks arrive at `hwnd` as `callback` messages.
    pub fn add(hwnd: HWND, callback: u32) -> Tray {
        let tray = Tray {
            hwnd,
            callback,
            icon: make_icon(),
            tip: "Glance".into(),
        };
        tray.show();
        tray
    }

    /// Adds the icon again. Needed after Explorer restarts, which forgets
    /// every icon and says so with a `TaskbarCreated` broadcast.
    pub fn show(&self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &self.data());
        }
    }

    pub fn set_tip(&mut self, tip: &str) {
        if self.tip == tip {
            return;
        }
        self.tip = tip.to_string();
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &self.data());
        }
    }

    fn data(&self) -> NOTIFYICONDATAW {
        let mut d = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: ID,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: self.callback,
            hIcon: self.icon,
            ..Default::default()
        };
        for (slot, unit) in d.szTip.iter_mut().zip(self.tip.encode_utf16().take(127)) {
            *slot = unit;
        }
        d
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data());
            let _ = DestroyIcon(self.icon);
        }
    }
}

/// One line of a popup menu.
pub enum Item {
    Action {
        id: usize,
        label: String,
        checked: bool,
    },
    Disabled(String),
    Separator,
}

impl Item {
    pub fn action(id: usize, label: impl Into<String>) -> Item {
        Item::Action {
            id,
            label: label.into(),
            checked: false,
        }
    }
}

/// Shows a menu at the cursor and returns the id picked. Runs a modal loop,
/// so the caller must not hold anything the message handlers need.
pub fn popup(hwnd: HWND, items: &[Item]) -> Option<usize> {
    static DARK: Once = Once::new();
    DARK.call_once(dark_menus);
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        for item in items {
            let _ = match item {
                Item::Action { id, label, checked } => {
                    let flags = if *checked {
                        MF_STRING | MF_CHECKED
                    } else {
                        MF_STRING
                    };
                    AppendMenuW(menu, flags, *id, &HSTRING::from(label.as_str()))
                }
                Item::Disabled(label) => AppendMenuW(
                    menu,
                    MF_STRING | MF_GRAYED,
                    0,
                    &HSTRING::from(label.as_str()),
                ),
                Item::Separator => AppendMenuW(menu, MF_SEPARATOR, 0, None),
            };
        }

        let mut at = POINT::default();
        let _ = GetCursorPos(&mut at);
        // Without the foreground the menu never closes on a click elsewhere,
        // and without the WM_NULL after, it closes on the next one. Both are
        // documented Windows behaviour.
        let _ = SetForegroundWindow(hwnd);
        let picked = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            at.x,
            at.y,
            None,
            hwnd,
            None,
        )
        .0 as usize;
        let _ = PostMessageW(Some(hwnd), WM_NULL, Default::default(), Default::default());
        let _ = DestroyMenu(menu);
        (picked != 0).then_some(picked)
    }
}

/// Draws every popup menu of the process dark, the way Explorer's are, to
/// match the tiles. Windows only offers this through uxtheme exports that
/// have no name, just an ordinal, stable since Windows 10 1903. Where they
/// are missing the menus stay light, which is fine.
fn dark_menus() {
    const SET_PREFERRED_APP_MODE: usize = 135;
    const FLUSH_MENU_THEMES: usize = 136;
    const FORCE_DARK: i32 = 2;
    unsafe {
        let Ok(uxtheme) = LoadLibraryW(w!("uxtheme.dll")) else {
            return;
        };
        let ordinal = |n: usize| GetProcAddress(uxtheme, PCSTR(n as *const u8));
        let (Some(set_mode), Some(flush)) =
            (ordinal(SET_PREFERRED_APP_MODE), ordinal(FLUSH_MENU_THEMES))
        else {
            return;
        };
        let set_mode: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(set_mode);
        let flush: unsafe extern "system" fn() = std::mem::transmute(flush);
        set_mode(FORCE_DARK);
        flush();
    }
}

/// The tray menu. `autostart` is None when the switch is not offered,
/// `hotkey` is the shortcut for the next waiting session, if it has one.
pub fn menu(
    hwnd: HWND,
    recent_projects: &[String],
    autostart: Option<bool>,
    hotkey: Option<&str>,
) -> Option<Choice> {
    const NEW: usize = 1;
    const QUIT: usize = 2;
    const AUTOSTART: usize = 3;
    const TIDY: usize = 4;
    const NEXT: usize = 5;
    const ARRANGE: usize = 6;
    const RAISE: usize = 7;
    const END_ALL: usize = 8;
    const RECENT: usize = 100;
    let mut items = vec![Item::action(NEW, "New session\u{2026}"), Item::Separator];
    if recent_projects.is_empty() {
        items.push(Item::Disabled("No recent projects".into()));
    }
    for (i, path) in recent_projects.iter().enumerate() {
        let (name, place) = recent::label(path);
        // A tab right aligns the rest; an ampersand would underline.
        let text = format!("{}\t{}", name.replace('&', "&&"), place.replace('&', "&&"));
        items.push(Item::action(RECENT + i, text));
    }
    items.push(Item::Separator);
    let next = match hotkey {
        Some(key) => format!("Next waiting session	{key}"),
        None => "Next waiting session".into(),
    };
    items.push(Item::action(NEXT, next));
    items.push(Item::action(RAISE, "Bring tiles to front"));
    items.push(Item::action(ARRANGE, "Fit terminal beside tiles"));
    items.push(Item::action(TIDY, "Tidy up tiles"));
    if let Some(checked) = autostart {
        items.push(Item::Action {
            id: AUTOSTART,
            label: "Start with Windows".into(),
            checked,
        });
    }
    items.push(Item::Separator);
    items.push(Item::action(END_ALL, "End all sessions"));
    items.push(Item::action(QUIT, "Quit Glance"));

    match popup(hwnd, &items)? {
        NEW => Some(Choice::New),
        QUIT => Some(Choice::Quit),
        AUTOSTART => Some(Choice::ToggleAutostart),
        TIDY => Some(Choice::Tidy),
        NEXT => Some(Choice::NextWaiting),
        ARRANGE => Some(Choice::Arrange),
        RAISE => Some(Choice::Raise),
        END_ALL => Some(Choice::EndAll),
        i if i >= RECENT => recent_projects
            .get(i - RECENT)
            .map(|p| Choice::Recent(p.clone())),
        _ => None,
    }
}

/// Turns the drawn pixels into an icon at the small icon size for this DPI.
fn make_icon() -> HICON {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
    let pixels = if glance_hooks::dev() {
        icon::dev_pixels(size as u32)
    } else {
        icon::pixels(size as u32)
    };
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                // Negative: rows run top down, as the pixels do.
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let Ok(color) = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
            return HICON::default();
        };
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u32, pixels.len());
        // An all zero mask: the alpha channel decides what shows.
        let mask_bits = vec![0u8; (size as usize).div_ceil(16) * 2 * size as usize];
        let mask = CreateBitmap(size, size, 1, 1, Some(mask_bits.as_ptr() as *const c_void));
        let icon = CreateIconIndirect(&ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        })
        .unwrap_or_default();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}
