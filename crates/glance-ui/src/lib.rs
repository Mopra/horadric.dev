//! The desktop face of Glance.
//!
//! One frameless, always-on-top window per project cluster. Tiles are drawn
//! inside it with Direct2D and DirectWrite. There is no toolkit and no web
//! view: the window manager fight is the whole project, so nothing sits
//! between us and Win32.
//!
//! [`layout`] and [`theme`] are pure and tested. [`render`] and [`window`]
//! are Windows only.

pub mod layout;
pub mod theme;

#[cfg(windows)]
pub mod app;
#[cfg(windows)]
mod render;
#[cfg(windows)]
mod window;
