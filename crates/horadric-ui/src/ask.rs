//! A one line text question, for naming a session.
//!
//! A plain Win32 dialog built from a template in memory, so no resource
//! file ships and no toolkit is needed: a line of text, an edit box, OK and
//! Cancel. Enter and Esc work as in any dialog. Like the menus it runs a
//! modal loop, so the caller must not hold anything the message handlers
//! need.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    DialogBoxIndirectParamW, EndDialog, GetDlgItem, GetDlgItemTextW, GetWindowLongPtrW,
    SendMessageW, SetDlgItemTextW, SetWindowLongPtrW, DLGTEMPLATE, GWLP_USERDATA, IDCANCEL, IDOK,
    WM_COMMAND, WM_INITDIALOG,
};

// Window and control styles, as the template spells them.
const WS_POPUP: u32 = 0x8000_0000;
const WS_CHILD: u32 = 0x4000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_CAPTION: u32 = 0x00C0_0000;
const WS_BORDER: u32 = 0x0080_0000;
const WS_SYSMENU: u32 = 0x0008_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const DS_SETFONT: u32 = 0x40;
const DS_MODALFRAME: u32 = 0x80;
const DS_SETFOREGROUND: u32 = 0x200;
const DS_CENTER: u32 = 0x800;
const ES_AUTOHSCROLL: u32 = 0x80;
const BS_DEFPUSHBUTTON: u32 = 0x1;
/// The predefined control classes, by atom.
const BUTTON: u16 = 0x80;
const EDIT: u16 = 0x81;
const STATIC: u16 = 0x82;
const EM_SETSEL: u32 = 0xB1;
const EDIT_ID: i32 = 100;
/// Longer than any name worth reading on a tile.
const MAX_LEN: usize = 200;

/// One control in a dialog template, in dialog units.
struct Control<'a> {
    class: u16,
    id: u16,
    style: u32,
    at: [i16; 4],
    text: &'a str,
}

/// A dialog template: a header, then each control, every control aligned
/// to four bytes. The layout Windows documents for `DLGTEMPLATE` and
/// `DLGITEMTEMPLATE`, as 16 bit units.
fn template(title: &str, prompt: &str) -> Vec<u16> {
    fn dword(out: &mut Vec<u16>, v: u32) {
        out.push(v as u16);
        out.push((v >> 16) as u16);
    }
    fn text(out: &mut Vec<u16>, s: &str) {
        out.extend(s.encode_utf16());
        out.push(0);
    }
    let controls = [
        Control {
            class: STATIC,
            id: 0xFFFF,
            style: WS_CHILD | WS_VISIBLE,
            at: [7, 7, 226, 10],
            text: prompt,
        },
        Control {
            class: EDIT,
            id: EDIT_ID as u16,
            style: WS_CHILD | WS_VISIBLE | WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL,
            at: [7, 20, 226, 13],
            text: "",
        },
        Control {
            class: BUTTON,
            id: IDOK.0 as u16,
            style: WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON,
            at: [129, 41, 50, 14],
            text: "OK",
        },
        Control {
            class: BUTTON,
            id: IDCANCEL.0 as u16,
            style: WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            at: [183, 41, 50, 14],
            text: "Cancel",
        },
    ];
    let mut out = Vec::new();
    let style = WS_POPUP
        | WS_CAPTION
        | WS_SYSMENU
        | DS_MODALFRAME
        | DS_SETFONT
        | DS_CENTER
        | DS_SETFOREGROUND;
    dword(&mut out, style);
    dword(&mut out, 0);
    out.push(controls.len() as u16);
    out.extend([0, 0, 240, 62]);
    // No menu, the standard class, then the title and the font.
    out.extend([0, 0]);
    text(&mut out, title);
    out.push(9);
    text(&mut out, "Segoe UI");
    for c in &controls {
        if out.len() % 2 == 1 {
            out.push(0);
        }
        dword(&mut out, c.style);
        dword(&mut out, 0);
        out.extend(c.at.map(|v| v as u16));
        out.push(c.id);
        out.extend([0xFFFF, c.class]);
        text(&mut out, c.text);
        // No creation data.
        out.push(0);
    }
    out
}

/// What the dialog procedure reads and writes, through `GWLP_USERDATA`.
struct State {
    initial: Vec<u16>,
    answer: Option<String>,
}

/// Asks for a line of text, `initial` filled in and selected. None when
/// cancelled.
pub fn text(owner: HWND, title: &str, prompt: &str, initial: &str) -> Option<String> {
    // The template has to start on a four byte boundary, which a buffer of
    // 16 bit units does not promise. Little endian: the first unit is low.
    let dialog: Vec<u32> = template(title, prompt)
        .chunks(2)
        .map(|c| c[0] as u32 | (c.get(1).copied().unwrap_or(0) as u32) << 16)
        .collect();
    let mut state = State {
        initial: initial.encode_utf16().chain([0]).collect(),
        answer: None,
    };
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        DialogBoxIndirectParamW(
            Some(instance.into()),
            dialog.as_ptr() as *const DLGTEMPLATE,
            Some(owner),
            Some(proc),
            LPARAM(&mut state as *mut State as isize),
        );
    }
    state.answer
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0);
            let state = &*(lparam.0 as *const State);
            let _ = SetDlgItemTextW(hwnd, EDIT_ID, PCWSTR(state.initial.as_ptr()));
            if let Ok(edit) = GetDlgItem(Some(hwnd), EDIT_ID) {
                SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
                let _ = SetFocus(Some(edit));
            }
            // The focus is set: Windows must not move it.
            0
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
            if id == IDOK.0 {
                let mut buf = [0u16; MAX_LEN + 1];
                let n = GetDlgItemTextW(hwnd, EDIT_ID, &mut buf) as usize;
                if let Some(state) = state.as_mut() {
                    state.answer = Some(String::from_utf16_lossy(&buf[..n]));
                }
                let _ = EndDialog(hwnd, 1);
                1
            } else if id == IDCANCEL.0 {
                let _ = EndDialog(hwnd, 0);
                1
            } else {
                0
            }
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_has_its_header_and_aligned_controls() {
        let t = template("Rename", "Name");
        let style = t[0] as u32 | (t[1] as u32) << 16;
        assert_ne!(style & DS_SETFONT, 0);
        assert_eq!(t[4], 4, "four controls");
        assert_eq!(&t[5..9], &[0, 0, 240, 62]);
        // Menu and class are none, then the title ends in a zero.
        assert_eq!(&t[9..11], &[0, 0]);
        let title: Vec<u16> = "Rename".encode_utf16().chain([0]).collect();
        assert_eq!(&t[11..11 + title.len()], &title[..]);
        // Every control starts on a four byte boundary, with its class
        // given by atom.
        let classes: Vec<usize> = t
            .windows(2)
            .enumerate()
            .filter(|(_, w)| w[0] == 0xFFFF && [BUTTON, EDIT, STATIC].contains(&w[1]))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(classes.len(), 4);
        for i in classes {
            // Style, extended style, four places and the id come first.
            assert_eq!((i - 9) % 2, 0, "control at {i} is not aligned");
        }
    }
}
