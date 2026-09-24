//! What a key press sends to the program, in xterm's encoding.
//!
//! Windows splits typing in two. Keys that make a character (letters, digits,
//! Enter, Tab, Backspace, dead key compositions, AltGr symbols on a Danish
//! keyboard) arrive as `WM_CHAR` after the layout has done its work. Keys
//! that do not (arrows, Home, F5) only arrive as `WM_KEYDOWN`. This module
//! has one function for each half and no Win32 in it.

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
}
