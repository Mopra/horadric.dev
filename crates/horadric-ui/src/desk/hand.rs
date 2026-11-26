//! The hand: Horadric's helper inside the agent's desk. It serves that
//! session's screen and takes input for it, for the `desk_` tools.

/// Whether this process runs inside the agent's desk, so a full Horadric
/// that autostarts there knows to leave.
pub fn in_desk() -> bool {
    false
}

/// Serves the desk until its session ends. What `horadric desk hand` runs.
pub fn run() -> Result<(), String> {
    Err("the desk's hand is not built yet".into())
}
