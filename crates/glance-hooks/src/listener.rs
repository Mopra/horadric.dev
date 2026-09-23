//! A minimal HTTP/1.1 server for two purposes: accept `POST /glance/hook`
//! from Claude Code, and `POST /glance/new` from `glance new`.
//!
//! Hand rolled on `std::net` because the whole protocol we need is a request
//! line, a handful of headers, a `Content-Length` body and a fixed reply. A
//! framework would be more code than this file.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use glance_core::HookEvent;
use serde_json::{json, Value};

use crate::{COMMAND_HEADER, HOOK_PATH, NEW_PATH, SESSION_HEADER};

/// A hook event together with the Glance session id from the header.
#[derive(Debug, Clone)]
pub struct Tagged {
    pub glance_id: String,
    pub event: HookEvent,
}

/// A request to start a session in a Glance terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    pub name: Option<String>,
    pub cwd: String,
    /// Passed to `claude` as they are.
    pub args: Vec<String>,
}

impl NewSession {
    pub fn to_json(&self) -> String {
        json!({ "name": self.name, "cwd": self.cwd, "args": self.args }).to_string()
    }

    pub fn from_json(body: &[u8]) -> Option<Self> {
        let v: Value = serde_json::from_slice(body).ok()?;
        let cwd = v.get("cwd")?.as_str()?.to_string();
        let name = v.get("name").and_then(Value::as_str).map(str::to_string);
        let args = match v.get("args") {
            None | Some(Value::Null) => Vec::new(),
            Some(a) => a
                .as_array()?
                .iter()
                .map(|x| x.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()?,
        };
        Some(NewSession { name, cwd, args })
    }
}

/// Starts listening on 127.0.0.1 and forwards every tagged event on `tx`,
/// and every new session request on `new` when there is one.
///
/// Runs on its own thread and never returns unless the socket fails. Requests
/// without a session header are answered 200 and dropped: that is a `claude`
/// running outside Glance, and it must never be slowed down or shown an error.
pub fn serve(port: u16, tx: Sender<Tagged>, new: Option<Sender<NewSession>>) -> io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let tx = tx.clone();
        let new = new.clone();
        thread::spawn(move || {
            // Claude Code waits for the hook to finish. A slow reply is a
            // slow agent, so every path here answers fast and gives up fast.
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
            let _ = handle(stream, &tx, new.as_ref());
        });
    }
    Ok(())
}

fn handle(
    mut stream: TcpStream,
    tx: &Sender<Tagged>,
    new: Option<&Sender<NewSession>>,
) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    let mut content_length = 0usize;
    let mut glance_id = String::new();
    let mut command = String::new();
    let mut from_browser = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();
            match name.as_str() {
                "content-length" => content_length = value.parse().unwrap_or(0),
                n if n == SESSION_HEADER => glance_id = value.to_string(),
                n if n == COMMAND_HEADER => command = value.to_string(),
                "origin" => from_browser = true,
                _ => {}
            }
        }
    }

    if method != "POST" || (path != HOOK_PATH && path != NEW_PATH) {
        return respond(&mut stream, "404 Not Found");
    }
    // A megabyte is far more than any hook payload. Anything bigger is not
    // Claude Code and is not worth reading.
    if content_length > 1 << 20 {
        return respond(&mut stream, "413 Payload Too Large");
    }

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;

    if path == NEW_PATH {
        // Starting a process is the one thing a web page must never reach.
        if from_browser || command != "new" {
            return respond(&mut stream, "403 Forbidden");
        }
        let Some(new) = new else {
            return respond(&mut stream, "503 Service Unavailable");
        };
        let Some(request) = NewSession::from_json(&body) else {
            return respond(&mut stream, "400 Bad Request");
        };
        let _ = new.send(request);
        return respond(&mut stream, "200 OK");
    }

    // Reply before parsing. The agent should not wait on us for anything.
    respond(&mut stream, "200 OK")?;

    if glance_id.is_empty() {
        return Ok(());
    }
    if let Ok(event) = HookEvent::from_json(&body) {
        let _ = tx.send(Tagged { glance_id, event });
    }
    Ok(())
}

fn respond(stream: &mut TcpStream, status: &str) -> io::Result<()> {
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
    );
    stream.write_all(reply.as_bytes())?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;
    use std::sync::mpsc;

    fn post(port: u16, headers: &str, body: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req = format!(
            "POST {HOOK_PATH} HTTP/1.1\r\nHost: x\r\n{headers}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    fn start() -> (u16, mpsc::Receiver<Tagged>) {
        // Bind to port 0 to find a free one, then hand it to serve().
        let probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || serve(port, tx, None));
        // Wait until it accepts.
        for _ in 0..50 {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        (port, rx)
    }

    #[test]
    fn tagged_post_is_forwarded() {
        let (port, rx) = start();
        let reply = post(
            port,
            "X-Glance-Session: tile-7\r\n",
            r#"{"session_id":"c","hook_event_name":"Stop"}"#,
        );
        assert!(reply.starts_with("HTTP/1.1 200"));
        let got = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(got.glance_id, "tile-7");
        assert_eq!(got.event.hook_event_name, "Stop");
    }

    #[test]
    fn untagged_post_is_accepted_and_dropped() {
        let (port, rx) = start();
        let reply = post(port, "", r#"{"session_id":"c","hook_event_name":"Stop"}"#);
        assert!(reply.starts_with("HTTP/1.1 200"));
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    fn start_with_new() -> (u16, mpsc::Receiver<NewSession>) {
        let probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let (tx, _rx) = mpsc::channel();
        let (new_tx, new_rx) = mpsc::channel();
        thread::spawn(move || serve(port, tx, Some(new_tx)));
        for _ in 0..50 {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        (port, new_rx)
    }

    fn post_new(port: u16, headers: &str, body: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req = format!(
            "POST {NEW_PATH} HTTP/1.1\r\nHost: x\r\n{headers}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    #[test]
    fn new_session_request_is_forwarded() {
        let (port, rx) = start_with_new();
        let want = NewSession {
            name: Some("fix-login".into()),
            cwd: "C:/dev/app".into(),
            args: vec!["--model".into(), "haiku".into()],
        };
        let reply = post_new(port, "X-Glance-Command: new\r\n", &want.to_json());
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), want);
    }

    #[test]
    fn new_session_needs_the_header_and_no_origin() {
        let (port, rx) = start_with_new();
        let body = r#"{"cwd":"C:/x"}"#;
        assert!(post_new(port, "", body).starts_with("HTTP/1.1 403"));
        let browser = "X-Glance-Command: new\r\nOrigin: https://example.com\r\n";
        assert!(post_new(port, browser, body).starts_with("HTTP/1.1 403"));
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn new_session_without_the_app_is_unavailable() {
        let (port, _rx) = start();
        let reply = post_new(port, "X-Glance-Command: new\r\n", r#"{"cwd":"C:/x"}"#);
        assert!(reply.starts_with("HTTP/1.1 503"), "{reply}");
    }

    #[test]
    fn new_session_json_tolerates_missing_optionals() {
        let n = NewSession::from_json(br#"{"cwd":"C:/x"}"#).unwrap();
        assert_eq!(n.name, None);
        assert!(n.args.is_empty());
        assert_eq!(NewSession::from_json(br#"{"name":"x"}"#), None);
    }

    #[test]
    fn wrong_path_is_404() {
        let (port, _rx) = start();
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(b"GET /nope HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 404"));
    }
}
