//! The window that shows the agent's desk: it hosts Windows' Remote
//! Desktop control, connected to a child session of this user.

/// Switches child sessions on for this machine. Needs admin rights: it
/// is what `horadric desk enable` runs, started elevated by the window.
pub fn enable_child_sessions() -> Result<(), String> {
    Err("the desk's window is not built yet".into())
}
