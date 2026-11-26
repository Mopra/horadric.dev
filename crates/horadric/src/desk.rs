//! The `desk_` tools of `horadric mcp`: mouse, keyboard and screen on the
//! agent's desk, a second Windows session of the same user that the app
//! shows in a window. The names its parts agree on are in
//! `horadric_core::desk`, the plan in `docs/plan/computer-use.md`.
//!
//! The desk's hand speaks the screen share's protocol, so the looking and
//! the input are `crate::remote`'s, with no tunnel: the hand listens on
//! this machine's own loopback. Its token is read from the state folder,
//! goes out only in a header, and only after whatever answers on the port
//! has proven it holds the same one.
//!
//! Nobody is asked before a tool runs: the human opening the desk is the
//! permission. With no desk open a tool says so and does nothing: the
//! window names the desk's session in a file while it is open, and a
//! hand that still answers without it belongs to a desk that was closed.

use std::path::PathBuf;

use horadric_core::remote::Response;
use horadric_core::screen_share::Region;
use horadric_core::{desk, remote};
use serde_json::{json, Value};

use crate::remote::{Failed, Hello, Link, SHORT_WAIT};

/// What every tool answers while no desk is open.
pub(crate) const CLOSED: &str = "The agent's desk is not open. The human opens it from \
     Horadric's tray menu (Agent's desk).";

/// Where the hand says what is open.
const APPS_PATH: &str = "/apps";

/// Where the hand is asked to start an app.
const LAUNCH_PATH: &str = "/launch";

/// The most windows of the desk that are listed, and the most characters
/// of a title: both come from programs on the desk, and are shown to a
/// model.
const WINDOWS_MOST: usize = 60;
const TITLE_MOST: usize = 120;

/// Where a hand is looked for: its port, the file its token is in, and
/// the file the window names the desk's session in while it is open.
struct Desk {
    port: u16,
    token_file: Option<PathBuf>,
    session_file: Option<PathBuf>,
}

impl Desk {
    /// This instance's desk: a dev instance has its own, beside the
    /// installed one's.
    fn here() -> Desk {
        Desk {
            port: desk::port(horadric_hooks::dev()),
            token_file: horadric_hooks::state_dir().map(|d| d.join(desk::TOKEN_FILE)),
            session_file: horadric_hooks::state_dir().map(|d| d.join(desk::SESSION_FILE)),
        }
    }

    /// Whether the desk's window is open: it names the session while it
    /// is, and takes the name away when it closes.
    fn open(&self) -> bool {
        (self.session_file.as_ref())
            .and_then(|p| std::fs::read_to_string(p).ok())
            .is_some_and(|text| desk::read_session(&text).is_some())
    }

    /// A link to the hand, proven: the token from its file, and an answer
    /// to the proof from the port. No window, no token or nobody there is
    /// a desk that is not open.
    fn reach(&self) -> Result<Link, String> {
        // Asked first and at every call: a closed desk stays signed in,
        // so its hand would still answer and prove itself.
        if !self.open() {
            return Err(CLOSED.into());
        }
        let token = self
            .token_file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| remote::parse_token(&t))
            .ok_or(CLOSED)?;
        // A quick look first: a closed desk is the usual reason, and the
        // proof would wait seconds on a port nobody listens on.
        if !crate::remote::answers(self.port) {
            return Err(CLOSED.into());
        }
        match crate::remote::hello(self.port, &token) {
            Hello::Proven => Ok(Link {
                port: self.port,
                token,
            }),
            Hello::Silent(_) => Err(CLOSED.into()),
            Hello::Wrong => Err(format!(
                "something else answers on the desk's port ({}): it could not prove it holds \
                 the desk's token, so nothing was sent to it. A hand only starts when the \
                 desk signs in: ending the desk from Horadric's tray menu (Agent's desk, End \
                 the desk) and opening it again starts a new one.",
                self.port
            )),
        }
    }

    /// Runs `ask` through a proven link. The hand writes a new token at
    /// each of its starts, so one refusal is a hand that started between
    /// the reading and the asking: the token is read and proven once more.
    fn through<T>(&self, mut ask: impl FnMut(&Link) -> Result<T, Failed>) -> Result<T, String> {
        for _ in 0..2 {
            let link = self.reach()?;
            match ask(&link) {
                Ok(t) => return Ok(t),
                Err(Failed::Other(e)) => return Err(worded(&remote::mask(&e, &link.token))),
                Err(Failed::Refused) => {}
            }
        }
        Err("the desk's hand refused its own token (403)".into())
    }
}

/// A failure of `crate::remote`'s, which speaks of a remote and a tunnel,
/// in the desk's words. Its sentences are shared with the `remote_` tools
/// rather than written twice, and here the far end is the desk.
fn worded(e: &str) -> String {
    e.replace("the remote's", "the desk's")
        .replace("the remote", "the desk")
        .replace("the tunnel", "the desk's hand")
}

/// Whether `name` is one of the `desk_` tools.
pub(crate) fn is_tool(name: &str) -> bool {
    matches!(
        name,
        "desk_screenshot"
            | "desk_click"
            | "desk_type"
            | "desk_press"
            | "desk_scroll"
            | "desk_apps"
            | "desk_open"
    )
}

/// What every description says of when the tools work.
const OPEN: &str = "It works only while the user has the agent's desk open (Horadric's tray \
     menu, Agent's desk): closed, the tool says so and does nothing.";

/// What every input tool's description says.
const ACT: &str = "On the agent's desk, as desk_screenshot shows it. Look with \
     desk_screenshot, act, then look again to see what it did: nothing comes back but a line. \
     Nothing but a click gives a window the keyboard, so click a window before typing into \
     it. When the input is refused (409) the error names the real reason, such as a UAC \
     prompt or a window that runs as administrator.";

/// The `desk_` tools, offered with the browser's on Windows, where the
/// desk is.
pub fn tools() -> Vec<Value> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let tool = crate::mcp::tool;
    vec![
        tool(
            "desk_screenshot",
            &format!(
                "A picture of the agent's desk: a Windows desktop of the user's own account, \
                 with their files, apps and logins, in a window the user can watch while \
                 they keep working in their own session. Use the desk_ tools when a task \
                 needs a desktop app, the mouse or the keyboard outside a web page; for a \
                 plain web page prefer the browser_ tools. {OPEN} Its text line gives the \
                 screen in physical pixels and the factor to multiply a spot in the picture \
                 by for the x and y desk_click and desk_scroll take, since the picture may \
                 be scaled down to width. With zoom the picture is only that region of the \
                 screen, at full resolution, and the line says what to add to a spot in it \
                 as well: look at the whole desk first, then zoom in where the text is too \
                 small to read."
            ),
            json!({
                "width": {
                    "type": "integer",
                    "description": "The widest the picture may be, in pixels (default 1600; with zoom there is no default and the region comes unscaled)"
                },
                "zoom": {
                    "type": "object",
                    "description": "A region in screen pixels (not picture pixels) to read small text in: the picture is then that region only, cut to the screen's edges, at full resolution",
                    "properties": {
                        "x": { "type": "number", "description": "Screen pixels from the left" },
                        "y": { "type": "number", "description": "Screen pixels from the top" },
                        "width": { "type": "number" },
                        "height": { "type": "number" },
                    },
                    "required": ["x", "y", "width", "height"],
                },
            }),
            &[],
        ),
        tool(
            "desk_click",
            &format!(
                "Click on the agent's desk, which also gives the window under it the \
                 keyboard. {ACT} x and y are screen pixels from the top left: a spot in the \
                 screenshot multiplied by the factor its text line gives (the product need \
                 not be whole, it is rounded). {OPEN}"
            ),
            json!({
                "x": { "type": "number", "description": "Screen pixels from the left" },
                "y": { "type": "number", "description": "Screen pixels from the top" },
                "button": {
                    "type": "string",
                    "enum": ["left", "right", "middle"],
                    "description": "Which button (default left)"
                },
                "double": { "type": "boolean", "description": "Click twice, to open (default false)" },
            }),
            &["x", "y"],
        ),
        tool(
            "desk_type",
            &format!(
                "Type text into whatever has the keyboard focus on the agent's desk. Any \
                 characters, typed as Unicode, so the keyboard layout does not change them. \
                 A line break in the text presses Enter and a tab presses Tab; for any other \
                 key or a shortcut use desk_press. {ACT} {OPEN}"
            ),
            json!({ "text": { "type": "string", "description": "What to type" } }),
            &["text"],
        ),
        tool(
            "desk_press",
            &format!(
                "Press a key or a shortcut on the agent's desk: a key such as Enter, Tab, \
                 Escape, Backspace, Delete, Up, PageDown, F5, a letter, a digit or one of \
                 - = , . / (Plus and Minus by name too), or a chord such as Ctrl+S, Alt+F4 \
                 or Win+R (the modifiers Ctrl, Shift, Alt and Win first, joined by +). \
                 {ACT} {OPEN}"
            ),
            json!({ "key": { "type": "string", "description": "The key or chord, e.g. Enter or Ctrl+S" } }),
            &["key"],
        ),
        tool(
            "desk_scroll",
            &format!(
                "Turn the mouse wheel over a spot on the agent's desk. {ACT} x and y are \
                 screen pixels, as for desk_click. {OPEN}"
            ),
            json!({
                "x": { "type": "number", "description": "Screen pixels from the left" },
                "y": { "type": "number", "description": "Screen pixels from the top" },
                "delta": {
                    "type": "integer",
                    "description": "Wheel notches, a few lines each: positive scrolls up, negative down, 100 at most either way"
                },
            }),
            &["x", "y", "delta"],
        ),
        tool(
            "desk_apps",
            &format!(
                "What is open: the windows on the agent's desk, and the apps the user has \
                 open in their own session, which desk_open may start as a twin. {OPEN}"
            ),
            json!({}),
            &[],
        ),
        tool(
            "desk_open",
            &format!(
                "Start an app on the agent's desk: app is a name such as edge, notepad or \
                 outlook, or give path (an .exe, or shell:AppsFolder\\<id> for a Store app) \
                 with args. The user keeps working in their own session, so an app they \
                 have open there that can not run twice is started as its twin (Edge for \
                 Chrome, the new Outlook for the classic one) and the answer says which and \
                 why; one with no twin is refused with the reason. Click the new window \
                 before typing into it. For a plain web page prefer the browser_ tools. \
                 {OPEN}"
            ),
            json!({
                "app": { "type": "string", "description": "The app's name, e.g. edge, notepad, outlook" },
                "path": { "type": "string", "description": "A program's full path, in place of app" },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Arguments for path"
                },
            }),
            &[],
        ),
    ]
}

/// Answers a `desk_` tool, None for any other.
pub fn call(name: &str, args: &Value) -> Option<Result<Vec<Value>, String>> {
    is_tool(name).then(|| answer(&Desk::here(), name, args))
}

/// Everything wrong with a call is said before the desk is reached, as
/// the `remote_` tools do.
fn answer(at: &Desk, name: &str, args: &Value) -> Result<Vec<Value>, String> {
    if let Some(input) = crate::remote::input_events(name, args) {
        let (bodies, done) = input?;
        let mut sent = 0;
        return at
            .through(|link| crate::remote::post_input(link, &bodies, &mut sent))
            .map(|()| vec![crate::mcp::text(done)]);
    }
    match name {
        "desk_screenshot" => {
            let zoom = args.get("zoom").map(region).transpose()?;
            let asked = args
                .get("width")
                .and_then(remote::whole)
                .map(|w| w.clamp(64, 8192) as u32);
            // A zoom is for reading small text, so it is scaled only when
            // the call says how wide.
            let width = match zoom {
                Some(_) => asked,
                None => Some(asked.unwrap_or(remote::FRAME_WIDTH)),
            };
            let r = at.through(|link| crate::remote::frame(link, width, zoom))?;
            // The line is worded from what the hand says it answered, so
            // it is right for the region as the hand cut it to the screen.
            let line = remote::screen_line_of("desk", &r, remote::jpeg_size(&r.body));
            Ok(crate::remote::picture(&r.body, line))
        }
        "desk_apps" => {
            let r = at.through(|link| {
                let r = link
                    .get(APPS_PATH, SHORT_WAIT, remote::SMALL_CAP)
                    .map_err(Failed::Other)?;
                crate::remote::refused(&r)?;
                Ok(r)
            })?;
            if r.status != 200 {
                return Err(worded(&remote::status_refusal(r.status, &said(&r))));
            }
            apps_text(&r.body).map(|t| vec![crate::mcp::text(t)])
        }
        "desk_open" => {
            let body = launch_body(args)?;
            let r = at.through(|link| {
                let r = link
                    .post(LAUNCH_PATH, &body, SHORT_WAIT)
                    .map_err(Failed::Other)?;
                crate::remote::refused(&r)?;
                Ok(r)
            })?;
            launch_said(r.status, &said(&r)).map(|t| vec![crate::mcp::text(t)])
        }
        other => Err(format!("no tool {other}")),
    }
}

fn said(r: &Response) -> String {
    crate::remote::said(r)
}

/// A zoom's region, in screen pixels: x, y, width, height.
fn region(v: &Value) -> Result<Region, String> {
    let n = |k: &str| {
        v.get(k)
            .and_then(remote::whole)
            .ok_or_else(|| format!("zoom is x, y, width and height in screen pixels: say {k}"))
    };
    let (x, y, w, h) = (n("x")?, n("y")?, n("width")?, n("height")?);
    if w < 1 || h < 1 {
        return Err("zoom's width and height are at least 1".into());
    }
    Ok(Region {
        x,
        y,
        width: w as u32,
        height: h as u32,
    })
}

/// The body `desk_open` posts: an app by name, or a path with arguments.
fn launch_body(args: &Value) -> Result<String, String> {
    let s = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let more = match args.get("args") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(list)) => list
            .iter()
            .map(|a| a.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .ok_or("args is a list of strings")?,
        Some(_) => return Err("args is a list of strings".into()),
    };
    let body = match (s("app"), s("path")) {
        (Some(_), Some(_)) => return Err("say app or path, not both".into()),
        (Some(_), None) if !more.is_empty() => {
            return Err("args go with path: an app by name is started as it is".into())
        }
        (Some(app), None) => json!({ "app": app }),
        (None, Some(path)) if more.is_empty() => json!({ "path": path }),
        (None, Some(path)) => json!({ "path": path, "args": more }),
        (None, None) => {
            return Err(
                "say what to open: app, a name such as edge or notepad, or path".into(),
            )
        }
    };
    let body = body.to_string();
    if body.len() >= remote::BODY_MAX {
        return Err("that is more than the 64 KiB the desk reads at once".into());
    }
    Ok(body)
}

/// What the hand answered a launch with, in words. A 409 comes with the
/// reason nothing was started, which is the error.
fn launch_said(status: u16, said: &str) -> Result<String, String> {
    match status {
        200 => {
            let answer: Value = serde_json::from_str(said).unwrap_or(Value::Null);
            let s = |k: &str| {
                answer
                    .get(k)
                    .and_then(Value::as_str)
                    .map(remote::brief)
                    .filter(|s| !s.is_empty())
            };
            let Some(started) = s("started") else {
                return Ok("The desk started it, but did not say what: look with \
                           desk_screenshot."
                    .into());
            };
            let mut out = match (s("instead_of"), s("why")) {
                (Some(asked), Some(why)) => {
                    format!("Started {started} on the desk in place of {asked}: {why}")
                }
                (Some(asked), None) => format!("Started {started} on the desk in place of {asked}"),
                (None, _) => format!("Started {started} on the desk"),
            };
            if !out.ends_with('.') {
                out.push('.');
            }
            if let Some(note) = s("note") {
                out.push(' ');
                out.push_str(&note);
                if !out.ends_with('.') {
                    out.push('.');
                }
            }
            out.push_str(" Its window may take a moment: look with desk_screenshot, and click it before typing into it.");
            Ok(out)
        }
        409 => {
            let why = remote::brief(said);
            Err(if why.is_empty() {
                "the desk did not start it, and gave no reason".to_string()
            } else {
                format!("the desk did not start it: {why}")
            })
        }
        404 => Err("the desk's hand does not start apps: it may be older".into()),
        s => Err(worded(&remote::status_refusal(s, said))),
    }
}

/// `text` on one line, held to [`TITLE_MOST`] characters.
fn short(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= TITLE_MOST {
        return line;
    }
    let mut out: String = line.chars().take(TITLE_MOST).collect();
    out.push_str("...");
    out
}

/// What the hand said is open, as text: the desk's windows, then the
/// apps of the human's own session.
fn apps_text(body: &[u8]) -> Result<String, String> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|_| "the desk's hand did not say what is open in a way that reads".to_string())?;
    let windows: Vec<String> = v["desk"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|w| {
                    let app = short(w["app"].as_str().unwrap_or(""));
                    let title = short(w["title"].as_str().unwrap_or(""));
                    match (app.is_empty(), title.is_empty()) {
                        (true, true) => None,
                        (false, true) => Some(format!("- {app}")),
                        (true, false) => Some(format!("- {title:?}")),
                        (false, false) => Some(format!("- {app}: {title:?}")),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let mut out = if windows.is_empty() {
        "No window is open on the desk.".to_string()
    } else {
        let mut out = "Open on the desk:\n".to_string();
        out.push_str(&windows[..windows.len().min(WINDOWS_MOST)].join("\n"));
        if windows.len() > WINDOWS_MOST {
            out.push_str(&format!("\n({} more)", windows.len() - WINDOWS_MOST));
        }
        out
    };
    let human: Vec<String> = v["human"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(short)
                .filter(|a| !a.is_empty())
                .take(WINDOWS_MOST)
                .collect()
        })
        .unwrap_or_default();
    if !human.is_empty() {
        out.push_str(&format!(
            "\nOpen in the user's own session: {}. desk_open starts a twin on the desk for \
             one of these that can not run twice.",
            human.join(", ")
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpListener};
    use std::sync::{Arc, Mutex};

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    const OTHER: &str = "fedcba9876543210fedcba9876543210";

    /// A JPEG's first bytes, saying it is 1600 by 900.
    fn jpeg(w: u16, h: u16) -> Vec<u8> {
        let mut b = vec![0xff, 0xd8, 0xff, 0xc0, 0x00, 0x11, 0x08];
        b.extend(h.to_be_bytes());
        b.extend(w.to_be_bytes());
        b.extend([0x03, 0x01]);
        b
    }

    /// The fake hand's screen.
    const SCREEN: (u32, u32) = (3200, 1800);

    /// What the fake hand answers a frame asked with `query`, as the real
    /// one is to: the status, the headers and the picture, whose size is
    /// the one the real one would send.
    fn fake_frame(query: &str) -> (u16, String, Vec<u8>) {
        use horadric_core::screen_share as share;
        let screen = format!(
            "X-Screen-Width: {}\r\nX-Screen-Height: {}\r\n",
            SCREEN.0, SCREEN.1
        );
        let max = share::max_width(query);
        let held = match share::region(query) {
            Ok(r) => r.map(|r| r.on(SCREEN.0, SCREEN.1)),
            Err(why) => return (400, String::new(), why.into_bytes()),
        };
        match held {
            None => {
                let (w, h) = share::scaled(SCREEN.0, SCREEN.1, max);
                (200, screen, jpeg(w as u16, h as u16))
            }
            Some(None) => (
                400,
                String::new(),
                share::off_screen(SCREEN.0, SCREEN.1).into_bytes(),
            ),
            Some(Some(r)) => {
                let (w, h) = share::scaled(r.width, r.height, max);
                let region = format!(
                    "X-Region-Left: {}\r\nX-Region-Top: {}\r\nX-Region-Width: {}\r\nX-Region-Height: {}\r\n",
                    r.x, r.y, r.width, r.height
                );
                (200, screen + &region, jpeg(w as u16, h as u16))
            }
        }
    }

    /// What a fake hand was asked: each request's method and target, its
    /// body, and whether it carried a token.
    type Asked = Arc<Mutex<Vec<(String, String, bool)>>>;

    /// A hand that holds `token` and answers as the real one is to, on a
    /// port of its own. `launch` is what it answers a launch with.
    fn hand(token: &'static str, launch: (u16, &'static str)) -> (u16, Asked) {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let asked: Asked = Arc::default();
        let kept = Arc::clone(&asked);
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { return };
                let mut got = Vec::new();
                let mut buf = [0u8; 4096];
                let (head, body) = loop {
                    let text = String::from_utf8_lossy(&got).into_owned();
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("Content-Length: "))
                            .map_or(0, |n| n.parse::<usize>().unwrap());
                        if body.len() >= len {
                            break (head.to_string(), body.to_string());
                        }
                    }
                    match s.read(&mut buf) {
                        Ok(n) if n > 0 => got.extend_from_slice(&buf[..n]),
                        _ => break (String::new(), String::new()),
                    }
                };
                // The quick look connects and says nothing.
                let Some(line) = head.lines().next() else {
                    continue;
                };
                let mut words = line.split(' ');
                let (method, target) = (words.next().unwrap(), words.next().unwrap());
                let carried = head.contains("Authorization:");
                kept.lock()
                    .unwrap()
                    .push((format!("{method} {target}"), body, carried));
                let authorized = head.contains(&format!("Authorization: Bearer {token}"));
                let (path, query) = target.split_once('?').unwrap_or((target, ""));
                let framed;
                let (status, extra, answer): (u16, &str, Vec<u8>) =
                    match (method, path) {
                        ("GET", "/hello") => {
                            let nonce = target.split_once("n=").unwrap().1;
                            (200, "", remote::proof(token, nonce).into_bytes())
                        }
                        _ if !authorized => (403, "", b"forbidden".to_vec()),
                        ("GET", "/frame.jpg") => {
                            framed = fake_frame(query);
                            (framed.0, framed.1.as_str(), framed.2.clone())
                        }
                        ("POST", "/input") => (204, "", Vec::new()),
                        ("GET", "/apps") => (
                            200,
                            "",
                            br#"{"desk":[{"app":"msedge","title":"Mail"}],"human":["chrome"]}"#
                                .to_vec(),
                        ),
                        ("POST", "/launch") => (launch.0, "", launch.1.as_bytes().to_vec()),
                        _ => (404, "", Vec::new()),
                    };
                let head = format!(
                    "HTTP/1.0 {status} X\r\n{extra}Content-Length: {}\r\n\r\n",
                    answer.len()
                );
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(&answer);
            }
        });
        (port, asked)
    }

    /// A folder of this test's own, with a token file in it when `token`
    /// is one, and the session named as an open window does.
    fn desk(name: &str, port: u16, token: Option<&str>) -> Desk {
        let dir = std::env::temp_dir().join(format!(
            "horadric-desk-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(desk::TOKEN_FILE);
        if let Some(t) = token {
            std::fs::write(&file, format!("{t}\n")).unwrap();
        }
        let session = dir.join(desk::SESSION_FILE);
        std::fs::write(&session, desk::session_line(3)).unwrap();
        Desk {
            port,
            token_file: Some(file),
            session_file: Some(session),
        }
    }

    fn said_text(content: &[Value]) -> String {
        content
            .iter()
            .filter_map(|c| c["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every tool, with arguments it takes.
    fn calls() -> Vec<(&'static str, Value)> {
        vec![
            ("desk_screenshot", json!({})),
            ("desk_click", json!({ "x": 1, "y": 2 })),
            ("desk_type", json!({ "text": "x" })),
            ("desk_press", json!({ "key": "Enter" })),
            ("desk_scroll", json!({ "x": 1, "y": 2, "delta": 1 })),
            ("desk_apps", json!({})),
            ("desk_open", json!({ "app": "notepad" })),
        ]
    }

    #[test]
    fn only_desk_tools_are_answered() {
        assert!(call("remote_click", &json!({ "x": 1, "y": 2 })).is_none());
        assert!(call("desk_nope", &json!({})).is_none());
        assert!(calls().iter().all(|(name, _)| is_tool(name)));
        let listed: Vec<String> = tools()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        if cfg!(windows) {
            assert_eq!(listed.len(), calls().len());
            assert!(listed.iter().all(|n| is_tool(n)), "{listed:?}");
        } else {
            assert!(listed.is_empty());
        }
    }

    #[test]
    fn the_tools_say_what_the_desk_is_and_when_it_works() {
        for t in tools() {
            let said = t["description"].as_str().unwrap();
            assert!(said.contains("agent's desk"), "{said}");
            assert!(said.contains("Agent's desk"), "{said}");
            let name = t["name"].as_str().unwrap();
            if matches!(name, "desk_type" | "desk_press" | "desk_click" | "desk_open") {
                assert!(said.contains("before typing"), "{said}");
            }
            if matches!(name, "desk_screenshot" | "desk_open") {
                assert!(said.contains("browser_"), "{said}");
            }
        }
    }

    #[test]
    fn a_closed_desk_is_said_in_one_sentence_and_nothing_is_sent() {
        // A token but nobody on the port: the desk was open once.
        let free = {
            let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            l.local_addr().unwrap().port()
        };
        let gone = desk("gone", free, Some(TOKEN));
        // A hand on the port but no token file, or one that is no token.
        let (port, asked) = hand(TOKEN, (200, "{}"));
        let no_file = desk("no-file", port, None);
        let no_token = desk("no-token", port, Some("not a token"));
        let nowhere = Desk {
            port,
            token_file: None,
            session_file: no_file.session_file.clone(),
        };
        for at in [&gone, &no_file, &no_token, &nowhere] {
            for (name, args) in calls() {
                assert_eq!(answer(at, name, &args).unwrap_err(), CLOSED, "{name}");
            }
        }
        assert!(asked.lock().unwrap().is_empty());
    }

    #[test]
    fn a_hand_that_outlives_its_window_is_a_closed_desk() {
        // The window closed and the session stayed signed in: the hand
        // answers and would prove its token, and is asked nothing.
        let (port, asked) = hand(TOKEN, (200, "{}"));
        let mut at = desk("window-closed", port, Some(TOKEN));
        let session = at.session_file.clone().unwrap();
        assert!(answer(&at, "desk_apps", &json!({})).is_ok());
        let before = asked.lock().unwrap().len();

        std::fs::remove_file(&session).unwrap();
        for (name, args) in calls() {
            assert_eq!(answer(&at, name, &args).unwrap_err(), CLOSED, "{name}");
        }
        // A file that names no session is no open window either.
        std::fs::write(&session, "").unwrap();
        assert_eq!(answer(&at, "desk_click", &json!({ "x": 1, "y": 2 })).unwrap_err(), CLOSED);
        at.session_file = None;
        assert_eq!(answer(&at, "desk_screenshot", &json!({})).unwrap_err(), CLOSED);
        assert_eq!(asked.lock().unwrap().len(), before);
    }

    #[test]
    fn what_is_wrong_with_a_call_is_said_before_the_desk_is_reached() {
        let nowhere = Desk {
            port: 1,
            token_file: None,
            session_file: None,
        };
        let e = |name: &str, args: Value| answer(&nowhere, name, &args).unwrap_err();
        assert!(e("desk_click", json!({ "x": 1 })).contains("say y"));
        assert!(e("desk_type", json!({ "text": "" })).contains("text"));
        assert!(e("desk_press", json!({ "key": "Ctrl+Nope" })).contains("Nope"));
        assert!(e("desk_scroll", json!({ "x": 1, "y": 2, "delta": 0 })).contains("notches"));
        assert!(e("desk_open", json!({})).contains("app"));
        assert!(e("desk_screenshot", json!({ "zoom": { "x": 1 } })).contains("say y"));
    }

    #[test]
    fn a_port_that_can_not_prove_the_token_is_sent_nothing() {
        let (port, asked) = hand(OTHER, (200, "{}"));
        let at = desk("unproven", port, Some(TOKEN));
        for (name, args) in calls() {
            let e = answer(&at, name, &args).unwrap_err();
            assert!(e.contains("could not prove"), "{name}: {e}");
            assert!(e.contains("End the desk") && !e.contains("anew"), "{e}");
            assert!(!e.contains(TOKEN), "{e}");
        }
        let asked = asked.lock().unwrap();
        assert!(!asked.is_empty());
        for (what, _, carried) in asked.iter() {
            assert!(what.starts_with("GET /hello?n="), "{what}");
            assert!(!carried, "{what}");
        }
    }

    #[test]
    fn input_goes_to_a_proven_hand_as_the_remote_tools_send_it() {
        let (port, asked) = hand(TOKEN, (200, "{}"));
        let at = desk("input", port, Some(TOKEN));
        let done = |name: &str, args: Value| said_text(&answer(&at, name, &args).unwrap());
        assert_eq!(
            done("desk_click", json!({ "x": 812.4, "y": 440, "double": true })),
            "double clicked at 812,440"
        );
        assert_eq!(done("desk_type", json!({ "text": "æøå" })), "typed 3 characters");
        assert_eq!(done("desk_press", json!({ "key": "Ctrl+S" })), "pressed Ctrl+S");
        assert_eq!(
            done("desk_scroll", json!({ "x": 5, "y": 6, "delta": -3 })),
            "scrolled down 3 notches at 5,6"
        );
        let asked = asked.lock().unwrap();
        let posts: Vec<&(String, String, bool)> =
            asked.iter().filter(|(what, ..)| what == "POST /input").collect();
        assert_eq!(posts.len(), 4);
        assert!(posts.iter().all(|(_, _, carried)| *carried));
        let typed: Value = serde_json::from_str(&posts[1].1).unwrap();
        assert_eq!(typed, json!([{ "kind": "text", "text": "æøå" }]));
        // Each call proves the hand before the token goes to it.
        for pair in asked.chunks(2) {
            assert!(pair[0].0.starts_with("GET /hello?n=") && !pair[0].2, "{pair:?}");
            assert_eq!(pair[1].0, "POST /input");
        }
    }

    #[test]
    fn a_screenshot_says_the_desks_scale_and_a_zoom_is_that_region_unscaled() {
        let (port, asked) = hand(TOKEN, (200, "{}"));
        let at = desk("frame", port, Some(TOKEN));
        let shot = answer(&at, "desk_screenshot", &json!({ "width": 1600 })).unwrap();
        assert_eq!(shot[0]["type"], "image");
        assert_eq!(shot[0]["mimeType"], "image/jpeg");
        assert_eq!(
            shot[1]["text"],
            "The desk screen is 3200 x 1800 physical pixels. This image is 1600 x 900 of \
             that screen: multiply image coordinates by 2 for the x and y the desk_ input \
             tools take, counted from the screen's top left."
        );
        let zoom = json!({ "zoom": { "x": 100, "y": 50, "width": 400, "height": 300 } });
        let line = said_text(&answer(&at, "desk_screenshot", &zoom).unwrap());
        assert_eq!(
            line,
            "The desk screen is 3200 x 1800 physical pixels. This image is 400 x 300 and \
             shows only the part of that screen from 100,50 to 500,350, unscaled: to image \
             coordinates add 100 to x and 50 to y, for the x and y the desk_ input tools \
             take, counted from the screen's top left."
        );
        // Scaled only when the call says how wide, and the line is of the
        // region as the hand cut it to the screen.
        let edge = json!({
            "width": 100,
            "zoom": { "x": 3000, "y": 1700, "width": 400, "height": 300 },
        });
        let line = said_text(&answer(&at, "desk_screenshot", &edge).unwrap());
        assert!(line.contains("This image is 100 x 50 and shows"), "{line}");
        assert!(line.contains("from 3000,1700 to 3200,1800: multiply"), "{line}");
        assert!(line.contains("by 2, then add 3000 to x and 1700 to y"), "{line}");
        let off = json!({ "zoom": { "x": 5000, "y": 0, "width": 10, "height": 10 } });
        let e = answer(&at, "desk_screenshot", &off).unwrap_err();
        assert!(e.contains("not on the screen, which is 3200 x 1800"), "{e}");
        let asked = asked.lock().unwrap();
        let frames: Vec<&str> = asked
            .iter()
            .map(|(what, ..)| what.as_str())
            .filter(|what| what.contains("/frame.jpg"))
            .collect();
        assert_eq!(
            frames,
            [
                "GET /frame.jpg?w=1600",
                "GET /frame.jpg?rx=100&ry=50&rw=400&rh=300",
                "GET /frame.jpg?w=100&rx=3000&ry=1700&rw=400&rh=300",
                "GET /frame.jpg?rx=5000&ry=0&rw=10&rh=10",
            ]
        );
    }

    #[test]
    fn what_is_open_and_what_was_started_come_back_as_words() {
        let twin = r#"{"started":"msedge","instead_of":"chrome","why":"Chrome is open in your own session and hands a second start to it"}"#;
        let (port, asked) = hand(TOKEN, (200, twin));
        let at = desk("apps", port, Some(TOKEN));
        let apps = said_text(&answer(&at, "desk_apps", &json!({})).unwrap());
        assert!(apps.contains("- msedge: \"Mail\""), "{apps}");
        assert!(apps.contains("own session: chrome."), "{apps}");
        let open = json!({ "path": r"C:\x\y.exe", "args": ["--new"] });
        let started = said_text(&answer(&at, "desk_open", &open).unwrap());
        assert!(
            started.starts_with(
                "Started msedge on the desk in place of chrome: Chrome is open in your own \
                 session and hands a second start to it."
            ),
            "{started}"
        );
        let asked = asked.lock().unwrap();
        let (_, body, carried) = asked.iter().find(|(w, ..)| w == "POST /launch").unwrap();
        assert!(carried);
        assert_eq!(
            serde_json::from_str::<Value>(body).unwrap(),
            json!({ "path": r"C:\x\y.exe", "args": ["--new"] })
        );

        let (port, _) = hand(TOKEN, (409, "classic Outlook can not run twice, and has no twin"));
        let at = desk("refused", port, Some(TOKEN));
        assert_eq!(
            answer(&at, "desk_open", &json!({ "app": "outlook" })).unwrap_err(),
            "the desk did not start it: classic Outlook can not run twice, and has no twin"
        );
    }

    #[test]
    fn a_launch_is_an_app_or_a_path_with_arguments() {
        let body = |args: Value| -> Value {
            serde_json::from_str(&launch_body(&args).unwrap()).unwrap()
        };
        assert_eq!(body(json!({ "app": " edge " })), json!({ "app": "edge" }));
        assert_eq!(
            body(json!({ "path": r"shell:AppsFolder\X!App" })),
            json!({ "path": r"shell:AppsFolder\X!App" })
        );
        assert_eq!(
            body(json!({ "path": "a.exe", "args": ["-x", "y z"] })),
            json!({ "path": "a.exe", "args": ["-x", "y z"] })
        );
        assert_eq!(body(json!({ "app": "edge", "args": [] })), json!({ "app": "edge" }));
        for bad in [
            json!({}),
            json!({ "app": "" }),
            json!({ "app": "edge", "path": "a.exe" }),
            json!({ "app": "edge", "args": ["x"] }),
            json!({ "path": "a.exe", "args": [1] }),
            json!({ "path": "a.exe", "args": "x" }),
        ] {
            assert!(launch_body(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_launch_answer_says_what_started_and_why_another() {
        let ok = launch_said(200, r#"{"started":"notepad"}"#).unwrap();
        assert!(ok.starts_with("Started notepad on the desk. "), "{ok}");
        assert!(ok.contains("click it before typing"), "{ok}");
        let twin = launch_said(200, r#"{"started":"olk","instead_of":"outlook"}"#).unwrap();
        assert!(twin.starts_with("Started olk on the desk in place of outlook. "), "{twin}");
        let noted = launch_said(200, r#"{"started":"notepad","note":"Mind the tabs"}"#).unwrap();
        assert!(
            noted.starts_with("Started notepad on the desk. Mind the tabs. Its window may"),
            "{noted}"
        );
        let both = r#"{"started":"msedge","instead_of":"chrome","why":"because","note":"mind it"}"#;
        assert!(launch_said(200, both)
            .unwrap()
            .starts_with("Started msedge on the desk in place of chrome: because. mind it. Its"));
        assert!(launch_said(200, "not json").unwrap().contains("did not say what"));
        assert!(launch_said(409, "").unwrap_err().contains("gave no reason"));
        assert!(launch_said(404, "").unwrap_err().contains("older"));
        assert_eq!(launch_said(500, "broke").unwrap_err(), "the desk answered 500: broke");
    }

    #[test]
    fn what_is_open_reads_whatever_the_hand_sends() {
        let text = |body: &str| apps_text(body.as_bytes()).unwrap();
        assert_eq!(text(r#"{"desk":[],"human":[]}"#), "No window is open on the desk.");
        assert_eq!(text("{}"), "No window is open on the desk.");
        let both = text(
            r#"{"desk":[{"app":"notepad","title":"a\nb"},{"app":"code","title":""}],"human":["chrome","outlook"]}"#,
        );
        assert!(both.starts_with("Open on the desk:\n- notepad: \"a b\"\n- code\n"), "{both}");
        assert!(both.contains("own session: chrome, outlook."), "{both}");
        let long = format!(r#"{{"desk":[{{"app":"x","title":"{}"}}]}}"#, "t".repeat(500));
        assert!(text(&long).len() < 200);
        assert!(apps_text(b"<html>").is_err());
    }

    #[test]
    fn failures_speak_of_the_desk() {
        assert_eq!(
            worded(&remote::input_refusal(409, "a UAC prompt is up")),
            "the desk's Windows did not take the input: a UAC prompt is up"
        );
        assert_eq!(worded("the tunnel did not answer: x"), "the desk's hand did not answer: x");
    }
}
