//! What a key press sends to the program, in xterm's encoding.
//!
//! Windows splits typing in two. Keys that make a character (letters, digits,
//! Enter, Tab, Backspace, dead key compositions, AltGr symbols on a Danish
//! keyboard) arrive as `WM_CHAR` after the layout has done its work. Keys
//! that do not (arrows, Home, F5) only arrive as `WM_KEYDOWN`. This module
//! has one function for each half and no Win32 in it.
//!
//! A few chords belong to the stage rather than the program: zoom a pane,
//! move to the next one, the font size and search ([`chord`]).

use crate::layout::Dir;

/// A key that produces no character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// F1 to F12.
    F(u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        shift: false,
        ctrl: false,
        alt: false,
    };

    /// xterm's modifier parameter: 1 plus a bit per modifier.
    fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }

    fn any(self) -> bool {
        self.shift || self.ctrl || self.alt
    }
}

/// The bytes for a non character key. `app_cursor` is DECCKM, which switches
/// unmodified arrows to their SS3 form.
pub fn key_bytes(key: Key, mods: Mods, app_cursor: bool) -> Vec<u8> {
    // Keys of the form CSI 1 ; m X, or SS3 X / CSI X without modifiers.
    let letter = match key {
        Key::Up => Some(b'A'),
        Key::Down => Some(b'B'),
        Key::Right => Some(b'C'),
        Key::Left => Some(b'D'),
        Key::Home => Some(b'H'),
        Key::End => Some(b'F'),
        Key::F(1) => Some(b'P'),
        Key::F(2) => Some(b'Q'),
        Key::F(3) => Some(b'R'),
        Key::F(4) => Some(b'S'),
        _ => None,
    };
    if let Some(l) = letter {
        let is_f = matches!(key, Key::F(_));
        return if mods.any() {
            format!("\x1b[1;{}{}", mods.param(), l as char).into_bytes()
        } else if is_f || app_cursor {
            vec![0x1b, b'O', l]
        } else {
            vec![0x1b, b'[', l]
        };
    }

    // Keys of the form CSI n ~ or CSI n ; m ~.
    let n = match key {
        Key::Insert => 2,
        Key::Delete => 3,
        Key::PageUp => 5,
        Key::PageDown => 6,
        Key::F(5) => 15,
        Key::F(6) => 17,
        Key::F(7) => 18,
        Key::F(8) => 19,
        Key::F(9) => 20,
        Key::F(10) => 21,
        Key::F(11) => 23,
        Key::F(12) => 24,
        _ => return Vec::new(),
    };
    if mods.any() {
        format!("\x1b[{n};{}~", mods.param()).into_bytes()
    } else {
        format!("\x1b[{n}~").into_bytes()
    }
}

/// What a typed character means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CharAction {
    Send(Vec<u8>),
    /// Ctrl+V, with or without Shift.
    Paste,
    /// Ctrl+C: copy when there is a selection, otherwise interrupt.
    CopyOrInterrupt,
    /// Ctrl+Shift+C: always copy, never interrupt.
    Copy,
    /// Ctrl+Shift+T: a new plain terminal in the project, as a new tab is
    /// in Windows Terminal. Plain Ctrl+T still reaches the program.
    NewShell,
}

/// Maps a `WM_CHAR` (or `WM_SYSCHAR` when `mods.alt`) to its bytes.
pub fn char_action(c: char, mods: Mods) -> CharAction {
    let mut out = Vec::new();
    match c {
        '\u{16}' => return CharAction::Paste,
        '\u{3}' if mods.shift => return CharAction::Copy,
        '\u{3}' => return CharAction::CopyOrInterrupt,
        '\u{14}' if mods.shift && mods.ctrl => return CharAction::NewShell,
        // Meta+Enter is the newline Claude Code understands everywhere.
        '\r' if mods.shift => out.extend_from_slice(b"\x1b\r"),
        '\t' if mods.shift => out.extend_from_slice(b"\x1b[Z"),
        // Windows reports Backspace as ^H and Ctrl+Backspace as DEL. Every
        // terminal program expects the reverse.
        '\u{8}' => out.push(0x7f),
        '\u{7f}' => out.push(0x08),
        _ => {
            if mods.alt {
                out.push(0x1b);
            }
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    CharAction::Send(out)
}

/// Font size in DIPs when nothing else was chosen. 15 DIPs is 11.25
/// points at 100 % scaling.
pub const FONT_DEFAULT: f32 = 15.0;
const FONT_MIN: f32 = 9.0;
const FONT_MAX: f32 = 32.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontStep {
    Bigger,
    Smaller,
    Reset,
}

/// The font size after a step, one DIP at a time and within reason.
pub fn font_size(current: f32, step: FontStep) -> f32 {
    let next = match step {
        FontStep::Bigger => current.round() + 1.0,
        FontStep::Smaller => current.round() - 1.0,
        FontStep::Reset => FONT_DEFAULT,
    };
    next.clamp(FONT_MIN, FONT_MAX)
}

/// A chord the stage keeps for itself rather than send to the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chord {
    /// Ctrl+Shift+Enter: this pane alone fills the stage, or the grid
    /// comes back.
    Zoom,
    /// Ctrl+Alt+arrow: the keyboard to the pane in that direction.
    Focus(Dir),
    /// Ctrl and plus, minus or zero, as in a browser.
    Font(FontStep),
    /// Ctrl+Shift+F: search the history, as in Windows Terminal.
    Find,
}

// Virtual key codes, which Windows has kept the same since forever.
const VK_RETURN: u16 = 0x0D;
const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;
const VK_0: u16 = 0x30;
const VK_F: u16 = 0x46;
const VK_NUMPAD0: u16 = 0x60;
const VK_ADD: u16 = 0x6B;
const VK_SUBTRACT: u16 = 0x6D;
/// The key right of 0 on a Danish keyboard, and `=` on a US one.
const VK_OEM_PLUS: u16 = 0xBB;
const VK_OEM_MINUS: u16 = 0xBD;

/// Maps a `WM_KEYDOWN` to a stage chord. AltGr is Ctrl+Alt, so no chord
/// that takes Ctrl alone also takes Alt.
pub fn chord(vk: u16, mods: Mods) -> Option<Chord> {
    let Mods { shift, ctrl, alt } = mods;
    if !ctrl {
        return None;
    }
    let chord = match vk {
        VK_RETURN if shift && !alt => Chord::Zoom,
        VK_F if shift && !alt => Chord::Find,
        VK_LEFT if alt && !shift => Chord::Focus(Dir::Left),
        VK_RIGHT if alt && !shift => Chord::Focus(Dir::Right),
        VK_UP if alt && !shift => Chord::Focus(Dir::Up),
        VK_DOWN if alt && !shift => Chord::Focus(Dir::Down),
        // Shift too, since `+` is Shift+`=` on a US keyboard.
        VK_OEM_PLUS | VK_ADD if !alt => Chord::Font(FontStep::Bigger),
        VK_OEM_MINUS | VK_SUBTRACT if !alt && !shift => Chord::Font(FontStep::Smaller),
        VK_0 | VK_NUMPAD0 if !alt && !shift => Chord::Font(FontStep::Reset),
        _ => return None,
    };
    Some(chord)
}

/// Clipboard text as the program should receive it. Line ends become a lone
/// CR, the way a typed Enter arrives. With bracketed paste on, the text is
/// wrapped so the program knows it was pasted, and escape characters are
/// dropped so the text can not end the bracket early and type commands.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let normalised = text.replace("\r\n", "\r").replace('\n', "\r");
    let mut out = Vec::with_capacity(normalised.len() + 12);
    if bracketed {
        out.extend_from_slice(b"\x1b[200~");
        out.extend(normalised.bytes().filter(|&b| b != 0x1b));
        out.extend_from_slice(b"\x1b[201~");
    } else {
        out.extend_from_slice(normalised.as_bytes());
    }
    out
}

/// A mouse button as programs number them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

impl Button {
    fn code(self) -> u32 {
        match self {
            Button::Left => 0,
            Button::Middle => 1,
            Button::Right => 2,
            Button::WheelUp => 64,
            Button::WheelDown => 65,
        }
    }
}

/// What the mouse did, for a program that asked for the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseEvent {
    Press(Button),
    Release(Button),
    /// A move, with the button held down if any.
    Move(Option<Button>),
}

/// How a report is written. `Sgr` is mode 1006 and has no limit on the
/// cell. `Utf8` is mode 1005 and reaches cell 2015. `X10` is the old one
/// byte form, which can only reach cell 223.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseEncoding {
    X10,
    Utf8,
    Sgr,
}

/// A mouse report. `col` and `row` count cells from the top left, from 0.
pub fn mouse_bytes(
    event: MouseEvent,
    col: usize,
    row: usize,
    mods: Mods,
    encoding: MouseEncoding,
) -> Vec<u8> {
    let held = |b: Option<Button>| b.map_or(3, Button::code);
    let code = match event {
        MouseEvent::Press(b) => b.code(),
        // Only SGR can say which button came up; the older forms send 3.
        MouseEvent::Release(b) if encoding == MouseEncoding::Sgr => b.code(),
        MouseEvent::Release(_) => 3,
        MouseEvent::Move(b) => 32 + held(b),
    } + 4 * mods.shift as u32
        + 8 * mods.alt as u32
        + 16 * mods.ctrl as u32;
    match encoding {
        MouseEncoding::Sgr => {
            let end = if matches!(event, MouseEvent::Release(_)) {
                'm'
            } else {
                'M'
            };
            format!("\x1b[<{code};{};{}{end}", col + 1, row + 1).into_bytes()
        }
        MouseEncoding::Utf8 => {
            let mut out = b"\x1b[M".to_vec();
            for n in [
                32 + code,
                33 + col.min(2014) as u32,
                33 + row.min(2014) as u32,
            ] {
                let c = char::from_u32(n).unwrap_or(' ');
                out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
            }
            out
        }
        MouseEncoding::X10 => {
            let at = |n: usize| (33 + n).min(255) as u8;
            vec![
                0x1b,
                b'[',
                b'M',
                (32 + code).min(255) as u8,
                at(col),
                at(row),
            ]
        }
    }
}

/// Whether a move is reported: every move in mode 1003, only moves with a
/// button down in mode 1002, none in plain click mode 1000.
pub fn reports_move(any: bool, drag: bool, held: bool) -> bool {
    any || (drag && held)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIFT: Mods = Mods {
        shift: true,
        ctrl: false,
        alt: false,
    };
    const CTRL: Mods = Mods {
        shift: false,
        ctrl: true,
        alt: false,
    };
    const ALT: Mods = Mods {
        shift: false,
        ctrl: false,
        alt: true,
    };

    #[test]
    fn arrows_follow_cursor_mode_and_modifiers() {
        assert_eq!(key_bytes(Key::Up, Mods::NONE, false), b"\x1b[A");
        assert_eq!(key_bytes(Key::Up, Mods::NONE, true), b"\x1bOA");
        assert_eq!(key_bytes(Key::Left, CTRL, true), b"\x1b[1;5D");
        assert_eq!(key_bytes(Key::End, SHIFT, false), b"\x1b[1;2F");
    }

    #[test]
    fn tilde_keys_and_function_keys() {
        assert_eq!(key_bytes(Key::Delete, Mods::NONE, false), b"\x1b[3~");
        assert_eq!(key_bytes(Key::PageUp, ALT, false), b"\x1b[5;3~");
        assert_eq!(key_bytes(Key::F(1), Mods::NONE, false), b"\x1bOP");
        assert_eq!(key_bytes(Key::F(4), CTRL, false), b"\x1b[1;5S");
        assert_eq!(key_bytes(Key::F(5), Mods::NONE, false), b"\x1b[15~");
        assert_eq!(key_bytes(Key::F(12), SHIFT, false), b"\x1b[24;2~");
        assert!(key_bytes(Key::F(13), Mods::NONE, false).is_empty());
    }

    #[test]
    fn characters_map_to_what_programs_expect() {
        let send = |c, m| char_action(c, m);
        assert_eq!(send('a', Mods::NONE), CharAction::Send(b"a".to_vec()));
        assert_eq!(
            send('\u{e6}', Mods::NONE),
            CharAction::Send("\u{e6}".as_bytes().to_vec())
        );
        assert_eq!(send('x', ALT), CharAction::Send(b"\x1bx".to_vec()));
        assert_eq!(send('\r', Mods::NONE), CharAction::Send(b"\r".to_vec()));
        assert_eq!(send('\r', SHIFT), CharAction::Send(b"\x1b\r".to_vec()));
        assert_eq!(send('\t', SHIFT), CharAction::Send(b"\x1b[Z".to_vec()));
        assert_eq!(send('\u{8}', Mods::NONE), CharAction::Send(vec![0x7f]));
        assert_eq!(send('\u{7f}', CTRL), CharAction::Send(vec![0x08]));
        assert_eq!(send('\u{1b}', Mods::NONE), CharAction::Send(vec![0x1b]));
    }

    #[test]
    fn clipboard_keys_are_actions() {
        assert_eq!(char_action('\u{16}', CTRL), CharAction::Paste);
        assert_eq!(char_action('\u{3}', CTRL), CharAction::CopyOrInterrupt);
        let ctrl_shift = Mods {
            shift: true,
            ..CTRL
        };
        assert_eq!(char_action('\u{3}', ctrl_shift), CharAction::Copy);
        assert_eq!(char_action('\u{14}', ctrl_shift), CharAction::NewShell);
        assert_eq!(char_action('\u{14}', CTRL), CharAction::Send(vec![0x14]));
    }

    #[test]
    fn paste_normalises_lines_and_brackets() {
        assert_eq!(paste_bytes("a\r\nb\nc", false), b"a\rb\rc");
        assert_eq!(
            paste_bytes("x\x1b[201~rm", true),
            b"\x1b[200~x[201~rm\x1b[201~"
        );
    }

    #[test]
    fn chords_need_their_modifiers() {
        let ctrl_shift = Mods {
            shift: true,
            ..CTRL
        };
        let ctrl_alt = Mods { alt: true, ..CTRL };
        assert_eq!(chord(VK_RETURN, ctrl_shift), Some(Chord::Zoom));
        assert_eq!(chord(VK_RETURN, CTRL), None);
        assert_eq!(chord(VK_RETURN, SHIFT), None);
        assert_eq!(chord(VK_F, ctrl_shift), Some(Chord::Find));
        assert_eq!(chord(VK_F, CTRL), None);
        assert_eq!(chord(VK_LEFT, ctrl_alt), Some(Chord::Focus(Dir::Left)));
        assert_eq!(chord(VK_DOWN, ctrl_alt), Some(Chord::Focus(Dir::Down)));
        assert_eq!(chord(VK_LEFT, CTRL), None);
        assert_eq!(chord(VK_LEFT, ALT), None);
        let bigger = Some(Chord::Font(FontStep::Bigger));
        assert_eq!(chord(VK_OEM_PLUS, CTRL), bigger);
        assert_eq!(chord(VK_OEM_PLUS, ctrl_shift), bigger);
        assert_eq!(chord(VK_ADD, CTRL), bigger);
        assert_eq!(
            chord(VK_OEM_MINUS, CTRL),
            Some(Chord::Font(FontStep::Smaller))
        );
        assert_eq!(chord(VK_0, CTRL), Some(Chord::Font(FontStep::Reset)));
        // AltGr and a key is typing, not a chord.
        assert_eq!(chord(VK_OEM_PLUS, ctrl_alt), None);
        assert_eq!(chord(VK_0, ctrl_alt), None);
    }

    #[test]
    fn font_size_steps_and_stops() {
        assert_eq!(font_size(15.0, FontStep::Bigger), 16.0);
        assert_eq!(font_size(15.0, FontStep::Smaller), 14.0);
        assert_eq!(font_size(14.6, FontStep::Bigger), 16.0);
        assert_eq!(font_size(FONT_MAX, FontStep::Bigger), FONT_MAX);
        assert_eq!(font_size(FONT_MIN, FontStep::Smaller), FONT_MIN);
        assert_eq!(font_size(30.0, FontStep::Reset), FONT_DEFAULT);
    }

    #[test]
    fn wheel_reports_in_every_encoding() {
        let wheel = |up, col, row, mods, enc| {
            let b = if up {
                Button::WheelUp
            } else {
                Button::WheelDown
            };
            mouse_bytes(MouseEvent::Press(b), col, row, mods, enc)
        };
        use MouseEncoding::*;
        assert_eq!(wheel(true, 0, 0, Mods::NONE, Sgr), b"\x1b[<64;1;1M");
        assert_eq!(wheel(false, 9, 4, CTRL, Sgr), b"\x1b[<81;10;5M");
        assert_eq!(
            wheel(true, 2, 3, Mods::NONE, X10),
            [0x1b, b'[', b'M', 96, 35, 36]
        );
        assert_eq!(wheel(false, 500, 0, SHIFT, X10)[3..], [101, 255, 33]);
    }

    #[test]
    fn clicks_and_releases() {
        use MouseEncoding::*;
        let left = MouseEvent::Press(Button::Left);
        let up = MouseEvent::Release(Button::Right);
        assert_eq!(mouse_bytes(left, 0, 0, Mods::NONE, Sgr), b"\x1b[<0;1;1M");
        assert_eq!(mouse_bytes(up, 4, 2, Mods::NONE, Sgr), b"\x1b[<2;5;3m");
        assert_eq!(
            mouse_bytes(left, 1, 1, ALT, X10),
            [0x1b, b'[', b'M', 40, 34, 34]
        );
        assert_eq!(mouse_bytes(up, 1, 1, Mods::NONE, X10)[3], 35);
        let middle = MouseEvent::Press(Button::Middle);
        assert_eq!(mouse_bytes(middle, 0, 0, CTRL, Sgr), b"\x1b[<17;1;1M");
    }

    #[test]
    fn moves_add_32_and_say_which_button_is_held() {
        use MouseEncoding::*;
        let drag = MouseEvent::Move(Some(Button::Left));
        let hover = MouseEvent::Move(None);
        assert_eq!(mouse_bytes(drag, 2, 0, Mods::NONE, Sgr), b"\x1b[<32;3;1M");
        assert_eq!(mouse_bytes(hover, 2, 0, Mods::NONE, Sgr), b"\x1b[<35;3;1M");
        assert_eq!(mouse_bytes(hover, 0, 0, Mods::NONE, X10)[3], 67);
    }

    #[test]
    fn utf8_reaches_past_cell_223() {
        let b = mouse_bytes(
            MouseEvent::Press(Button::Left),
            300,
            0,
            Mods::NONE,
            MouseEncoding::Utf8,
        );
        let mut want = b"\x1b[M ".to_vec();
        want.extend_from_slice("\u{14d}!".as_bytes());
        assert_eq!(b, want);
    }

    #[test]
    fn which_moves_are_reported() {
        assert!(!reports_move(false, false, true));
        assert!(!reports_move(false, true, false));
        assert!(reports_move(false, true, true));
        assert!(reports_move(true, false, false));
    }
}
