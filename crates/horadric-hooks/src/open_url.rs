//! `horadric open-url`: a link a command line program opens (`gh auth
//! login --web`, `gcloud auth login`, Claude Code's MCP sign in) lands in
//! the browser pane of the project the terminal belongs to, through
//! `BROWSER`, instead of the system browser.
//!
//! Every terminal Horadric starts, a session's or a plain shell's, carries
//! [`PANE_ENV`]: the port of the Horadric that started it, the pane's id
//! and a key the app minted for it. A plain shell gets no session tag (a
//! `claude` typed into it must stay nobody's), so this is a tag of its
//! own, good for opening a link and nothing else.

use std::path::Path;

/// Environment variable naming the pane a terminal runs in, as
/// `port:key:id` (see [`Pane`]).
pub const PANE_ENV: &str = "HORADRIC_PANE";

/// The `op` of the browser call that opens a link for the human.
pub const OPEN_OP: &str = "open_url";

/// The terminal `horadric open-url` runs in, read from [`PANE_ENV`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pane {
    pub port: u16,
    pub key: String,
    pub id: String,
}

impl Pane {
    /// The id last, since only it could hold a colon.
    pub fn to_env(&self) -> String {
        format!("{}:{}:{}", self.port, self.key, self.id)
    }

    pub fn parse(value: &str) -> Option<Pane> {
        let (port, rest) = value.trim().split_once(':')?;
        let (key, id) = rest.split_once(':')?;
        if key.is_empty() || id.is_empty() {
            return None;
        }
        Some(Pane {
            port: port.parse().ok()?,
            key: key.to_string(),
            id: id.to_string(),
        })
    }
}

/// The name the pane's key is kept under, apart from a session's own
/// secret key so it can never pass for one.
pub fn key_name(id: &str) -> String {
    format!("open-url:{id}")
}

/// `url` when it is a web address, None for anything else: a link handed
/// to the pane or the system's opener must not be a file, a `javascript:`
/// page or a program's own scheme.
pub fn web_url(url: &str) -> Option<&str> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    if host.is_empty() || host.starts_with(':') {
        return None;
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    Some(url)
}

/// What `BROWSER` is set to: the program alone, no arguments, with
/// forward slashes. Each opener reads it its own way, and this is the one
/// form all of them run. Claude Code and Python's `webbrowser` (gcloud,
/// az) run the whole value as one program with the link as its argument,
/// so it can not carry a subcommand. gh splits it like a POSIX shell,
/// where a backslash is an escape and would be eaten. Windows takes
/// forward slashes in a program's path. A space would still split it for
/// gh, so the caller passes a path without one where it can (the 8.3
/// name). Node's `open` package (vercel, wrangler) does not read
/// `BROWSER` at all.
pub fn browser_value(program: &Path) -> String {
    program.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_round_trips_with_a_colon_in_the_id() {
        let pane = Pane {
            port: 43118,
            key: "ab12".into(),
            id: "shell:3".into(),
        };
        assert_eq!(pane.to_env(), "43118:ab12:shell:3");
        assert_eq!(Pane::parse(&pane.to_env()), Some(pane));
    }

    #[test]
    fn pane_refuses_what_is_not_one() {
        for bad in ["", "43118", "43118:key", "43118::id", "x:key:id", "43118:key:"] {
            assert_eq!(Pane::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn web_urls_only() {
        for good in [
            "https://github.com/login/device",
            "HTTP://localhost:8085/callback?code=1",
            "https://user@claude.ai/oauth/authorize?a=b#c",
        ] {
            assert!(web_url(good).is_some(), "{good}");
        }
        for bad in [
            "file:///C:/Windows/system32/calc.exe",
            "javascript:alert(1)",
            "ms-settings:privacy",
            "https://",
            "https:///path",
            "https://:80/",
            "https://a.com/x y",
            "https://a.com/\nx",
            "C:\\Windows\\notepad.exe",
            "github.com",
        ] {
            assert!(web_url(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn browser_value_has_no_backslash() {
        let p = Path::new(r"C:\Users\Ann\AppData\Local\Horadric\hosts\horadric-host-1-2.exe");
        assert_eq!(
            browser_value(p),
            "C:/Users/Ann/AppData/Local/Horadric/hosts/horadric-host-1-2.exe"
        );
    }
}
