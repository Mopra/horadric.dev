//! What a key pressed on a Mac sends to the program, or does on the stage.
//!
//! The Mac splits typing differently from Windows. Every key arrives as one
//! event with a hardware key code, the characters the layout made of it and
//! the characters it makes without modifiers. Option composes characters
//! (a Danish `@` is Option+2), so it is never Meta here, as in Terminal.
//! Cmd belongs to the app: copy, paste, the font. Ctrl goes to the
//! program. Keys that make plain text are left to AppKit's text input, so
//! dead keys and input methods work; this module says which those are.
//! No AppKit in it, so all of it is tested.

use crate::keys::{self, FontStep, Key, KeyEvent, Kitty, Mods};
use crate::layout::Dir;

/// A key press as AppKit reports it.
#[derive(Debug, Clone, Copy)]
pub struct Press<'a> {
    /// `keyCode`: the key's place on the keyboard, whatever the layout.
    pub code: u16,
    /// `characters`: what the layout made of it with the modifiers held.
    pub chars: &'a str,
    /// `charactersIgnoringModifiers`.
    pub bare: &'a str,
    pub shift: bool,
    pub ctrl: bool,
    /// Option.
    pub alt: bool,
    pub cmd: bool,
}

/// What a press does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Send(Vec<u8>),
    /// Plain typing: hand the event to AppKit's text input, which calls
    /// back with the text, composed or not.
    Text,
    Copy,
    Paste,
    /// Cmd+T: a new plain terminal in the project.
    NewShell,
    Font(FontStep),
    /// Cmd+Shift+Return: this pane alone fills the stage, or the grid
    /// comes back.
    Zoom,
    /// Cmd+Option+arrow: the keyboard to the pane in that direction.
    Focus(Dir),
    /// Not for the terminal: let the menus have it.
    Pass,
}

const RETURN: u16 = 36;
const TAB: u16 = 48;
const DELETE: u16 = 51;
const ESCAPE: u16 = 53;
const KEYPAD_ENTER: u16 = 76;
const LEFT: u16 = 123;
const RIGHT: u16 = 124;
const DOWN: u16 = 125;
const UP: u16 = 126;

/// The key at a key code that makes no character.
pub fn special(code: u16) -> Option<Key> {
    Some(match code {
        LEFT => Key::Left,
        RIGHT => Key::Right,
        DOWN => Key::Down,
        UP => Key::Up,
        115 => Key::Home,
        119 => Key::End,
        116 => Key::PageUp,
        121 => Key::PageDown,
        117 => Key::Delete,
        114 => Key::Insert,
        122 => Key::F(1),
        120 => Key::F(2),
        99 => Key::F(3),
        118 => Key::F(4),
        96 => Key::F(5),
        97 => Key::F(6),
        98 => Key::F(7),
        100 => Key::F(8),
        101 => Key::F(9),
        109 => Key::F(10),
        103 => Key::F(11),
        111 => Key::F(12),
        _ => return None,
    })
}

/// The control character Ctrl makes with `c`, as a terminal sends it.
pub fn control(c: char) -> Option<u8> {
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 27,
        '\\' | '4' => 28,
        ']' | '5' => 29,
        '^' | '6' => 30,
        '_' | '-' | '7' => 31,
        '?' | '8' => 127,
        _ => return None,
    })
}

/// What `p` does. `flags` are the kitty keyboard enhancements the program
/// asked for, `app_cursor` is DECCKM.
pub fn action(p: Press, flags: Kitty, app_cursor: bool) -> Action {
    let mods = Mods {
        shift: p.shift,
        ctrl: p.ctrl,
        alt: p.alt,
    };
    if p.cmd {
        return command(p);
    }
    if let Some(key) = special(p.code) {
        let bytes = keys::kitty_key(key, mods, KeyEvent::Press, flags, app_cursor);
        return Action::Send(bytes);
    }
    let named = match p.code {
        RETURN | KEYPAD_ENTER => Some(13),
        TAB => Some(9),
        DELETE => Some(127),
        ESCAPE => Some(27),
        _ => None,
    };
    if let Some(base) = named {
        if let Some(bytes) = keys::kitty_text(base, None, mods, KeyEvent::Press, flags) {
            return Action::Send(bytes);
        }
        let bytes = match base {
            // Meta+Enter is the newline Claude Code understands everywhere.
            13 if p.shift || p.alt => b"\x1b\r".to_vec(),
            13 => b"\r".to_vec(),
            9 if p.shift => b"\x1b[Z".to_vec(),
            9 => b"\t".to_vec(),
            // Option+Delete deletes a word, as in Terminal.
            127 if p.alt => b"\x1b\x7f".to_vec(),
            127 if p.ctrl => vec![0x08],
            127 => vec![0x7f],
            _ => vec![0x1b],
        };
        return Action::Send(bytes);
    }
    if p.ctrl {
        let Some(c) = p.bare.chars().next() else {
            return Action::Pass;
        };
        let base = c.to_lowercase().next().unwrap_or(c) as u32;
        if let Some(bytes) = keys::kitty_text(base, Some(c), mods, KeyEvent::Press, flags) {
            return Action::Send(bytes);
        }
        return match control(c) {
            Some(b) => Action::Send(vec![b]),
            None => Action::Text,
        };
    }
    Action::Text
}

/// Cmd chords, which are the app's.
fn command(p: Press) -> Action {
    if p.alt {
        let dir = match p.code {
            LEFT => Some(Dir::Left),
            RIGHT => Some(Dir::Right),
            UP => Some(Dir::Up),
            DOWN => Some(Dir::Down),
            _ => None,
        };
        if let Some(d) = dir {
            return Action::Focus(d);
        }
    }
    if matches!(p.code, RETURN | KEYPAD_ENTER) && p.shift {
        return Action::Zoom;
    }
    match p.bare.to_lowercase().as_str() {
        "c" => Action::Copy,
        "v" => Action::Paste,
        "t" => Action::NewShell,
        "=" | "+" => Action::Font(FontStep::Bigger),
        "-" => Action::Font(FontStep::Smaller),
        "0" => Action::Font(FontStep::Reset),
        _ => Action::Pass,
    }
}

/// The text AppKit's text input made, as bytes for the program. A newline
/// typed through an input method is Return.
pub fn text_bytes(text: &str) -> Vec<u8> {
    text.replace('\n', "\r").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: u16, chars: &'static str) -> Press<'static> {
        Press {
            code,
            chars,
            bare: chars,
            shift: false,
            ctrl: false,
            alt: false,
            cmd: false,
        }
    }

    const PLAIN: Kitty = Kitty {
        disambiguate: false,
        events: false,
        alternates: false,
        all_keys: false,
        text: false,
    };

    #[test]
    fn letters_and_option_symbols_are_left_to_text_input() {
        assert_eq!(action(press(0, "a"), PLAIN, false), Action::Text);
        // Option+2 on a Danish keyboard is @, which is typing.
        let at = Press {
            alt: true,
            ..press(19, "@")
        };
        assert_eq!(action(at, PLAIN, false), Action::Text);
    }

    #[test]
    fn arrows_follow_the_cursor_mode() {
        assert_eq!(
            action(press(UP, "\u{F700}"), PLAIN, false),
            Action::Send(b"\x1b[A".to_vec())
        );
        assert_eq!(
            action(press(UP, "\u{F700}"), PLAIN, true),
            Action::Send(b"\x1bOA".to_vec())
        );
    }

    #[test]
    fn return_tab_delete_and_escape() {
        assert_eq!(
            action(press(RETURN, "\r"), PLAIN, false),
            Action::Send(b"\r".to_vec())
        );
        let shift_return = Press {
            shift: true,
            ..press(RETURN, "\r")
        };
        assert_eq!(
            action(shift_return, PLAIN, false),
            Action::Send(b"\x1b\r".to_vec())
        );
        let flags = Kitty {
            disambiguate: true,
            ..PLAIN
        };
        assert_eq!(
            action(shift_return, flags, false),
            Action::Send(b"\x1b[13;2u".to_vec())
        );
        assert_eq!(
            action(press(DELETE, "\u{7f}"), PLAIN, false),
            Action::Send(vec![0x7f])
        );
        assert_eq!(
            action(press(ESCAPE, "\u{1b}"), PLAIN, false),
            Action::Send(vec![0x1b])
        );
        let back_tab = Press {
            shift: true,
            ..press(TAB, "\t")
        };
        assert_eq!(
            action(back_tab, PLAIN, false),
            Action::Send(b"\x1b[Z".to_vec())
        );
    }

    #[test]
    fn ctrl_letters_are_control_characters() {
        let ctrl_c = Press {
            ctrl: true,
            ..press(8, "\u{3}")
        };
        let ctrl_c = Press {
            bare: "c",
            ..ctrl_c
        };
        assert_eq!(action(ctrl_c, PLAIN, false), Action::Send(vec![3]));
        assert_eq!(control('['), Some(27));
        assert_eq!(control('é'), None);
    }

    #[test]
    fn cmd_chords_are_the_apps() {
        let cmd = |bare: &'static str| Press {
            cmd: true,
            ..press(0, bare)
        };
        assert_eq!(action(cmd("c"), PLAIN, false), Action::Copy);
        assert_eq!(action(cmd("v"), PLAIN, false), Action::Paste);
        assert_eq!(action(cmd("t"), PLAIN, false), Action::NewShell);
        assert_eq!(
            action(cmd("="), PLAIN, false),
            Action::Font(FontStep::Bigger)
        );
        assert_eq!(action(cmd("q"), PLAIN, false), Action::Pass);
        let focus = Press {
            alt: true,
            ..Press {
                cmd: true,
                ..press(LEFT, "")
            }
        };
        assert_eq!(action(focus, PLAIN, false), Action::Focus(Dir::Left));
    }

    #[test]
    fn function_keys_have_their_codes() {
        assert_eq!(special(122), Some(Key::F(1)));
        assert_eq!(special(111), Some(Key::F(12)));
        assert_eq!(special(0), None);
        assert_eq!(text_bytes("a\nb"), b"a\rb".to_vec());
    }
}
