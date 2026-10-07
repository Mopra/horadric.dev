//! The Mac face of Horadric: AppKit through `objc2`, drawn with Core
//! Graphics and Core Text. It shares the pure halves of this crate with
//! the Windows face (layout, columns, theme, keys) and draws its own.

pub mod autostart;

/// Runs the app until it quits.
pub fn run(_port: u16, _reload: bool) -> Result<(), String> {
    Err("the Mac app is not built yet".into())
}
