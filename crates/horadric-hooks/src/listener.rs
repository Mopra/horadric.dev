//! A minimal HTTP/1.1 server for nine purposes: accept `POST /horadric/hook`
//! from Claude Code, `POST /horadric/status` from its status line,
//! `POST /horadric/new` from `horadric new`, `POST /horadric/reload` from
//! `horadric reload`, `POST /horadric/tasks` from `horadric quest`,
//! `POST /horadric/browser`, `POST /horadric/recall` and
//! `POST /horadric/glow` from `horadric mcp` and `POST /horadric/know` from
//! Horadric's Claude mod. The browser, recall and the mod's are the ones
//! that wait for the app's answer, and so does a Claude Code prompt's
//! hook, which the memory answers with what it recalls on the prompt.
//!
//! Hand rolled on `std::net` because the whole protocol we need is a request
//! line, a handful of headers, a `Content-Length` body and a fixed reply. A
//! framework would be more code than this file.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime};

use horadric_core::overlap::{self, Claims, Overlap};
use horadric_core::spend::uptake;
use horadric_core::{HookEvent, Limits, Status};
use serde_json::{json, Value};

use crate::{
    checkpoint, client, otel, pace, transcript, BROWSER_PATH, COMMAND_HEADER, GLOW_PATH,
    HEAVY_PATH, HOOK_PATH, KNOW_PATH, NEW_PATH, OWNER_HEADER, RECALL_PATH, RELOAD_PATH,
    SESSION_HEADER, STATE_HEADER, STATUS_PATH, TASKS_PATH,
};

/// A hook event together with the Horadric session id from the header.
#[derive(Debug, Clone)]
pub struct Tagged {
    pub horadric_id: String,
    pub event: HookEvent,
    /// The account's limits, when the event brought them, as a status
    /// line's do.
    pub limits: Option<Limits>,
}

/// A request to start a session in a Horadric terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    pub name: Option<String>,
    pub cwd: String,
    /// Passed to the agent as they are.
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
        // An older `horadric new` could ask for another agent, which is
        // not started as Claude.
        match v.get("agent") {
            None | Some(Value::Null) => {}
            Some(a) if a.as_str()?.eq_ignore_ascii_case("claude") => {}
            Some(_) => return None,
        }
        Some(NewSession { name, cwd, args })
    }
}

/// A request to swap the running app for a new build and carry on with the
/// same sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reload {
    /// The `horadric.exe` to reload into. `horadricw.exe` sits beside it.
    pub exe: String,
    /// Skip waiting for sessions to finish their turn.
    pub now: bool,
}

impl Reload {
    pub fn to_json(&self) -> String {
        json!({ "exe": self.exe, "now": self.now }).to_string()
    }

    pub fn from_json(body: &[u8]) -> Option<Self> {
        let v: Value = serde_json::from_slice(body).ok()?;
        Some(Reload {
            exe: v.get("exe")?.as_str()?.to_string(),
            now: v.get("now").and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

/// `horadric quest` changed the task list of the project in this folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TasksChanged {
    pub dir: String,
    /// The quest whose session Warriv answers, with `tell`.
    pub quest: Option<String>,
    /// What Warriv tells that quest's session, typed in between turns.
    pub tell: Option<String>,
    /// What a review found wrong with that quest, which sends it back.
    pub fix: Option<String>,
    /// A stone to cast, from `horadric runeword cast`.
    pub cast: Option<String>,
    /// What the quest just filed, `quest`, shares with others, for Warriv.
    pub overlap: Option<String>,
    /// The session that ran the command, so the app knows Warriv's own.
    pub by: Option<String>,
}

impl TasksChanged {
    pub fn to_json(&self) -> String {
        json!({
            "dir": self.dir,
            "quest": self.quest,
            "tell": self.tell,
            "fix": self.fix,
            "cast": self.cast,
            "overlap": self.overlap,
            "by": self.by,
        })
        .to_string()
    }

    pub fn from_json(body: &[u8]) -> Option<Self> {
        let v: Value = serde_json::from_slice(body).ok()?;
        let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        Some(TasksChanged {
            dir: v.get("dir")?.as_str()?.to_string(),
            quest: text("quest"),
            tell: text("tell"),
            fix: text("fix"),
            cast: text("cast"),
            overlap: text("overlap"),
            by: text("by"),
        })
    }
}

/// An agent's call on its project's browser pane, from `horadric mcp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserCall {
    /// The session that asked, whose project's page it is.
    pub session: String,
    /// What it asked, as `horadric mcp` wrote it.
    pub body: Value,
    pub reply: Reply,
}

/// Horadric's Claude mod asks what Horadric knows about its session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowCall {
    pub session: String,
    /// The files it shares with other sessions, which only the listener
    /// knows, by session id.
    pub overlaps: Vec<Overlap>,
    pub reply: Reply,
}

/// A `lore_recall` from `horadric mcp`, for the app's memory. Any agent's
/// may ask, Horadric's session or not: the notes are the project's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallCall {
    /// What was asked, as `horadric mcp` wrote it.
    pub body: Value,
    pub reply: Reply,
}

/// Where the app sends its answer, a JSON body. It is not part of what was
/// asked, so any two are equal.
#[derive(Debug, Clone)]
pub struct Reply {
    to: Sender<String>,
    /// Set when the caller stopped waiting.
    gone: Arc<AtomicBool>,
}

impl Reply {
    fn new() -> (Reply, Receiver<String>) {
        let (to, rx) = std::sync::mpsc::channel();
        let gone = Arc::new(AtomicBool::new(false));
        (Reply { to, gone }, rx)
    }

    pub fn send(&self, body: Value) {
        let _ = self.to.send(body.to_string());
    }

    /// Whether the caller still waits. One that gave up was told the call
    /// failed and may make it again, so a call not begun by then must not
    /// begin late: it would act twice.
    pub fn wanted(&self) -> bool {
        !self.gone.load(Ordering::Relaxed)
    }

    fn give_up(&self) {
        self.gone.store(true, Ordering::Relaxed);
    }
}

impl PartialEq for Reply {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Reply {}

/// The longest an agent waits on its browser: a page that is slow to load,
/// or a script that runs long. A test's app may start a process before it
/// answers, which takes over a second on a busy CI runner.
const BROWSER_WAIT: Duration = Duration::from_secs(if cfg!(test) { 5 } else { 90 });

/// The mod asks again in a few seconds, so an app busy for longer than
/// this is not worth waiting on.
const KNOW_WAIT: Duration = Duration::from_secs(2);

/// A recall waits on the model, which may be embedding notes it has not
/// seen. Past this the server answers by words itself.
const RECALL_WAIT: Duration = Duration::from_secs(if cfg!(test) { 1 } else { 10 });

/// A recall on a prompt holds the human's prompt up, and Claude Code
/// gives the hook five seconds in all: past this the prompt goes on
/// without it.
pub(crate) const PROMPT_WAIT: Duration = Duration::from_secs(3);

/// The most of a prompt a recall reads: a pasted log is not a question,
/// and its first lines say what it is about.
const PROMPT_CHARS: usize = 2000;

/// What the command line can ask the running app for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    New(NewSession),
    Reload(Reload),
    Tasks(TasksChanged),
    /// A session edited a file another session changed and has not
    /// committed, in the same working tree.
    Overlap(Overlap),
    Browser(BrowserCall),
    Know(KnowCall),
    Recall(RecallCall),
}

/// Starts listening on 127.0.0.1 and forwards every tagged event on `tx`,
/// and every command on `commands` when there is an app to take them.
///
/// Runs on its own thread and never returns unless the socket fails. Requests
/// without a session header are answered 200 and dropped: that is a `claude`
/// running outside Horadric, and it must never be slowed down or shown an error.
/// Events owned by a Horadric on another port are passed on to it.
pub fn serve(port: u16, tx: Sender<Tagged>, commands: Option<Sender<Command>>) -> io::Result<()> {
    let listener = bind(port)?;
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let tx = tx.clone();
        let commands = commands.clone();
        thread::spawn(move || {
            // Claude Code waits for the hook to finish. A slow reply is a
            // slow agent, so every path here answers fast and gives up fast.
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
            let _ = handle(stream, port, &tx, commands.as_ref());
        });
    }
    Ok(())
}

/// Binds, retrying for a few seconds. After a reload the Horadric before
/// held the port until a moment ago.
fn bind(port: u16) -> io::Result<TcpListener> {
    let mut tries = 0;
    loop {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => return Ok(l),
            Err(e) if tries >= 50 => return Err(e),
            Err(_) => {
                tries += 1;
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn handle(
    stream: TcpStream,
    port: u16,
    tx: &Sender<Tagged>,
    commands: Option<&Sender<Command>>,
) -> io::Result<()> {
    // Read through a borrow, never a `try_clone`: on Windows the clone is
    // an inheritable handle, so a git or a session the app starts meanwhile
    // keeps the connection open after the reply. The caller then waits on
    // it, or is reset when that child exits, though its call ran.
    let mut reader = BufReader::new(&stream);

    let Some(request_line) = read_capped(&mut reader)? else {
        return respond(&stream, "431 Request Header Fields Too Large");
    };
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    let mut content_length = 0usize;
    let mut horadric_id = String::new();
    let mut owner = None;
    let mut command = String::new();
    let mut state = String::new();
    let mut from_browser = false;
    let mut foreign_host = false;
    let mut count = 0;
    loop {
        count += 1;
        if count > MAX_HEADERS {
            return respond(&stream, "431 Request Header Fields Too Large");
        }
        let Some(line) = read_capped(&mut reader)? else {
            return respond(&stream, "431 Request Header Fields Too Large");
        };
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();
            match name.as_str() {
                "content-length" => content_length = value.parse().unwrap_or(0),
                n if n == SESSION_HEADER => horadric_id = value.to_string(),
                n if n == OWNER_HEADER => owner = value.parse::<u16>().ok(),
                n if n == COMMAND_HEADER => command = value.to_string(),
                n if n == STATE_HEADER => state = value.to_string(),
                "host" => foreign_host = !local_host(value, port),
                n if n == "origin" || n.starts_with("sec-fetch-") => from_browser = true,
                _ => {}
            }
        }
    }

    // Every path, hooks included: a page can post a hook with a no-cors
    // fetch that needs no preflight, and Claude Code, its exporter and
    // Horadric's own client never send an Origin. A Host that is not this
    // machine is a page whose name was rebound to 127.0.0.1.
    if from_browser || foreign_host {
        return respond(&stream, "403 Forbidden");
    }

    let paths = [
        HOOK_PATH,
        STATUS_PATH,
        NEW_PATH,
        RELOAD_PATH,
        TASKS_PATH,
        BROWSER_PATH,
        KNOW_PATH,
        GLOW_PATH,
        RECALL_PATH,
        HEAVY_PATH,
        otel::LOGS_PATH,
        otel::METRICS_PATH,
    ];
    if method != "POST" || !paths.contains(&path) {
        return respond(&stream, "404 Not Found");
    }
    // A megabyte is far more than any hook payload. Anything bigger is not
    // Claude Code and is not worth reading.
    if content_length > 1 << 20 {
        return respond(&stream, "413 Payload Too Large");
    }

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;

    if path == STATUS_PATH {
        // Like a hook: answered at once, dropped without a session. Always
        // posted straight to the owner, so there is nothing to pass on.
        respond(&stream, "200 OK")?;
        if let (false, Some(status)) = (horadric_id.is_empty(), Status::from_json(&body)) {
            // The id lets the tile tell its own status line from one a
            // background session sends under its tag.
            let session_id = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("session_id")?.as_str().map(str::to_string))
                .unwrap_or_default();
            let limits = Some(status.limits.clone()).filter(|l| !l.is_empty());
            let event = HookEvent {
                status: Some(status),
                session_id,
                ..HookEvent::synthetic(HookEvent::STATUS)
            };
            let _ = tx.send(Tagged {
                horadric_id,
                event,
                limits,
            });
        }
        return Ok(());
    }

    if path == otel::LOGS_PATH || path == otel::METRICS_PATH {
        if from_browser {
            return respond(&stream, "403 Forbidden");
        }
        // An empty object is a full success to an OTLP exporter.
        respond(&stream, "200 OK")?;
        if path == otel::LOGS_PATH && !horadric_id.is_empty() {
            spent(&horadric_id, &body, tx);
        }
        return Ok(());
    }

    if path == KNOW_PATH {
        // Read only, but what sessions are doing is not a web page's.
        if from_browser || horadric_id.is_empty() {
            return respond(&stream, "403 Forbidden");
        }
        let Some(commands) = commands else {
            return respond(&stream, "503 Service Unavailable");
        };
        return know(&stream, horadric_id, commands);
    }

    if path != HOOK_PATH {
        // Starting a process is the one thing a web page must never reach.
        let wanted = match path {
            NEW_PATH => "new",
            TASKS_PATH => "tasks",
            BROWSER_PATH => "browser",
            GLOW_PATH => "glow",
            RECALL_PATH => "recall",
            HEAVY_PATH => "heavy",
            _ => "reload",
        };
        if from_browser || command != wanted {
            return respond(&stream, "403 Forbidden");
        }
        let ours = crate::state_header();
        if !crate::same_state(&ours, &state) {
            let why = crate::refusal(port, &ours, &state);
            return respond_with(
                &stream,
                "409 Conflict",
                &json!({ "error": why }).to_string(),
            );
        }
        if path == GLOW_PATH {
            return glow(&stream, horadric_id, &body, tx, HookEvent::GLOW);
        }
        if path == HEAVY_PATH {
            return glow(&stream, horadric_id, &body, tx, HookEvent::HEAVY);
        }
        let Some(commands) = commands else {
            return respond(&stream, "503 Service Unavailable");
        };
        if path == BROWSER_PATH {
            return browser(&stream, horadric_id, &body, commands);
        }
        if path == RECALL_PATH {
            return recall(&stream, &body, commands);
        }
        let request = match path {
            NEW_PATH => NewSession::from_json(&body).map(Command::New),
            TASKS_PATH => TasksChanged::from_json(&body).map(Command::Tasks),
            _ => Reload::from_json(&body).map(Command::Reload),
        };
        let Some(request) = request else {
            return respond(&stream, "400 Bad Request");
        };
        let _ = commands.send(request);
        return respond(&stream, "200 OK");
    }

    // Parsing is all that happens before the reply, which tells an agent
    // that just edited a file another session is still changing. A
    // prompt's reply waits for the memory instead, after the event is
    // handed on, so the tile turns to working at once.
    let parsed = HookEvent::from_json(&body).ok().and_then(sane);
    // A turn's checkpoint is taken before the agent hears back, so its
    // first edit cannot come before it. Only the app keeps them, and only
    // for sessions it holds: an event passed on is the owner's to keep.
    if let (Some(event), Some(_), false) = (&parsed, commands, owner.is_some_and(|o| o != port)) {
        checkpoint::hear(&horadric_id, event);
    }
    let prompt = parsed
        .as_ref()
        .filter(|_| !horadric_id.is_empty())
        .and_then(|e| prompt_recall(&horadric_id, e, &body));
    let overlap = match &parsed {
        Some(event) => claims()
            .lock()
            .ok()
            .and_then(|mut c| c.hear(&horadric_id, event, SystemTime::now())),
        None => None,
    };
    let known = match (&overlap, &parsed, commands) {
        (None, Some(event), Some(commands)) => before_search(&horadric_id, event, commands),
        _ => None,
    };
    let wide = parsed
        .as_ref()
        .filter(|_| !horadric_id.is_empty())
        .and_then(|e| pace().lock().ok()?.hear(&horadric_id, e));
    let known = match (known, wide) {
        (Some(k), Some(w)) => Some(format!("{k}\n\n{w}")),
        (k, w) => k.or(w),
    };
    // A prompt is answered once, below; an overlap only comes of an edit,
    // and what the lore knows only before a search.
    match (&overlap, known, &prompt) {
        (_, _, Some(_)) => {}
        (Some(o), _, None) => {
            respond_with(&stream, "200 OK", &overlap::reply(&overlap::context(o)))?
        }
        (None, Some(text), None) => {
            respond_with(&stream, "200 OK", &overlap::reply_to("PreToolUse", &text))?
        }
        (None, None, None) => respond(&stream, "200 OK")?,
    }
    if let (Some(o), Some(commands)) = (overlap, commands) {
        let _ = commands.send(Command::Overlap(o));
    }

    // Untagged events are kept too: a background session whose daemon was
    // started outside Horadric has no tag, and the app asks Claude Code
    // whether a conversation is one of those before it shows anything.
    if let Some(owner) = owner.filter(|&o| o != port && !horadric_id.is_empty()) {
        // Without the owner header the other Horadric keeps it, so this can
        // not bounce back. Nobody listening there means the event is lost,
        // which is what it would be without the hop.
        let body = String::from_utf8_lossy(&body);
        let headers = [(SESSION_HEADER, horadric_id.as_str())];
        if prompt.is_none() {
            let _ = client::post(owner, HOOK_PATH, &headers, &body);
            return Ok(());
        }
        // The owner's memory answers its own session's prompt.
        let answer = client::ask(owner, HOOK_PATH, &headers, &body, PROMPT_WAIT)
            .ok()
            .filter(|(status, _)| *status == 200);
        return match answer {
            Some((_, reply)) => respond_with(&stream, "200 OK", &reply),
            None => respond(&stream, "200 OK"),
        };
    }
    if let Some(mut event) = parsed {
        if event.may_retitle() {
            event.title = transcript::title(&event.transcript_path);
            event.title = transcript::best_title(event.title.take(), &event.session_id);
        } else if event.may_peek_title() {
            event.title = transcript::tail_title(std::path::Path::new(&event.transcript_path));
            event.title = transcript::best_title(event.title.take(), &event.session_id);
        }
        let _ = tx.send(Tagged {
            horadric_id,
            event,
            limits: None,
        });
    }
    match (prompt, commands) {
        (Some(ask), Some(commands)) => offer(&stream, ask, commands),
        (Some(_), None) => respond(&stream, "200 OK"),
        (None, _) => Ok(()),
    }
}

/// The event as far as it may be believed, or None when it is no event of
/// Claude Code's. Its session id names files and its transcript is read,
/// so an id that is not a plain name is dropped and a transcript that is
/// not a local `.jsonl` is forgotten; everything after reads the paths
/// from here, the memory keeper and the folio's watch of a subagent's
/// transcript included.
fn sane(mut event: HookEvent) -> Option<HookEvent> {
    let plain = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    if event.session_id.len() > 128 || !event.session_id.chars().all(plain) {
        return None;
    }
    if !event.transcript_path.is_empty() && transcript::trusted(&event.transcript_path).is_none() {
        event.transcript_path.clear();
    }
    if event
        .agent_transcript_path
        .as_deref()
        .is_some_and(|p| !p.is_empty() && transcript::trusted(p).is_none())
    {
        event.agent_transcript_path = None;
    }
    Some(event)
}

/// What the memory is asked on a Claude Code prompt of session
/// `horadric_id`, or None when `event` is no prompt worth a recall: a
/// slash command is an order to Claude Code, not a question. Claude Code
/// names the text `prompt`, which [`HookEvent`] does not read.
fn prompt_recall(horadric_id: &str, event: &HookEvent, body: &[u8]) -> Option<Value> {
    if event.hook_event_name != "UserPromptSubmit" {
        return None;
    }
    let raw: Value = serde_json::from_slice(body).ok()?;
    let text = raw.get("prompt")?.as_str()?.trim();
    // `/compact` is a command, `/src/a.rs is broken` a path.
    let first = text.split_whitespace().next().unwrap_or("");
    let command = first.starts_with('/') && !first[1..].contains('/');
    // A background task reporting back is the harness talking, not a question.
    if text.is_empty() || command || text.starts_with("<task-notification>") {
        return None;
    }
    let query: String = text.chars().take(PROMPT_CHARS).collect();
    Some(json!({
        "dir": event.cwd,
        "query": query,
        "prompt": true,
        "strong": true,
        "session": horadric_id,
        "agent_session": event.session_id,
    }))
}

/// Answers a prompt's hook with what the memory recalls on it, or with
/// nothing when it recalls nothing close or takes too long.
fn offer(stream: &TcpStream, ask: Value, commands: &Sender<Command>) -> io::Result<()> {
    let said = |k: &str| ask[k].as_str().unwrap_or("").to_string();
    let (session, conversation) = (said("session"), said("agent_session"));
    let (reply, rx) = Reply::new();
    let gone = Arc::clone(&reply.gone);
    let call = RecallCall { body: ask, reply };
    if commands.send(Command::Recall(call)).is_err() {
        return respond(stream, "200 OK");
    }
    let text = match rx.recv_timeout(PROMPT_WAIT) {
        Ok(answer) => serde_json::from_str::<Value>(&answer)
            .ok()
            .and_then(|v| v.get("text")?.as_str().map(str::to_string))
            .unwrap_or_default(),
        Err(_) => {
            gone.store(true, Ordering::Relaxed);
            String::new()
        }
    };
    if text.is_empty() {
        return respond(stream, "200 OK");
    }
    respond_with(
        stream,
        "200 OK",
        &overlap::reply_to("UserPromptSubmit", &text),
    )?;
    crate::given::keep(uptake::PROMPT, &session, &conversation, &text);
    Ok(())
}

/// The longest a search is held for what the lore knows. The tool waits on
/// it, so a memory that is busy is not worth waiting on.
const SEARCH_WAIT: Duration = Duration::from_millis(if cfg!(test) { 300 } else { 1500 });

/// The most of the lore's answer put before an agent that only searched.
const SEARCH_CONTEXT: usize = 1200;

/// The name a Grep or Glob looks for, when it is a name: an identifier,
/// maybe with `::` or `.` in it, of at least three characters. A pattern
/// that is a real regex or a path wildcard says nothing the lore can match.
pub fn searched_name(tool: &str, input: &Value) -> Option<String> {
    let raw = match tool {
        "Grep" => input.get("pattern")?.as_str()?.to_string(),
        // `**/overlap*.rs` looks for `overlap`: the file's own name.
        "Glob" => {
            let file = input.get("pattern")?.as_str()?.rsplit(['/', '\\']).next()?;
            file.split('.').next()?.replace('*', "")
        }
        _ => return None,
    };
    let name = raw.trim().trim_start_matches("\\b").trim_end_matches("\\b");
    let name = name.trim_end_matches('(');
    let shaped = name.len() >= 3
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || "_:.-".contains(c))
        && name.chars().any(char::is_alphabetic);
    shaped.then(|| name.to_string())
}

/// Names a session was told about already, so a second search for one
/// brings nothing new.
fn told() -> &'static Mutex<std::collections::HashSet<(String, String)>> {
    static TOLD: OnceLock<Mutex<std::collections::HashSet<(String, String)>>> = OnceLock::new();
    TOLD.get_or_init(Default::default)
}

/// Before an agent greps or globs for a name, what the lore knows of it
/// (where it is defined, notes naming it), so the next search is not
/// needed. None when it knows nothing strong, or the memory was slow.
fn before_search(session: &str, event: &HookEvent, commands: &Sender<Command>) -> Option<String> {
    if event.hook_event_name != "PreToolUse" || event.cwd.is_empty() {
        return None;
    }
    let name = searched_name(event.tool_name.as_deref()?, event.tool_input.as_ref()?)?;
    if !told()
        .lock()
        .ok()?
        .insert((session.to_string(), name.to_lowercase()))
    {
        return None;
    }
    let (reply, rx) = Reply::new();
    let gone = Arc::clone(&reply.gone);
    let body = json!({ "dir": event.cwd, "query": name, "strong": true });
    commands
        .send(Command::Recall(RecallCall { body, reply }))
        .ok()?;
    let answer = rx.recv_timeout(SEARCH_WAIT).inspect_err(|_| {
        gone.store(true, Ordering::Relaxed);
    });
    let answer: Value = serde_json::from_str(&answer.ok()?).ok()?;
    let text = answer.get("text")?.as_str()?;
    if text.starts_with("Nothing matches") {
        return None;
    }
    let mut text: String = text.chars().take(SEARCH_CONTEXT).collect();
    if text.len() < answer["text"].as_str()?.len() {
        text.push_str(" ...");
    }
    crate::given::keep(uptake::SEARCH, session, &event.session_id, &text);
    Some(format!(
        "Horadric: the lore knows this of `{name}` already:\n{text}"
    ))
}

/// Hands news of a session that is not a hook to the app as Horadric's
/// own event `name`: what a lore tool showed it ([`HookEvent::GLOW`]) or
/// that its `horadric heavy` waits ([`HookEvent::HEAVY`]). Nothing waits
/// on it: it is news, not work.
fn glow(
    stream: &TcpStream,
    session: String,
    body: &[u8],
    tx: &Sender<Tagged>,
    name: &str,
) -> io::Result<()> {
    let Ok(input) = serde_json::from_slice::<Value>(body) else {
        return respond(stream, "400 Bad Request");
    };
    if session.is_empty() {
        return respond(stream, "400 Bad Request");
    }
    respond(stream, "200 OK")?;
    let event = HookEvent {
        tool_input: Some(input),
        ..HookEvent::synthetic(name)
    };
    let _ = tx.send(Tagged {
        horadric_id: session,
        event,
        limits: None,
    });
    Ok(())
}

/// Hands an agent's browser call to the app and answers with what the app
/// answers. Only a session's: the page is its project's.
fn browser(
    stream: &TcpStream,
    session: String,
    body: &[u8],
    commands: &Sender<Command>,
) -> io::Result<()> {
    let Ok(body) = serde_json::from_slice::<Value>(body) else {
        return respond(stream, "400 Bad Request");
    };
    if session.is_empty() {
        return respond(stream, "400 Bad Request");
    }
    let (reply, rx) = Reply::new();
    let call = BrowserCall {
        session,
        body,
        reply: reply.clone(),
    };
    if commands.send(Command::Browser(call)).is_err() {
        return respond(stream, "503 Service Unavailable");
    }
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    match rx.recv_timeout(BROWSER_WAIT) {
        Ok(answer) => respond_with(stream, "200 OK", &answer),
        Err(_) => {
            reply.give_up();
            respond(stream, "504 Gateway Timeout")
        }
    }
}

/// Hands a recall to the app and answers with what the app answers.
fn recall(stream: &TcpStream, body: &[u8], commands: &Sender<Command>) -> io::Result<()> {
    let Ok(body) = serde_json::from_slice::<Value>(body) else {
        return respond(stream, "400 Bad Request");
    };
    let (reply, rx) = Reply::new();
    // The flag, not the reply, is kept: an app that drops the call (the
    // Mac has no memory yet) closes the reply, and the caller hears at once.
    let gone = Arc::clone(&reply.gone);
    if commands
        .send(Command::Recall(RecallCall { body, reply }))
        .is_err()
    {
        return respond(stream, "503 Service Unavailable");
    }
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    match rx.recv_timeout(RECALL_WAIT) {
        Ok(answer) => respond_with(stream, "200 OK", &answer),
        Err(_) => {
            // The server answers by words now; a recall still queued
            // behind others is not worth the memory's time.
            gone.store(true, Ordering::Relaxed);
            respond(stream, "504 Gateway Timeout")
        }
    }
}

/// Hands the mod's question to the app with the files its session shares,
/// and answers with what the app answers.
fn know(stream: &TcpStream, session: String, commands: &Sender<Command>) -> io::Result<()> {
    let overlaps = shared_files(&session);
    let (reply, rx) = Reply::new();
    let call = KnowCall {
        session,
        overlaps,
        reply,
    };
    if commands.send(Command::Know(call)).is_err() {
        return respond(stream, "503 Service Unavailable");
    }
    match rx.recv_timeout(KNOW_WAIT) {
        Ok(answer) => respond_with(stream, "200 OK", &answer),
        Err(_) => respond(stream, "504 Gateway Timeout"),
    }
}

/// The files `session` shares with other sessions, as `/horadric/know`
/// would answer. A Folio's band asks here, in the process that serves the
/// listener, rather than over the port.
pub fn shared_files(session: &str) -> Vec<Overlap> {
    claims()
        .lock()
        .map(|c| c.shared(session, SystemTime::now()))
        .unwrap_or_default()
}

/// Every session's turn as [`pace::Pace`] counts it.
fn pace() -> &'static Mutex<pace::Pace> {
    static PACE: OnceLock<Mutex<pace::Pace>> = OnceLock::new();
    PACE.get_or_init(Default::default)
}

/// Who changed which file, across every session this listener hears.
fn claims() -> &'static Mutex<Claims> {
    static CLAIMS: OnceLock<Mutex<Claims>> = OnceLock::new();
    CLAIMS.get_or_init(Default::default)
}

/// Passes on what an OTLP export says the session tagged `horadric_id`
/// spent, one event per conversation in it, which picks its tile as a
/// status line's id does.
fn spent(horadric_id: &str, body: &[u8], tx: &Sender<Tagged>) {
    let mut by: Vec<(String, Vec<horadric_core::usage::Spent>)> = Vec::new();
    for (session, s) in otel::api_requests(body) {
        match by.iter_mut().find(|(id, _)| *id == session) {
            Some((_, list)) => list.push(s),
            None => by.push((session, vec![s])),
        }
    }
    for (session_id, spent) in by {
        let event = HookEvent {
            session_id,
            spent,
            ..HookEvent::synthetic(HookEvent::SPENT)
        };
        let _ = tx.send(Tagged {
            horadric_id: horadric_id.to_string(),
            event,
            limits: None,
        });
    }
}

/// Longer than any header Claude Code or Horadric sends; a line past it is
/// not read on, so a client cannot make a thread hold megabytes.
const MAX_LINE: u64 = 8 * 1024;
const MAX_HEADERS: usize = 64;

/// One line of the head, or None when it runs past [`MAX_LINE`]. Empty at
/// the end of the stream.
fn read_capped(reader: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut line = String::new();
    let read = reader.take(MAX_LINE).read_line(&mut line)?;
    Ok((read < MAX_LINE as usize || line.ends_with('\n')).then_some(line))
}

/// Whether a Host header names this machine on `port`. A bare address is
/// what Horadric's own client sends.
fn local_host(value: &str, port: u16) -> bool {
    let (name, at) = match value.rsplit_once(':') {
        Some((n, p)) if !n.is_empty() && !p.contains(']') => (n, Some(p)),
        _ => (value, None),
    };
    let named = ["127.0.0.1", "localhost", "[::1]"]
        .iter()
        .any(|h| name.eq_ignore_ascii_case(h));
    named && at.is_none_or(|p| p.parse::<u16>() == Ok(port))
}

fn respond(stream: &TcpStream, status: &str) -> io::Result<()> {
    respond_with(stream, status, "{}")
}

fn respond_with(mut stream: &TcpStream, status: &str, body: &str) -> io::Result<()> {
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(reply.as_bytes())?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;
    use std::sync::mpsc;

    #[test]
    fn a_search_for_a_name_is_a_name_and_a_regex_is_not() {
        let grep = |p: &str| searched_name("Grep", &json!({ "pattern": p }));
        let glob = |p: &str| searched_name("Glob", &json!({ "pattern": p }));
        assert_eq!(grep("overlap").as_deref(), Some("overlap"));
        assert_eq!(grep("\\bRecallCall\\b").as_deref(), Some("RecallCall"));
        assert_eq!(grep("fn recall(").as_deref(), None);
        assert_eq!(grep("a.*b|c"), None);
        assert_eq!(grep("ab"), None);
        assert_eq!(glob("crates/**/overlap*.rs").as_deref(), Some("overlap"));
        assert_eq!(glob("**/*.rs"), None);
        assert_eq!(
            searched_name("Read", &json!({ "pattern": "overlap" })),
            None
        );
    }

    #[test]
    fn a_search_brings_what_the_lore_knows_once() {
        let (tx, rx) = mpsc::channel();
        let app = thread::spawn(move || {
            if let Ok(Command::Recall(call)) = rx.recv_timeout(Duration::from_secs(2)) {
                assert_eq!(call.body["strong"], true);
                call.reply.send(json!({ "text": "Defined in a.rs" }));
            }
        });
        let event = HookEvent {
            session_id: "s".into(),
            hook_event_name: "PreToolUse".into(),
            cwd: "/p".into(),
            tool_name: Some("Grep".into()),
            tool_input: Some(json!({ "pattern": "UniqueSearchedName" })),
            ..Default::default()
        };
        let text = before_search("once", &event, &tx).unwrap();
        assert!(text.contains("Defined in a.rs"), "{text}");
        app.join().unwrap();
        assert!(before_search("once", &event, &tx).is_none());
    }

    fn post(port: u16, headers: &str, body: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req = format!(
            "POST {HOOK_PATH} HTTP/1.1\r\nHost: 127.0.0.1\r\n{headers}Content-Length: {}\r\n\r\n{body}",
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
            "X-Horadric-Session: tile-7\r\n",
            r#"{"session_id":"c","hook_event_name":"Stop"}"#,
        );
        assert!(reply.starts_with("HTTP/1.1 200"));
        let got = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(got.horadric_id, "tile-7");
        assert_eq!(got.event.hook_event_name, "Stop");
    }

    #[test]
    fn a_hook_from_a_web_page_or_a_rebound_name_is_refused() {
        let (port, rx) = start();
        let body = r#"{"session_id":"c","hook_event_name":"Stop"}"#;
        for headers in [
            "X-Horadric-Session: t\r\nOrigin: https://a.b\r\n".to_string(),
            "X-Horadric-Session: t\r\nSec-Fetch-Mode: no-cors\r\n".to_string(),
        ] {
            assert!(post(port, &headers, body).starts_with("HTTP/1.1 403"));
        }
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req = format!(
            "POST {HOOK_PATH} HTTP/1.1\r\nHost: evil.example:{port}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 403"), "{out}");
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn only_this_machine_on_this_port_is_a_local_host() {
        assert!(local_host("127.0.0.1", 43117));
        assert!(local_host("127.0.0.1:43117", 43117));
        assert!(local_host("LocalHost:43117", 43117));
        assert!(local_host("[::1]:43117", 43117));
        assert!(local_host("[::1]", 43117));
        assert!(!local_host("127.0.0.1:43118", 43117));
        assert!(!local_host("evil.example:43117", 43117));
        assert!(!local_host("127.0.0.1.evil.example", 43117));
        assert!(!local_host("", 43117));
    }

    #[test]
    fn an_odd_session_id_is_dropped_and_a_foreign_transcript_forgotten() {
        let (port, rx) = start();
        post(
            port,
            "X-Horadric-Session: t\r\n",
            r#"{"session_id":"..\\x","hook_event_name":"Stop"}"#,
        );
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
        post(
            port,
            "X-Horadric-Session: t\r\n",
            r#"{"session_id":"c","hook_event_name":"Stop","transcript_path":"\\\\evil@80\\s\\t.jsonl"}"#,
        );
        let got = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(got.event.transcript_path, "");
        post(
            port,
            "X-Horadric-Session: t\r\n",
            r#"{"session_id":"c","hook_event_name":"SubagentStop","agent_transcript_path":"//evil/s/a.jsonl"}"#,
        );
        let got = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(got.event.agent_transcript_path, None);
    }

    #[test]
    fn an_endless_header_line_is_refused() {
        let (port, _rx) = start();
        let long = format!("X-Pad: {}\r\n", "a".repeat(MAX_LINE as usize));
        assert!(post(port, &long, "{}").starts_with("HTTP/1.1 431"));
    }

    #[test]
    fn an_edit_to_a_file_another_session_changed_is_answered_with_a_warning() {
        let (port, _rx) = start();
        let edit = r#"{"session_id":"c","hook_event_name":"PostToolUse","tool_name":"Edit",
            "tool_input":{"file_path":"C:/listener-test/shared.rs"}}"#;
        let first = post(port, "X-Horadric-Session: one\r\n", edit);
        assert!(first.ends_with("\r\n\r\n{}"), "{first}");
        let second = post(port, "X-Horadric-Session: two\r\n", edit);
        assert!(second.starts_with("HTTP/1.1 200"), "{second}");
        assert!(second.contains("additionalContext"), "{second}");
        assert!(second.contains("C:/listener-test/shared.rs"), "{second}");
    }

    #[test]
    fn event_owned_elsewhere_is_passed_on() {
        let (host, host_rx) = start();
        let (dev, dev_rx) = start();
        let reply = post(
            host,
            &format!("X-Horadric-Session: tile-9\r\nX-Horadric-Port: {dev}\r\n"),
            r#"{"session_id":"c","hook_event_name":"Stop"}"#,
        );
        assert!(reply.starts_with("HTTP/1.1 200"));
        let got = dev_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(got.horadric_id, "tile-9");
        assert_eq!(got.event.hook_event_name, "Stop");
        assert!(host_rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn event_owned_here_or_by_nobody_stays() {
        let (port, rx) = start();
        for owner in [format!("{port}"), String::new(), "junk".into()] {
            post(
                port,
                &format!("X-Horadric-Session: tile-1\r\nX-Horadric-Port: {owner}\r\n"),
                r#"{"session_id":"c","hook_event_name":"Stop"}"#,
            );
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(2)).unwrap().horadric_id,
                "tile-1"
            );
        }
    }

    #[test]
    fn status_is_forwarded_as_an_event() {
        let (port, rx) = start();
        let body = r#"{"model":{"display_name":"Haiku"},"rate_limits":{"five_hour":{"used_percentage":9}}}"#;
        let reply = post_to(port, STATUS_PATH, "X-Horadric-Session: tile-3\r\n", body);
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        let got = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(got.horadric_id, "tile-3");
        assert_eq!(got.event.hook_event_name, HookEvent::STATUS);
        let status = got.event.status.unwrap();
        assert_eq!(status.model.as_deref(), Some("Haiku"));
        assert_eq!(status.limits.five_hour.map(|l| l.used), Some(9.0));
        // Untagged, it is a status line outside Horadric.
        post_to(port, STATUS_PATH, "", body);
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn untagged_post_is_passed_on_without_a_tag() {
        let (port, rx) = start();
        let reply = post(port, "", r#"{"session_id":"c","hook_event_name":"Stop"}"#);
        assert!(reply.starts_with("HTTP/1.1 200"));
        let got = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(got.horadric_id, "");
        assert_eq!(got.event.session_id, "c");
    }

    fn start_with_new() -> (u16, mpsc::Receiver<Command>) {
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
        post_to(port, NEW_PATH, headers, body)
    }

    fn post_to(port: u16, path: &str, headers: &str, body: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{headers}Content-Length: {}\r\n\r\n{body}",
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
        let reply = post_new(port, "X-Horadric-Command: new\r\n", &want.to_json());
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Command::New(want)
        );
    }

    #[test]
    fn reload_request_is_forwarded() {
        let (port, rx) = start_with_new();
        let want = Reload {
            exe: "C:/dev/horadric/target/release/horadric.exe".into(),
            now: true,
        };
        let reply = post_to(
            port,
            RELOAD_PATH,
            "X-Horadric-Command: reload\r\n",
            &want.to_json(),
        );
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Command::Reload(want)
        );
    }

    #[test]
    fn reload_needs_its_own_header_and_no_origin() {
        let (port, rx) = start_with_new();
        let body = r#"{"exe":"C:/x/horadric.exe"}"#;
        for headers in [
            "",
            "X-Horadric-Command: new\r\n",
            "X-Horadric-Command: reload\r\nOrigin: https://example.com\r\n",
        ] {
            let reply = post_to(port, RELOAD_PATH, headers, body);
            assert!(reply.starts_with("HTTP/1.1 403"), "{headers}: {reply}");
        }
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn a_changed_task_list_is_forwarded_with_its_own_header_only() {
        let (port, rx) = start_with_new();
        let want = TasksChanged {
            dir: "C:/dev/app".into(),
            quest: Some("Serve the API".into()),
            tell: Some("Use port 4100.".into()),
            fix: Some("No tests.".into()),
            cast: Some("Ship Local".into()),
            overlap: Some("Shares crates/api with Fix the API.".into()),
            by: Some("warriv-5".into()),
        };
        let json = want.to_json();
        let reply = post_to(port, TASKS_PATH, "X-Horadric-Command: new\r\n", &json);
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
        let reply = post_to(port, TASKS_PATH, "X-Horadric-Command: tasks\r\n", &json);
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Command::Tasks(want)
        );
    }

    #[test]
    fn the_mod_is_answered_what_the_app_knows_of_its_session() {
        let (port, rx) = start_with_new();
        let app = thread::spawn(move || match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Command::Know(call)) => {
                assert_eq!(call.session, "tile-know");
                assert!(call.overlaps.is_empty());
                call.reply.send(json!({ "quest": "Serve the API" }));
            }
            other => panic!("{other:?}"),
        });
        let reply = post_to(port, KNOW_PATH, "X-Horadric-Session: tile-know\r\n", "{}");
        app.join().unwrap();
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(reply.contains("Serve the API"), "{reply}");
    }

    #[test]
    fn the_mod_must_name_its_session_and_not_be_a_web_page() {
        let (port, rx) = start_with_new();
        for headers in [
            "",
            "X-Horadric-Session: tile-know\r\nOrigin: https://example.com\r\n",
        ] {
            let reply = post_to(port, KNOW_PATH, headers, "{}");
            assert!(reply.starts_with("HTTP/1.1 403"), "{headers}: {reply}");
        }
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn a_browser_call_is_answered_with_what_the_app_answers() {
        let (port, rx) = start_with_new();
        let app = thread::spawn(move || match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Command::Browser(call)) => {
                assert_eq!(call.session, "tile-2");
                assert_eq!(call.body["op"], "info");
                call.reply.send(json!({ "open": false }));
            }
            other => panic!("{other:?}"),
        });
        let headers = "X-Horadric-Command: browser\r\nX-Horadric-Session: tile-2\r\n";
        let reply = post_to(port, BROWSER_PATH, headers, r#"{"op":"info"}"#);
        app.join().unwrap();
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(reply.ends_with(r#"{"open":false}"#), "{reply}");
    }

    #[test]
    fn a_recall_is_answered_with_what_the_app_answers() {
        let (port, rx) = start_with_new();
        let app = thread::spawn(move || match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Command::Recall(call)) => {
                assert_eq!(call.body["query"], "reload");
                call.reply
                    .send(json!({ "text": "#1 (note, a minute ago)" }));
            }
            other => panic!("{other:?}"),
        });
        let reply = post_to(
            port,
            RECALL_PATH,
            "X-Horadric-Command: recall
",
            r#"{"query":"reload"}"#,
        );
        app.join().unwrap();
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(
            reply.ends_with(r##"{"text":"#1 (note, a minute ago)"}"##),
            "{reply}"
        );
    }

    #[test]
    fn a_prompt_is_answered_with_what_the_memory_recalls_on_it() {
        let (port, rx) = start_with_new();
        let app = thread::spawn(move || match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Command::Recall(call)) => {
                assert_eq!(call.body["prompt"], true);
                assert_eq!(call.body["query"], "how does reload work?");
                assert_eq!(call.body["session"], "tile-3");
                assert_eq!(call.body["agent_session"], "c");
                assert_eq!(call.body["dir"], "C:/p");
                call.reply
                    .send(json!({ "text": "#1 (note, a minute ago)" }));
            }
            other => panic!("{other:?}"),
        });
        let reply = post(
            port,
            "X-Horadric-Session: tile-3\r\n",
            r#"{"session_id":"c","hook_event_name":"UserPromptSubmit","cwd":"C:/p",
                "prompt":"how does reload work?"}"#,
        );
        app.join().unwrap();
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        let body: Value = serde_json::from_str(reply.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let out = &body["hookSpecificOutput"];
        assert_eq!(out["hookEventName"], "UserPromptSubmit");
        assert_eq!(out["additionalContext"], "#1 (note, a minute ago)");
    }

    #[test]
    fn a_prompt_with_nothing_close_or_no_question_goes_on_bare() {
        let (port, rx) = start_with_new();
        let app = thread::spawn(move || {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(Command::Recall(call)) => call.reply.send(json!({ "text": "" })),
                other => panic!("{other:?}"),
            }
            rx
        });
        let prompt = |text: &str| {
            format!(
                r#"{{"session_id":"c","hook_event_name":"UserPromptSubmit","cwd":"C:/p","prompt":"{text}"}}"#
            )
        };
        let reply = post(port, "X-Horadric-Session: tile-3\r\n", &prompt("why?"));
        let rx = app.join().unwrap();
        assert!(reply.ends_with("\r\n\r\n{}"), "{reply}");
        // A slash command, or a claude Horadric did not start, is not asked
        // about at all.
        let reply = post(port, "X-Horadric-Session: tile-3\r\n", &prompt("/compact"));
        assert!(reply.ends_with("\r\n\r\n{}"), "{reply}");
        let reply = post(port, "", &prompt("why?"));
        assert!(reply.ends_with("\r\n\r\n{}"), "{reply}");
        let note = "<task-notification><status>completed</status></task-notification>";
        let reply = post(port, "X-Horadric-Session: tile-3\r\n", &prompt(note));
        assert!(reply.ends_with("\r\n\r\n{}"), "{reply}");
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn a_recall_needs_its_header_and_no_origin() {
        let (port, rx) = start_with_new();
        for headers in [
            "X-Horadric-Command: browser
",
            "X-Horadric-Command: recall
Origin: https://a.b
",
        ] {
            let reply = post_to(port, RECALL_PATH, headers, "{}");
            assert!(reply.starts_with("HTTP/1.1 403"), "{headers}: {reply}");
        }
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    /// The app starts gits and sessions while a call waits on it. None of
    /// them may hold the caller's connection open: the caller would wait
    /// past its answer, or be reset when the child exits, with its call
    /// done all the same.
    #[cfg(windows)]
    #[test]
    fn a_child_the_app_starts_meanwhile_does_not_hold_the_reply() {
        let (port, rx) = start_with_new();
        let app = thread::spawn(move || match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Command::Browser(call)) => {
                let child = std::process::Command::new("ping")
                    .args(["-n", "4", "127.0.0.1"])
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap();
                call.reply.send(json!({ "ok": true }));
                (child, std::time::Instant::now())
            }
            other => panic!("{other:?}"),
        });
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let body = r#"{"op":"info"}"#;
        let req = format!(
            "POST {BROWSER_PATH} HTTP/1.1\r\nX-Horadric-Command: browser\r\n\
             X-Horadric-Session: tile-2\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        let read = s.read_to_string(&mut out);
        let finished = std::time::Instant::now();
        let (mut child, replied) = app.join().unwrap();
        let _ = child.kill();
        read.unwrap();
        assert!(out.ends_with(r#"{"ok":true}"#), "{out}");
        // Timed from the answer, not the request, since starting the child
        // alone can take a second on a busy runner.
        let waited = finished.saturating_duration_since(replied);
        assert!(waited < Duration::from_secs(2), "waited {waited:?}");
    }

    #[test]
    fn a_browser_call_given_up_on_is_no_longer_wanted() {
        let (port, rx) = start_with_new();
        let headers = "X-Horadric-Command: browser
X-Horadric-Session: tile-2
";
        let reply = post_to(port, BROWSER_PATH, headers, r#"{"op":"info"}"#);
        assert!(reply.starts_with("HTTP/1.1 504"), "{reply}");
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Command::Browser(call)) => assert!(!call.reply.wanted()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_browser_call_needs_its_header_a_session_and_no_origin() {
        let (port, rx) = start_with_new();
        for headers in [
            "X-Horadric-Session: tile-2\r\n",
            "X-Horadric-Command: browser\r\n",
            "X-Horadric-Command: browser\r\nX-Horadric-Session: t\r\nOrigin: https://a.b\r\n",
        ] {
            let reply = post_to(port, BROWSER_PATH, headers, r#"{"op":"info"}"#);
            assert!(reply.starts_with("HTTP/1.1 4"), "{headers}: {reply}");
        }
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn a_glow_is_passed_on_as_an_event_of_its_session() {
        let (port, rx) = start();
        let body = r#"{"nodes":["note:3"],"touch":"recall"}"#;
        let headers = "X-Horadric-Command: glow
X-Horadric-Session: tile-g
";
        let reply = post_to(port, GLOW_PATH, headers, body);
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        let t = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(t.horadric_id, "tile-g");
        assert_eq!(t.event.hook_event_name, HookEvent::GLOW);
        assert_eq!(t.event.tool_input.unwrap()["nodes"][0], "note:3");
    }

    #[test]
    fn a_glow_needs_its_header_a_session_and_no_origin() {
        let (port, rx) = start();
        for headers in [
            "X-Horadric-Session: tile-g
",
            "X-Horadric-Command: glow
",
            "X-Horadric-Command: glow
X-Horadric-Session: t
Origin: https://a.b
",
        ] {
            let reply = post_to(port, GLOW_PATH, headers, "{}");
            assert!(reply.starts_with("HTTP/1.1 4"), "{headers}: {reply}");
        }
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn a_task_list_change_from_an_older_build_still_reads() {
        let t = TasksChanged::from_json(br#"{"dir":"C:/dev/app"}"#).unwrap();
        assert_eq!((t.quest, t.tell), (None, None));
        // A tomb's report, from before the tombs were removed.
        let t = TasksChanged::from_json(br#"{"dir":"C:/dev/app","tomb":"a-1.x2.1","why":"x"}"#);
        assert_eq!(t.map(|t| t.dir).as_deref(), Some("C:/dev/app"));
    }

    #[test]
    fn reload_json_defaults_to_waiting() {
        let r = Reload::from_json(br#"{"exe":"C:/x/horadric.exe"}"#).unwrap();
        assert!(!r.now);
        assert_eq!(Reload::from_json(br#"{"now":true}"#), None);
    }

    #[test]
    fn new_session_needs_the_header_and_no_origin() {
        let (port, rx) = start_with_new();
        let body = r#"{"cwd":"C:/x"}"#;
        assert!(post_new(port, "", body).starts_with("HTTP/1.1 403"));
        let browser = "X-Horadric-Command: new\r\nOrigin: https://example.com\r\n";
        assert!(post_new(port, browser, body).starts_with("HTTP/1.1 403"));
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn a_command_from_another_state_folder_is_refused_with_the_owner_named() {
        let (port, rx) = start_with_new();
        let ours = crate::state_header();
        let body = r#"{"cwd":"C:/x"}"#;
        let from =
            |state: &str| format!("X-Horadric-Command: new\r\nX-Horadric-State: {state}\r\n");
        let reply = post_new(port, &from(&ours), body);
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok());
        if ours.is_empty() {
            return;
        }
        let reply = post_new(port, &from(r"C:\elsewhere\Horadric-dev"), body);
        assert!(reply.starts_with("HTTP/1.1 409"), "{reply}");
        assert!(reply.contains(&format!("port {port}")), "{reply}");
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn new_session_without_the_app_is_unavailable() {
        let (port, _rx) = start();
        let reply = post_new(port, "X-Horadric-Command: new\r\n", r#"{"cwd":"C:/x"}"#);
        assert!(reply.starts_with("HTTP/1.1 503"), "{reply}");
    }

    #[test]
    fn new_session_json_tolerates_missing_optionals() {
        let n = NewSession::from_json(br#"{"cwd":"C:/x"}"#).unwrap();
        assert_eq!(n.name, None);
        assert!(n.args.is_empty());
        assert_eq!(NewSession::from_json(br#"{"name":"x"}"#), None);
        assert!(NewSession::from_json(br#"{"cwd":"C:/x","agent":"claude"}"#).is_some());
        assert_eq!(
            NewSession::from_json(br#"{"cwd":"C:/x","agent":"codex"}"#),
            None
        );
    }

    #[test]
    fn wrong_path_is_404() {
        let (port, _rx) = start();
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(b"GET /nope HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 404"));
    }
}
