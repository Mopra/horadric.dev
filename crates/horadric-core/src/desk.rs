//! The agent's desk: a second Windows session of the same user (a child
//! session), where sessions drive mouse and keyboard while the human
//! keeps their own. The pure half: the names and numbers its three parts
//! agree on. The window that shows it is `horadric_ui::desk::window`,
//! the helper that runs inside it `horadric_ui::desk::hand`, and the
//! `desk_` tools are in the binary's `desk.rs`. The plan is
//! `docs/plan/computer-use.md`.

/// The hand's port, on 127.0.0.1 only. It speaks the screen share's
/// protocol (`crate::screen_share`), plus `/apps` and `/launch`.
pub const PORT: u16 = 43121;

/// A dev instance's hand, so it serves beside the installed one.
pub const DEV_PORT: u16 = 43122;

/// The hand's token, in the state folder, written anew at each start.
pub const TOKEN_FILE: &str = "desk-token";

/// The file in the state folder that names the desk's Windows session
/// while the window is connected, see [`session_line`].
pub const SESSION_FILE: &str = "desk-session";

/// The HKCU RunOnce value that starts the hand at the desk's sign-in.
pub const RUN_ONCE: &str = "HoradricDeskHand";

/// The port to reach the hand on.
pub fn port(dev: bool) -> u16 {
    if dev {
        DEV_PORT
    } else {
        PORT
    }
}

/// The Credential Manager name the desk's sign-in is kept under.
/// `state_name` is `horadric_hooks::state_name()`, so a dev instance has
/// its own. The user name is the account's sign-in name (the email of a
/// Microsoft account), the secret the password as UTF-8.
pub fn credential_target(state_name: &str) -> String {
    format!("Horadric desk ({state_name})")
}

/// What [`SESSION_FILE`] holds: the child session's id.
pub fn session_line(id: u32) -> String {
    format!("{id}\n")
}

/// The session id a [`SESSION_FILE`] names, if it is one.
pub fn read_session(text: &str) -> Option<u32> {
    text.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_file_reads_back() {
        assert_eq!(read_session(&session_line(7)), Some(7));
        assert_eq!(read_session(""), None);
        assert_eq!(read_session("seven"), None);
    }

    #[test]
    fn a_dev_instance_has_its_own_port_and_credential() {
        assert_ne!(port(true), port(false));
        assert_ne!(credential_target("Horadric"), credential_target("Horadric-dev"));
    }
}
