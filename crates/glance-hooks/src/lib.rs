//! How Claude Code tells Glance what a session is doing.
//!
//! Claude Code supports `http` hooks: on every lifecycle event it POSTs the
//! event payload to a URL. Glance listens on localhost for those posts. The
//! hook is configured to send the `GLANCE_SESSION` environment variable as a
//! header, so a `claude` started by Glance (or inside a Glance terminal) is
//! tagged with the tile it belongs to, and a `claude` started anywhere else
//! sends an empty header and is ignored.
//!
//! No script runs, no process is spawned per event. Connection refused when
//! Glance is not running costs Claude Code a few microseconds.

pub mod client;
pub mod install;
pub mod listener;
pub mod transcript;

/// Environment variable Glance sets on every `claude` it spawns.
pub const SESSION_ENV: &str = "GLANCE_SESSION";

/// Header the hook carries the session id in.
pub const SESSION_HEADER: &str = "x-glance-session";

/// Environment variable Glance sets beside [`SESSION_ENV`]: the port of the
/// Glance that owns the session. The hook URL is fixed at install time, so
/// a session started by a dev instance still posts to the installed one,
/// which reads this from [`OWNER_HEADER`] and passes the event on.
pub const OWNER_ENV: &str = "GLANCE_OWNER_PORT";

/// Header the hook carries [`OWNER_ENV`] in.
pub const OWNER_HEADER: &str = "x-glance-port";

/// Default port. Overridable with `GLANCE_PORT`.
pub const DEFAULT_PORT: u16 = 43117;

/// Set to anything but `0` to run a development instance beside the
/// installed Glance: its own port, its own state, and nothing that changes
/// the machine. It is how Glance gets developed from inside Glance.
pub const DEV_ENV: &str = "GLANCE_DEV";

/// A dev instance's port, unless `GLANCE_PORT` says otherwise.
pub const DEV_PORT: u16 = 43118;

/// Path the hook posts to. The word `glance` in the URL is how the installer
/// recognises its own entries when updating or removing them.
pub const HOOK_PATH: &str = "/glance/hook";

/// Path `glance status` posts to: what Claude Code gave the status line of
/// a session Glance started.
pub const STATUS_PATH: &str = "/glance/status";

/// Path `glance new` posts to, asking the running app to start a session in
/// a terminal of its own.
pub const NEW_PATH: &str = "/glance/new";

/// Path `glance reload` posts to, asking the running app to hand over to a
/// new build once no session is mid turn.
pub const RELOAD_PATH: &str = "/glance/reload";

/// Header a command request must carry. A browser can not send a custom
/// header to another origin without a preflight we never answer, so this is
/// what stops a web page from starting processes through localhost.
pub const COMMAND_HEADER: &str = "x-glance-command";

/// Whether this is a development instance, from [`DEV_ENV`].
pub fn dev() -> bool {
    is_dev(std::env::var(DEV_ENV).ok().as_deref())
}

fn is_dev(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// Port to listen on and to write into the hook URL.
pub fn port() -> u16 {
    port_from(std::env::var("GLANCE_PORT").ok().as_deref(), dev())
}

fn port_from(glance_port: Option<&str>, dev: bool) -> u16 {
    glance_port
        .and_then(|p| p.parse().ok())
        .unwrap_or(if dev { DEV_PORT } else { DEFAULT_PORT })
}

pub fn hook_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}{HOOK_PATH}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_is_any_value_but_empty_or_zero() {
        assert!(is_dev(Some("1")));
        assert!(is_dev(Some("yes")));
        assert!(!is_dev(Some("0")));
        assert!(!is_dev(Some("")));
        assert!(!is_dev(None));
    }

    #[test]
    fn dev_moves_the_default_port_but_not_an_explicit_one() {
        assert_eq!(port_from(None, false), DEFAULT_PORT);
        assert_eq!(port_from(None, true), DEV_PORT);
        assert_eq!(port_from(Some("5000"), true), 5000);
        assert_eq!(port_from(Some("junk"), false), DEFAULT_PORT);
    }
}
