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

/// Environment variable Glance sets on every `claude` it spawns.
pub const SESSION_ENV: &str = "GLANCE_SESSION";

/// Header the hook carries the session id in.
pub const SESSION_HEADER: &str = "x-glance-session";

/// Default port. Overridable with `GLANCE_PORT`.
pub const DEFAULT_PORT: u16 = 43117;

/// Path the hook posts to. The word `glance` in the URL is how the installer
/// recognises its own entries when updating or removing them.
pub const HOOK_PATH: &str = "/glance/hook";

/// Path `glance new` posts to, asking the running app to start a session in
/// a terminal of its own.
pub const NEW_PATH: &str = "/glance/new";

/// Header a command request must carry. A browser can not send a custom
/// header to another origin without a preflight we never answer, so this is
/// what stops a web page from starting processes through localhost.
pub const COMMAND_HEADER: &str = "x-glance-command";

/// Port to listen on and to write into the hook URL.
pub fn port() -> u16 {
    std::env::var("GLANCE_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

pub fn hook_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}{HOOK_PATH}")
}
