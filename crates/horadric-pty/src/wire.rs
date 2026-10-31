//! What a session host and the UI say to each other over the pipe, and the
//! output ring the host keeps. Pure, so all of it is tested.
//!
//! A connection starts with the host's hello: four magic bytes and the
//! protocol version. Then frames, each a kind byte, a little endian length
//! and that many bytes. Five kinds and no more: input, resize, output,
//! exit, kill. Hosts from older builds live on until their session ends,
//! so a new UI has to understand every version still running, and a small
//! protocol keeps that easy.

use std::collections::{HashMap, VecDeque};

/// This build's protocol. A UI speaks every version up to its own.
pub const VERSION: u8 = 1;

/// Starts every connection, before the version byte.
pub const MAGIC: &[u8; 4] = b"HRDH";

/// The hello the host sends first.
pub fn hello() -> [u8; 5] {
    [MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], VERSION]
}

/// The version in a hello, or None when it is not one this build speaks.
pub fn read_hello(bytes: &[u8; 5]) -> Option<u8> {
    (&bytes[..4] == MAGIC && (1..=VERSION).contains(&bytes[4])).then_some(bytes[4])
}

/// Anything longer is a broken stream, not a message.
const MAX_FRAME: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Keys for the program, UI to host.
    Input(Vec<u8>),
    /// UI to host: the console's new size. Host to UI, first after the
    /// hello: the size it has now, which the replay was drawn at.
    Resize { cols: u16, rows: u16 },
    /// What the program printed, host to UI.
    Output(Vec<u8>),
    /// The program ended with this code, host to UI, last.
    Exit(u32),
    /// End the program now, UI to host.
    Kill,
}

const INPUT: u8 = 1;
const RESIZE: u8 = 2;
const OUTPUT: u8 = 3;
const EXIT: u8 = 4;
const KILL: u8 = 5;

impl Message {
    pub fn encode(&self) -> Vec<u8> {
        let (kind, body): (u8, Vec<u8>) = match self {
            Message::Input(b) => (INPUT, b.clone()),
            Message::Resize { cols, rows } => {
                (RESIZE, [cols.to_le_bytes(), rows.to_le_bytes()].concat())
            }
            Message::Output(b) => (OUTPUT, b.clone()),
            Message::Exit(code) => (EXIT, code.to_le_bytes().to_vec()),
            Message::Kill => (KILL, Vec::new()),
        };
        frame(kind, &body)
    }
}

/// A kind byte, a little endian length, the body.
fn frame(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + body.len());
    out.push(kind);
    out.extend((body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    out
}

/// Collects bytes as they arrive and hands out whole messages.
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next whole message, None until one has arrived, or an error
    /// when the stream can not be a conversation with a host. A kind this
    /// build does not know is skipped, so a newer peer can add one without
    /// breaking an older one.
    pub fn take(&mut self) -> Result<Option<Message>, String> {
        loop {
            let Some((kind, body)) = self.frame(MAX_FRAME)? else {
                return Ok(None);
            };
            let message = match (kind, body.len()) {
                (INPUT, _) => Message::Input(body),
                (OUTPUT, _) => Message::Output(body),
                (RESIZE, 4) => Message::Resize {
                    cols: u16::from_le_bytes([body[0], body[1]]),
                    rows: u16::from_le_bytes([body[2], body[3]]),
                },
                (EXIT, 4) => {
                    Message::Exit(u32::from_le_bytes([body[0], body[1], body[2], body[3]]))
                }
                (KILL, 0) => Message::Kill,
                (RESIZE | EXIT | KILL, n) => return Err(format!("kind {kind} with {n} bytes")),
                _ => continue,
            };
            return Ok(Some(message));
        }
    }

    /// The next whole frame as its kind and body, none longer than `max`.
    fn frame(&mut self, max: usize) -> Result<Option<(u8, Vec<u8>)>, String> {
        if self.buf.len() < 5 {
            return Ok(None);
        }
        let len = u32::from_le_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]]) as usize;
        if len > max {
            return Err(format!("a frame of {len} bytes"));
        }
        if self.buf.len() < 5 + len {
            return Ok(None);
        }
        let kind = self.buf[0];
        Ok(Some((kind, self.buf.drain(..5 + len).skip(5).collect())))
    }
}

/// The last `cap` bytes of a session's output, which a UI that attaches
/// replays into a fresh terminal.
pub struct Ring {
    bytes: VecDeque<u8>,
    cap: usize,
    /// Whether the oldest bytes were ever dropped.
    trimmed: bool,
    /// Every byte ever pushed, so a reader that took the ring up to some
    /// point can ask for what came after it.
    pushed: u64,
}

impl Ring {
    pub fn new(cap: usize) -> Ring {
        Ring {
            bytes: VecDeque::new(),
            cap,
            trimmed: false,
            pushed: 0,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.pushed += bytes.len() as u64;
        self.bytes.extend(bytes);
        let over = self.bytes.len().saturating_sub(self.cap);
        if over > 0 {
            self.bytes.drain(..over);
            self.trimmed = true;
        }
    }

    /// What to replay. Once the ring has dropped its oldest bytes it most
    /// likely starts inside a line or an escape sequence, so it starts
    /// after the first line break instead, where the terminal is in a
    /// state it knows.
    pub fn replay(&self) -> Vec<u8> {
        self.replay_at().1
    }

    /// The replay with the offset of its first byte, counted as
    /// [`Ring::end`] counts.
    pub fn replay_at(&self) -> (u64, Vec<u8>) {
        let (a, b) = self.bytes.as_slices();
        let all = [a, b].concat();
        let start = self.end() - all.len() as u64;
        if !self.trimmed {
            return (start, all);
        }
        match all.iter().position(|&c| c == b'\n') {
            Some(i) => (start + i as u64 + 1, all[i + 1..].to_vec()),
            None => (self.end(), Vec::new()),
        }
    }

    /// How many bytes were ever pushed: the offset the next byte will have.
    pub fn end(&self) -> u64 {
        self.pushed
    }

    /// The bytes from offset `from` on, or None when the ring has dropped
    /// some of them, and a reader that wants them all has to start over
    /// from a replay.
    pub fn since(&self, from: u64) -> Option<Vec<u8>> {
        let start = self.end() - self.bytes.len() as u64;
        if from < start || from > self.end() {
            return None;
        }
        Some(
            self.bytes
                .range((from - start) as usize..)
                .copied()
                .collect(),
        )
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

// The headless protocol: a host running `claude.exe` on pipes, whose
// stream is lines of JSON rather than a screen. It has a hello of its own,
// so a terminal client and a headless host (or the reverse) tell each
// other apart at once, and its own version, so an older client meeting a
// newer host says so instead of misreading it.

/// Starts every headless connection, before the version byte.
pub const HEADLESS_MAGIC: &[u8; 4] = b"HRDL";

/// This build's headless protocol. A client speaks every version up to it.
pub const HEADLESS_VERSION: u8 = 1;

/// The hello a headless host sends first.
pub fn headless_hello() -> [u8; 5] {
    let m = HEADLESS_MAGIC;
    [m[0], m[1], m[2], m[3], HEADLESS_VERSION]
}

/// The version in a headless hello, or why it is not one this build speaks.
pub fn read_headless_hello(bytes: &[u8; 5]) -> Result<u8, &'static str> {
    if &bytes[..4] == MAGIC {
        return Err("a terminal session host, not a headless one");
    }
    if &bytes[..4] != HEADLESS_MAGIC || bytes[4] == 0 {
        return Err("not a Horadric session host");
    }
    if bytes[4] > HEADLESS_VERSION {
        return Err("a headless host from a newer Horadric");
    }
    Ok(bytes[4])
}

/// A stdout line can hold an image or a whole file. Longer ones go as a
/// [`too_long`] line instead.
pub const MAX_LINE: usize = 64 * 1024 * 1024;

/// What stands in for a stdout line longer than [`MAX_LINE`].
pub fn too_long(bytes: usize) -> String {
    format!(r#"{{"type":"horadric","subtype":"line_too_long","bytes":{bytes}}}"#)
}

/// What a headless host and its client say to each other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineMessage {
    /// Client to host: one line for claude's stdin.
    Send(String),
    /// Client to host: end the session for good. Detaching is just
    /// closing the pipe.
    Close,
    /// Host to client, first after the hello: claude's process id and
    /// what the client that started it asked to keep, as JSON.
    Info(String),
    /// Host to client: the backlog lost this many of its oldest lines.
    /// Comes before the backlog when it did.
    Dropped(u64),
    /// Host to client: a line claude wrote to stdout.
    Out(String),
    /// Host to client, only in a backlog: a line a client sent to stdin.
    Sent(String),
    /// Host to client: the backlog is over, what follows is live.
    Live,
    /// Host to client: one of claude's last stderr lines, oldest first,
    /// just before the exit.
    Stderr(String),
    /// Host to client, last: claude ended with this code.
    Exit(u32),
}

const L_SEND: u8 = 1;
const L_CLOSE: u8 = 2;
const L_INFO: u8 = 3;
const L_DROPPED: u8 = 4;
const L_OUT: u8 = 5;
const L_SENT: u8 = 6;
const L_LIVE: u8 = 7;
const L_STDERR: u8 = 8;
const L_EXIT: u8 = 9;

impl LineMessage {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            LineMessage::Send(t) => frame(L_SEND, t.as_bytes()),
            LineMessage::Close => frame(L_CLOSE, &[]),
            LineMessage::Info(t) => frame(L_INFO, t.as_bytes()),
            LineMessage::Dropped(n) => frame(L_DROPPED, &n.to_le_bytes()),
            LineMessage::Out(t) => frame(L_OUT, t.as_bytes()),
            LineMessage::Sent(t) => frame(L_SENT, t.as_bytes()),
            LineMessage::Live => frame(L_LIVE, &[]),
            LineMessage::Stderr(t) => frame(L_STDERR, t.as_bytes()),
            LineMessage::Exit(code) => frame(L_EXIT, &code.to_le_bytes()),
        }
    }
}

/// [`Decoder`] for the headless protocol.
#[derive(Default)]
pub struct LineDecoder {
    frames: Decoder,
}

impl LineDecoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.frames.push(bytes);
    }

    /// The next whole message, None until one has arrived, or an error
    /// when the stream is broken. Unknown kinds are skipped, as in
    /// [`Decoder::take`].
    pub fn take(&mut self) -> Result<Option<LineMessage>, String> {
        let text = |b: Vec<u8>| String::from_utf8_lossy(&b).into_owned();
        loop {
            let Some((kind, body)) = self.frames.frame(MAX_LINE + 1024)? else {
                return Ok(None);
            };
            let message = match (kind, body.len()) {
                (L_SEND, _) => LineMessage::Send(text(body)),
                (L_CLOSE, 0) => LineMessage::Close,
                (L_INFO, _) => LineMessage::Info(text(body)),
                (L_DROPPED, 8) => LineMessage::Dropped(u64::from_le_bytes(
                    body[..8].try_into().expect("eight bytes"),
                )),
                (L_OUT, _) => LineMessage::Out(text(body)),
                (L_SENT, _) => LineMessage::Sent(text(body)),
                (L_LIVE, 0) => LineMessage::Live,
                (L_STDERR, _) => LineMessage::Stderr(text(body)),
                (L_EXIT, 4) => LineMessage::Exit(u32::from_le_bytes(
                    body[..4].try_into().expect("four bytes"),
                )),
                (L_CLOSE | L_DROPPED | L_LIVE | L_EXIT, n) => {
                    return Err(format!("kind {kind} with {n} bytes"))
                }
                _ => continue,
            };
            return Ok(Some(message));
        }
    }
}

/// One line of a headless session, as the host keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Logged {
    /// claude.exe wrote it to stdout.
    Out(String),
    /// A client sent it to claude.exe's stdin. Prompts and answers to asks
    /// are never echoed on stdout, so a replay without them would show a
    /// conversation with no questions and asks that look still open.
    Sent(String),
}

impl Logged {
    fn len(&self) -> usize {
        match self {
            Logged::Out(t) | Logged::Sent(t) => t.len(),
        }
    }
}

/// Every line of a headless session since it started, for a client that
/// attaches later, up to `cap` bytes. Past that the oldest go, and the
/// count of those gone says so. The newest line always stays, however
/// long. The whole conversation is in the session's transcript on disk
/// anyway; this is what a reload needs to carry on mid turn.
///
/// A reply streams token by token before its whole copy arrives, and the
/// tokens are most of a long session's lines. Once a content block has
/// both ended and been copied whole, its deltas say nothing the copy does
/// not, so they go and the cap holds far more of the conversation. The
/// block's start and stop stay, small, so a client replaying the rest
/// still pairs the copy with the block it streamed.
pub struct LineLog {
    /// None where a delta was let go: taking it out of the middle of the
    /// queue would move every line after it.
    lines: VecDeque<Option<Logged>>,
    bytes: usize,
    cap: usize,
    dropped: u64,
    /// How many of `lines` are None.
    holes: usize,
    /// The number the next line pushed gets, so a line's place can be
    /// found again after the oldest went.
    next: u64,
    streaming: Streaming,
    conversation: Option<String>,
}

/// The message streaming now, as far as letting its deltas go needs.
#[derive(Default)]
struct Streaming {
    message: String,
    /// Its content blocks by index.
    blocks: HashMap<u64, Streamed>,
    /// How many blocks started, so each knows its place among them.
    started: u64,
    /// How many whole copies of its blocks arrived. They come one per
    /// block, in the order the blocks started.
    copies: u64,
}

struct Streamed {
    /// Which of the message's blocks it is, from 0.
    nth: u64,
    stopped: bool,
    /// The numbers of its delta lines.
    deltas: Vec<u64>,
}

/// What a stdout line is, as far as [`LineLog`] cares. A subagent's lines
/// (with a `parent_tool_use_id`) are always `Other`: their streams
/// interleave with the main one.
#[derive(Debug, PartialEq, Eq)]
enum Shape {
    MessageStart(String),
    BlockStart(u64),
    Delta(u64),
    BlockStop(u64),
    Copy(String),
    Init(String),
    Other,
}

fn shape(line: &str) -> Shape {
    use serde::de::IgnoredAny;
    use serde::Deserialize;
    #[derive(Deserialize)]
    struct Head {
        #[serde(rename = "type", default)]
        kind: String,
        #[serde(default)]
        subtype: String,
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        parent_tool_use_id: Option<IgnoredAny>,
        #[serde(default)]
        event: Option<EventHead>,
        #[serde(default)]
        message: Option<MessageHead>,
    }
    #[derive(Deserialize)]
    struct EventHead {
        #[serde(rename = "type", default)]
        kind: String,
        #[serde(default)]
        index: u64,
        #[serde(default)]
        message: Option<MessageHead>,
    }
    #[derive(Deserialize)]
    struct MessageHead {
        #[serde(default)]
        id: Option<String>,
    }
    // Most lines are none of these, and some are megabytes of tool result.
    let words = [r#""stream_event""#, r#""assistant""#, r#""init""#];
    if !words.iter().any(|w| line.contains(w)) {
        return Shape::Other;
    }
    let Ok(h) = serde_json::from_str::<Head>(line) else {
        return Shape::Other;
    };
    if h.parent_tool_use_id.is_some() {
        return Shape::Other;
    }
    let id = |m: Option<MessageHead>| m.and_then(|m| m.id).unwrap_or_default();
    match (h.kind.as_str(), h.event) {
        ("stream_event", Some(e)) => match e.kind.as_str() {
            "message_start" => Shape::MessageStart(id(e.message)),
            "content_block_start" => Shape::BlockStart(e.index),
            "content_block_delta" => Shape::Delta(e.index),
            "content_block_stop" => Shape::BlockStop(e.index),
            _ => Shape::Other,
        },
        ("assistant", _) => Shape::Copy(id(h.message)),
        ("system", _) if h.subtype == "init" => match h.session_id {
            Some(s) => Shape::Init(s),
            None => Shape::Other,
        },
        _ => Shape::Other,
    }
}

impl LineLog {
    pub fn new(cap: usize) -> LineLog {
        LineLog {
            lines: VecDeque::new(),
            bytes: 0,
            cap,
            dropped: 0,
            holes: 0,
            next: 0,
            streaming: Streaming::default(),
            conversation: None,
        }
    }

    pub fn push(&mut self, line: Logged) {
        let shape = match &line {
            Logged::Out(t) => shape(t),
            Logged::Sent(_) => Shape::Other,
        };
        let seq = self.next;
        match shape {
            Shape::MessageStart(id) => {
                // Nothing points into the queue between messages, so this
                // is when the holes can close.
                if self.holes > self.lines.len() / 2 {
                    self.lines.retain(Option::is_some);
                    self.holes = 0;
                }
                self.streaming = Streaming {
                    message: id,
                    ..Streaming::default()
                };
            }
            Shape::BlockStart(i) => {
                let s = &mut self.streaming;
                let nth = s.started;
                s.started += 1;
                s.blocks.insert(
                    i,
                    Streamed {
                        nth,
                        stopped: false,
                        deltas: Vec::new(),
                    },
                );
            }
            Shape::Delta(i) => {
                if let Some(b) = self.streaming.blocks.get_mut(&i) {
                    b.deltas.push(seq);
                }
            }
            Shape::BlockStop(i) => {
                if let Some(b) = self.streaming.blocks.get_mut(&i) {
                    b.stopped = true;
                }
            }
            Shape::Copy(id) if !id.is_empty() && id == self.streaming.message => {
                self.streaming.copies += 1;
            }
            Shape::Init(id) => self.conversation = Some(id),
            Shape::Copy(_) | Shape::Other => {}
        }
        self.bytes += line.len();
        self.lines.push_back(Some(line));
        self.next += 1;
        self.let_go_of_deltas();
        while self.bytes > self.cap && self.lines.len() - self.holes > 1 {
            match self.lines.pop_front() {
                Some(Some(old)) => {
                    self.bytes -= old.len();
                    self.dropped += 1;
                }
                Some(None) => self.holes -= 1,
                None => break,
            }
        }
    }

    /// Lets go of the deltas of every block that has ended and been copied.
    fn let_go_of_deltas(&mut self) {
        let s = &mut self.streaming;
        let done: Vec<u64> = (s.blocks.iter())
            .filter(|(_, b)| b.stopped && b.nth < s.copies)
            .map(|(&i, _)| i)
            .collect();
        let first = self.next - self.lines.len() as u64;
        for i in done {
            let Some(b) = s.blocks.remove(&i) else {
                continue;
            };
            for seq in b.deltas.into_iter().filter(|&q| q >= first) {
                let slot = self.lines.get_mut((seq - first) as usize);
                if let Some(old) = slot.and_then(Option::take) {
                    self.bytes -= old.len();
                    self.holes += 1;
                }
            }
        }
    }

    /// The conversation the session carries on now, as its latest
    /// `system`/`init` named it. A `/clear` starts a new one.
    pub fn conversation(&self) -> Option<&str> {
        self.conversation.as_deref()
    }

    /// How many of the oldest lines are gone.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// What a client that attaches now is sent before the live tail.
    pub fn backlog(&self) -> Vec<LineMessage> {
        let mut out = Vec::with_capacity(self.lines.len() + 2);
        if self.dropped > 0 {
            out.push(LineMessage::Dropped(self.dropped));
        }
        out.extend(self.lines.iter().flatten().map(|l| match l {
            Logged::Out(t) => LineMessage::Out(t.clone()),
            Logged::Sent(t) => LineMessage::Sent(t.clone()),
        }));
        out.push(LineMessage::Live);
        out
    }

    /// How many lines it keeps.
    pub fn len(&self) -> usize {
        self.lines.len() - self.holes
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The bytes the kept lines hold.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

/// The pipe of one headless session. Its prefix is not [`pipe_prefix`]'s,
/// so a UI looking for terminal hosts to attach never finds these.
pub fn headless_pipe_name(instance: &str, id: &str) -> String {
    format!(r"\\.\pipe\{}{}", headless_pipe_prefix(instance), clean(id))
}

/// What every headless pipe name of an instance starts with after
/// `\\.\pipe\`.
pub fn headless_pipe_prefix(instance: &str) -> String {
    format!("horadric-headless-{}-", clean(instance))
}

/// The pipe of one session, unique to the Horadric instance, so a dev UI
/// never attaches to a session of the installed one.
pub fn pipe_name(instance: &str, id: &str) -> String {
    format!(r"\\.\pipe\{}{}", pipe_prefix(instance), clean(id))
}

/// What every pipe name of an instance starts with after `\\.\pipe\`.
pub fn pipe_prefix(instance: &str) -> String {
    format!("horadric-{}-", clean(instance))
}

/// The job holding a session's processes, which the UI opens to ask
/// whether a window's process belongs to the session.
pub fn job_name(instance: &str, id: &str) -> String {
    format!(r"Local\horadric-{}-{}", clean(instance), clean(id))
}

/// A backslash would be a path in both namespaces.
fn clean(s: &str) -> String {
    s.replace(['\\', '/'], "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(d: &mut Decoder) -> Vec<Message> {
        let mut out = Vec::new();
        while let Some(m) = d.take().unwrap() {
            out.push(m);
        }
        out
    }

    #[test]
    fn every_message_comes_back_as_it_went() {
        let messages = vec![
            Message::Input(b"hi\r".to_vec()),
            Message::Resize {
                cols: 120,
                rows: 36,
            },
            Message::Output(vec![0x1b, b'[', b'm']),
            Message::Output(Vec::new()),
            Message::Exit(0xC000_013A),
            Message::Kill,
        ];
        let mut d = Decoder::default();
        for m in &messages {
            d.push(&m.encode());
        }
        assert_eq!(all(&mut d), messages);
    }

    #[test]
    fn a_message_split_anywhere_waits_for_the_rest() {
        let bytes = Message::Output(b"hello".to_vec()).encode();
        for cut in 0..bytes.len() {
            let mut d = Decoder::default();
            d.push(&bytes[..cut]);
            assert_eq!(d.take().unwrap(), None, "cut at {cut}");
            d.push(&bytes[cut..]);
            assert_eq!(d.take().unwrap(), Some(Message::Output(b"hello".to_vec())));
        }
    }

    #[test]
    fn unknown_kinds_are_skipped_and_nonsense_is_an_error() {
        let mut d = Decoder::default();
        d.push(&[9, 2, 0, 0, 0, 1, 2]);
        d.push(&Message::Kill.encode());
        assert_eq!(all(&mut d), vec![Message::Kill]);

        let mut d = Decoder::default();
        d.push(&[EXIT, 1, 0, 0, 0, 7]);
        assert!(d.take().is_err());
        let mut d = Decoder::default();
        d.push(&[OUTPUT, 0xff, 0xff, 0xff, 0xff]);
        assert!(d.take().is_err());
    }

    #[test]
    fn hello_names_a_version_this_build_speaks() {
        assert_eq!(read_hello(&hello()), Some(VERSION));
        assert_eq!(read_hello(b"HRDH\0"), None);
        assert_eq!(read_hello(&[b'H', b'R', b'D', b'H', VERSION + 1]), None);
        assert_eq!(read_hello(b"HTTP/"), None);
    }

    #[test]
    fn the_ring_keeps_the_newest_bytes() {
        let mut r = Ring::new(8);
        r.push(b"abc\n");
        assert_eq!(r.replay(), b"abc\n");
        r.push(b"defghij");
        assert_eq!(r.len(), 8);
        assert_eq!(r.replay(), b"defghij", "after the line break it kept");
        r.push(b"\nkl");
        assert_eq!(r.replay(), b"kl");
    }

    #[test]
    fn a_reader_takes_what_came_after_it_until_the_ring_drops_it() {
        let mut r = Ring::new(8);
        r.push(b"ab");
        assert_eq!(r.replay_at(), (0, b"ab".to_vec()));
        assert_eq!(r.end(), 2);
        assert_eq!(r.since(2), Some(Vec::new()));
        r.push(b"cd");
        assert_eq!(r.since(2), Some(b"cd".to_vec()));
        assert_eq!(r.since(5), None, "nothing is there yet");
        r.push(b"ef\ngh\n");
        assert_eq!(r.end(), 10);
        assert_eq!(r.since(1), None, "dropped");
        assert_eq!(r.since(2), Some(b"cdef\ngh\n".to_vec()));
        assert_eq!(
            r.replay_at(),
            (7, b"gh\n".to_vec()),
            "after the first break"
        );
    }

    #[test]
    fn a_trimmed_replay_starts_on_a_new_line() {
        let mut r = Ring::new(30);
        r.push(b"xxxxxxxx\x1b[3mtail of a line\r\nnext line\r\nlast");
        assert_eq!(r.replay(), b"next line\r\nlast");
    }

    fn all_lines(d: &mut LineDecoder) -> Vec<LineMessage> {
        let mut out = Vec::new();
        while let Some(m) = d.take().unwrap() {
            out.push(m);
        }
        out
    }

    #[test]
    fn every_line_message_comes_back_as_it_went() {
        let messages = vec![
            LineMessage::Send(r#"{"type":"user"}"#.into()),
            LineMessage::Close,
            LineMessage::Info(r#"{"pid":7}"#.into()),
            LineMessage::Dropped(u64::MAX - 3),
            LineMessage::Out("caf\u{e9}".into()),
            LineMessage::Out(String::new()),
            LineMessage::Sent("prompt".into()),
            LineMessage::Live,
            LineMessage::Stderr("bad flag".into()),
            LineMessage::Exit(0xC000_013A),
        ];
        let mut d = LineDecoder::default();
        let bytes: Vec<u8> = messages.iter().flat_map(|m| m.encode()).collect();
        // A byte at a time, as a slow pipe might.
        for b in bytes {
            d.push(&[b]);
        }
        assert_eq!(all_lines(&mut d), messages);
    }

    #[test]
    fn line_frames_check_their_sizes_and_skip_kinds_from_later_builds() {
        let mut d = LineDecoder::default();
        d.push(&[42, 1, 0, 0, 0, 9]);
        d.push(&LineMessage::Live.encode());
        assert_eq!(all_lines(&mut d), vec![LineMessage::Live]);

        let mut d = LineDecoder::default();
        d.push(&[L_EXIT, 2, 0, 0, 0, 1, 2]);
        assert!(d.take().is_err());
        let mut d = LineDecoder::default();
        d.push(&[L_OUT, 0xff, 0xff, 0xff, 0x7f]);
        assert!(d.take().is_err(), "past the longest line");
    }

    #[test]
    fn a_line_longer_than_a_terminal_frame_still_goes() {
        let long = "x".repeat(MAX_FRAME + 10);
        let mut d = LineDecoder::default();
        d.push(&LineMessage::Out(long.clone()).encode());
        assert_eq!(d.take().unwrap(), Some(LineMessage::Out(long)));
        let v: serde_json::Value = serde_json::from_str(&too_long(5)).unwrap();
        assert_eq!(v["bytes"], 5);
    }

    #[test]
    fn hellos_say_which_host_and_which_version() {
        assert_eq!(read_headless_hello(&headless_hello()), Ok(HEADLESS_VERSION));
        assert!(read_headless_hello(&hello())
            .unwrap_err()
            .contains("terminal"));
        assert!(read_hello(&headless_hello()).is_none());
        let newer = [b'H', b'R', b'D', b'L', HEADLESS_VERSION + 1];
        assert!(read_headless_hello(&newer).unwrap_err().contains("newer"));
        assert!(read_headless_hello(b"HRDL\0").is_err());
        assert!(read_headless_hello(b"HTTP/").is_err());
    }

    #[test]
    fn the_log_keeps_both_directions_in_order() {
        let mut log = LineLog::new(1024);
        log.push(Logged::Out("init".into()));
        log.push(Logged::Sent("prompt".into()));
        log.push(Logged::Out("reply".into()));
        assert_eq!(
            log.backlog(),
            vec![
                LineMessage::Out("init".into()),
                LineMessage::Sent("prompt".into()),
                LineMessage::Out("reply".into()),
                LineMessage::Live,
            ]
        );
        assert_eq!(log.dropped(), 0);
        assert_eq!(log.bytes(), 15);
    }

    #[test]
    fn a_full_log_drops_its_oldest_and_says_how_many() {
        let mut log = LineLog::new(9);
        for l in ["aaaa", "bbbb", "cccc", "dd"] {
            log.push(Logged::Out(l.into()));
        }
        assert_eq!(log.len(), 2);
        assert_eq!(log.bytes(), 6);
        assert_eq!(log.dropped(), 2);
        assert_eq!(
            log.backlog(),
            vec![
                LineMessage::Dropped(2),
                LineMessage::Out("cccc".into()),
                LineMessage::Out("dd".into()),
                LineMessage::Live,
            ]
        );
    }

    #[test]
    fn the_newest_line_stays_however_long() {
        let mut log = LineLog::new(4);
        log.push(Logged::Out("ab".into()));
        log.push(Logged::Out("a much longer line".into()));
        assert_eq!(log.len(), 1);
        assert_eq!(log.dropped(), 1);
        log.push(Logged::Sent("x".into()));
        assert_eq!(log.backlog()[1], LineMessage::Sent("x".into()));
        assert_eq!(log.len(), 1);
    }

    fn event(kind: &str, rest: &str) -> String {
        format!(
            r#"{{"type":"stream_event","event":{{"type":"{kind}"{rest}}},"parent_tool_use_id":null}}"#
        )
    }

    fn copy(id: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"id":"{id}","content":[]}},"parent_tool_use_id":null}}"#
        )
    }

    fn kept(log: &LineLog) -> Vec<String> {
        (log.backlog().into_iter())
            .filter_map(|m| match m {
                LineMessage::Out(t) | LineMessage::Sent(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_block_ended_and_copied_lets_its_deltas_go() {
        let mut log = LineLog::new(1 << 20);
        let start = event("message_start", r#","message":{"id":"m1"}"#);
        let lines = [
            start.clone(),
            event("content_block_start", r#","index":0"#),
            event("content_block_delta", r#","index":0,"delta":"a""#),
            event("content_block_delta", r#","index":0,"delta":"b""#),
            copy("m1"),
            event("content_block_stop", r#","index":0"#),
            event("content_block_start", r#","index":1"#),
            event("content_block_delta", r#","index":1,"delta":"c""#),
        ];
        for l in &lines {
            log.push(Logged::Out(l.clone()));
        }
        let left = kept(&log);
        assert_eq!(left.len(), 6, "{left:#?}");
        assert!(!left.contains(&lines[2]) && !left.contains(&lines[3]));
        assert!(
            left.contains(&lines[7]),
            "the block still streaming keeps its"
        );
        assert!(
            left.contains(&start),
            "so the copy pairs with what streamed"
        );
        assert_eq!(log.len(), 6);

        // The copy may come after the stop as well.
        log.push(Logged::Out(event("content_block_stop", r#","index":1"#)));
        assert!(kept(&log).contains(&lines[7]), "not copied yet");
        log.push(Logged::Out(copy("m1")));
        assert!(!kept(&log).contains(&lines[7]));
        assert_eq!(
            log.bytes(),
            kept(&log).iter().map(String::len).sum::<usize>()
        );
    }

    #[test]
    fn a_subagent_stream_and_another_messages_copy_keep_their_deltas() {
        let mut log = LineLog::new(1 << 20);
        let theirs = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0},"parent_tool_use_id":"toolu_1"}"#;
        for l in [
            event("message_start", r#","message":{"id":"m1"}"#),
            event("content_block_start", r#","index":0"#),
            event("content_block_delta", r#","index":0"#),
            theirs.to_string(),
            event("content_block_stop", r#","index":0"#),
            copy("m0"),
        ] {
            log.push(Logged::Out(l));
        }
        assert_eq!(log.len(), 6);
    }

    #[test]
    fn a_recorded_stream_keeps_all_but_its_finished_deltas() {
        let stream = include_str!("../../horadric-core/tests/fixtures/headless/edits.jsonl");
        let mut log = LineLog::new(1 << 30);
        for l in stream.lines() {
            log.push(Logged::Out(l.to_string()));
        }
        let want: Vec<&str> = (stream.lines())
            .filter(|l| !l.contains(r#""type":"content_block_delta""#))
            .collect();
        assert_eq!(kept(&log), want);
        assert_eq!(
            log.conversation(),
            Some("4792f003-15a6-47f5-8cdd-efd07abf2ea4")
        );
    }

    #[test]
    fn holes_close_between_messages_and_the_cap_still_counts_whole_lines() {
        let mut log = LineLog::new(1 << 20);
        for n in 0..3 {
            let id = format!("m{n}");
            log.push(Logged::Out(event(
                "message_start",
                &format!(r#","message":{{"id":"{id}"}}"#),
            )));
            log.push(Logged::Out(event("content_block_start", r#","index":0"#)));
            for _ in 0..10 {
                log.push(Logged::Out(event("content_block_delta", r#","index":0"#)));
            }
            log.push(Logged::Out(copy(&id)));
            log.push(Logged::Out(event("content_block_stop", r#","index":0"#)));
        }
        assert_eq!(log.len(), 12);
        assert!(log.lines.len() < 12 + 30, "closed some");
        let before = kept(&log);
        let mut small = LineLog::new(log.bytes() - 1);
        for l in &before {
            small.push(Logged::Out(l.clone()));
        }
        assert_eq!(small.dropped(), 1);
        assert_eq!(kept(&small), before[1..]);
    }

    #[test]
    fn the_conversation_follows_each_init() {
        let mut log = LineLog::new(1 << 20);
        assert_eq!(log.conversation(), None);
        log.push(Logged::Out(
            r#"{"type":"system","subtype":"init","session_id":"a"}"#.into(),
        ));
        log.push(Logged::Out(
            r#"{"type":"system","subtype":"status","session_id":"x"}"#.into(),
        ));
        assert_eq!(log.conversation(), Some("a"));
        log.push(Logged::Out(
            r#"{"type":"system","subtype":"init","session_id":"b"}"#.into(),
        ));
        assert_eq!(log.conversation(), Some("b"));
    }

    #[test]
    fn headless_pipes_are_not_terminal_pipes() {
        let name = headless_pipe_name("43118", r"proj\1");
        assert_eq!(name, r"\\.\pipe\horadric-headless-43118-proj_1");
        assert!(!name.contains(&pipe_prefix("43118")));
        assert!(name.contains(&headless_pipe_prefix("43118")));
    }

    #[test]
    fn names_are_per_instance_and_never_paths() {
        assert_eq!(
            pipe_name("43117", "fix-1"),
            r"\\.\pipe\horadric-43117-fix-1"
        );
        assert_eq!(
            pipe_name("43118", r"a\b/c"),
            r"\\.\pipe\horadric-43118-a_b_c"
        );
        assert_eq!(job_name("43118", "fix-1"), r"Local\horadric-43118-fix-1");
        assert!(pipe_name("43117", "x").contains(&pipe_prefix("43117")));
        assert!(!pipe_name("43118", "x").contains(&pipe_prefix("43117")));
    }
}
