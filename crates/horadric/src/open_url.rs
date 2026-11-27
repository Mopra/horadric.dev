//! `horadric open-url URL`, and `horadric URL` for the openers that run
//! `BROWSER` as one program: opens a web link in the browser pane of the
//! project whose terminal this runs in, asking the Horadric that started
//! the terminal (see `horadric_hooks::open_url`). Where there is none, or
//! it says no, the system's browser opens it, so a sign in is never stuck.

use std::process::{Command, Stdio};
use std::time::Duration;

use horadric_hooks::open_url::{web_url, Pane, OPEN_OP, PANE_ENV};
use horadric_hooks::{client, BROWSER_PATH, COMMAND_HEADER, SESSION_HEADER};
use serde_json::{json, Value};

/// The app answers at once; a little slack for a busy UI thread.
const APP_WAIT: Duration = Duration::from_secs(10);

pub fn run(args: &[String]) -> Result<(), String> {
    let [url] = args else {
        return Err("usage: horadric open-url URL".into());
    };
    let url = web_url(url).ok_or("only http and https addresses are opened")?;
    let Some(pane) = std::env::var(PANE_ENV).ok().and_then(|v| Pane::parse(&v)) else {
        return system_open(url);
    };
    match in_pane(&pane, url) {
        Ok(()) => Ok(()),
        Err(why) => {
            eprintln!(
                "horadric: the browser pane did not open it ({why}), the system browser does"
            );
            system_open(url)
        }
    }
}

fn in_pane(pane: &Pane, url: &str) -> Result<(), String> {
    let headers = [
        (COMMAND_HEADER, "browser"),
        (SESSION_HEADER, pane.id.as_str()),
    ];
    let body = json!({ "op": OPEN_OP, "url": url, "key": pane.key }).to_string();
    let (status, text) = client::ask(pane.port, BROWSER_PATH, &headers, &body, APP_WAIT)
        .map_err(|e| format!("Horadric did not answer: {e}"))?;
    if status != 200 {
        return Err(format!("Horadric refused it ({status})"));
    }
    let answer: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    match answer["error"].as_str() {
        Some(e) => Err(e.to_string()),
        None => Ok(()),
    }
}

/// The system's own opener, never `BROWSER`, which points back here.
fn system_open(url: &str) -> Result<(), String> {
    #[cfg(windows)]
    let mut command = {
        let mut c = Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    command
        .env_remove("BROWSER")
        .stdin(Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| format!("could not open {url}: {e}"))
}
