//! The desktop face of Glance.
//!
//! One frameless, always-on-top window per project cluster. Tiles are drawn
//! inside it with Direct2D and DirectWrite. There is no toolkit and no web
//! view: the window manager fight is the whole project, so nothing sits
//! between us and Win32.
//!
//! An expanded session is a second kind of window, a real terminal: the
//! agent runs in a ConPTY, `alacritty_terminal` keeps the grid, and the grid
//! is drawn with DirectWrite glyph runs.
//!
//! [`layout`], [`theme`], [`palette`], [`keys`] and [`frame`] are pure and
//! tested. The rest is Windows only and verified on screen.

pub mod frame;
pub mod keys;
pub mod layout;
pub mod palette;
pub mod theme;

#[cfg(windows)]
pub mod app;
#[cfg(windows)]
mod clipboard;
#[cfg(windows)]
mod console;
#[cfg(windows)]
mod glyphs;
#[cfg(windows)]
mod render;
#[cfg(windows)]
mod terminal;
#[cfg(windows)]
mod window;
