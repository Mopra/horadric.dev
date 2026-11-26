//! Watching another machine's desktop: the pure half of the client. A
//! remote Horadric started with `HORADRIC_SCREEN_SHARE=1` serves its screen
//! on its own `127.0.0.1`, this machine reaches it through `ssh -L`, and
//! every request but the proof carries the token the remote makes at each
//! of its starts, in a header. See `docs/plan/remote-screen.md`.
//!
//! Here: what `ssh` is started with, what a token looks like, the proof
//! that the far end holds it, the requests and reading their answers
//! within limits, the input events, and the file that remembers the
//! tunnels so sessions share them.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::screen_share::Region;
use crate::webpush::hmac_sha256;

/// Where a remote's installed Horadric serves its screen.
pub const PORT: u16 = 43119;
/// Where a remote's dev instance serves it, so one on this machine can be
/// tried against the installed one beside it.
pub const DEV_PORT: u16 = 43120;

/// What stands for the token wherever a URL is shown.
pub const MASK: &str = "<token>";

/// How wide a remote screenshot is unless asked otherwise.
pub const FRAME_WIDTH: u32 = 1600;

/// The file in the state folder that remembers the tunnels.
pub const TUNNELS_FILE: &str = "remote-tunnels.json";

/// The remote port to forward to.
pub fn remote_port(dev: bool) -> u16 {
    if dev {
        DEV_PORT
    } else {
        PORT
    }
}

/// The most bytes a frame's answer may be: a whole wall of monitors as a
/// JPEG is far below it, and a peer that sends more is not a screen.
pub const FRAME_CAP: usize = 32 * 1024 * 1024;

/// The most bytes any other answer may be.
pub const SMALL_CAP: usize = 64 * 1024;

/// The most bytes an answer's head may be before its end is seen.
const HEAD_CAP: usize = 16 * 1024;

/// The remote reads an input body below this many bytes.
pub const BODY_MAX: usize = 64 * 1024;

/// The remote runs at most this many events from one body.
pub const EVENTS_MAX: usize = 256;

/// Whether `dest` is made only of what a host name, a user, an IPv6
/// address in brackets or an alias is written with. Narrower than what
/// `ssh` takes, on purpose: the destination comes from an agent and is
/// shown to the human in the question that allows it, so nothing in it may
/// read as an option, a second word or something unseen.
pub fn usable(dest: &str) -> bool {
    !dest.is_empty()
        && !dest.starts_with('-')
        && dest.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | ':' | '[' | ']' | '-')
        })
}

/// Why `dest` can not be handed to `ssh` as a destination, None when it can.
pub fn refusal(dest: &str) -> Option<String> {
    if usable(dest) {
        None
    } else {
        Some(format!(
            "{dest:?} is not an ssh destination: letters, digits and . _ @ : [ ] - only, \
             not starting with -"
        ))
    }
}

/// Options every `ssh` here is started with. `BatchMode`, since nobody
/// sees a prompt for a password or a new host key; keepalives, so a link
/// that died quietly ends the tunnel and the next call starts a fresh one.
fn common() -> Vec<String> {
    [
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=15",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// What `ssh` is started with to forward `local` on this machine to
/// `remote` on the remote's own loopback. `ExitOnForwardFailure`, so a
/// port taken meanwhile ends it rather than leaving a tunnel that forwards
/// nothing; that happens only after the login, so whoever took the port
/// answers on it first, and the proof is what tells the two apart. The
/// local end binds to loopback by name, so no `ssh_config`
/// `GatewayPorts` can open it to the network. The end of options marker
/// before `dest` keeps it only a destination.
pub fn tunnel_args(dest: &str, local: u16, remote: u16) -> Vec<String> {
    let mut args = vec!["-N".to_string()];
    args.extend(common());
    args.extend([
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
        "-L".into(),
        format!("127.0.0.1:{local}:127.0.0.1:{remote}"),
        "--".into(),
        dest.into(),
    ]);
    args
}

/// What `ssh` is started with to print the remote's token. Through
/// `cmd /c`, since a Windows host's default shell may be PowerShell, and
/// this reads the same from either.
pub fn token_args(dest: &str, dev: bool) -> Vec<String> {
    let folder = if dev { "Horadric-dev" } else { "Horadric" };
    let mut args = common();
    args.extend([
        "--".into(),
        dest.into(),
        format!(r#"cmd /c type "%APPDATA%\{folder}\screen-token""#),
    ]);
    args
}

/// The token in what the remote printed: 32 hex characters in lower case,
/// as the remote writes them, nothing else but white space and a byte
/// order mark around them. Strict, since the token is also the key of the
/// proof, byte for byte.
pub fn parse_token(printed: &str) -> Option<String> {
    let t = printed.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    (t.len() == 32 && t.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .then(|| t.to_string())
}

/// The viewer page through a tunnel on `port`. The token is in the
/// fragment, which a browser sends to no server; the page takes it from
/// there.
pub fn view_url(port: u16, token: &str) -> String {
    format!("http://127.0.0.1:{port}/#t={token}")
}

/// The path of one frame, at most `width` pixels wide.
pub fn frame_path(width: u32) -> String {
    format!("/frame.jpg?w={width}")
}

/// The path of one frame of `region`, or of the whole screen with none,
/// at most `width` pixels wide, or unscaled with none: without `w` the
/// server scales nothing.
pub fn frame_path_of(width: Option<u32>, region: Option<Region>) -> String {
    let asked: Vec<String> = width
        .map(|w| format!("w={w}"))
        .into_iter()
        .chain(region.map(|r| r.query()))
        .collect();
    if asked.is_empty() {
        "/frame.jpg".into()
    } else {
        format!("/frame.jpg?{}", asked.join("&"))
    }
}

/// The path input events are posted to.
pub const INPUT_PATH: &str = "/input";

/// The path that answers with the display's resolution: the smallest
/// request that needs the token, so it shows a remote really serves.
pub const DISPLAY_PATH: &str = "/display";

/// `bytes` as lower case hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The path that asks the far end to prove it holds the token, for a
/// `nonce` of 32 hex characters ([`hex`] of 16 random bytes).
pub fn hello_path(nonce: &str) -> String {
    format!("/hello?n={nonce}")
}

/// What the real server answers `/hello` with: the HMAC of the nonce's
/// characters under the token's characters, as lower case hex.
pub fn proof(token: &str, nonce: &str) -> String {
    hex(&hmac_sha256(token.as_bytes(), nonce.as_bytes()))
}

/// Whether `body` is the proof for `nonce` under `token`. Every byte is
/// compared whatever the ones before, so the time taken says nothing of
/// how much of a guess was right.
pub fn proven(token: &str, nonce: &str, body: &[u8]) -> bool {
    let want = proof(token, nonce);
    let got = body.trim_ascii();
    got.len() == want.len()
        && want
            .bytes()
            .zip(got)
            .fold(0u8, |differs, (a, b)| differs | (a ^ b))
            == 0
}

/// The head of a request through the tunnel on `port`. The token goes in
/// a header and nowhere else, so no URL, log or history ever holds it.
/// HTTP/1.0 and `Connection: close`, so the remote closes when done and
/// there is no chunking to undo. `body` is the length of a JSON body that
/// follows.
pub fn request_head(
    method: &str,
    port: u16,
    path: &str,
    token: Option<&str>,
    body: Option<usize>,
) -> String {
    let mut head =
        format!("{method} {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    if let Some(token) = token {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    if let Some(len) = body {
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {len}\r\n"
        ));
    }
    head.push_str("\r\n");
    head
}

/// `text` with the token in it replaced, for anything shown or logged. An
/// empty token would replace between every character, so it changes
/// nothing.
pub fn mask(text: &str, token: &str) -> String {
    if token.is_empty() {
        text.to_string()
    } else {
        text.replace(token, MASK)
    }
}

/// An HTTP answer read whole: the status, the headers with their names in
/// lower case, and the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// The first header called `name`, any case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Why what came back is not an answer to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unread {
    /// Not HTTP at all: a tunnel whose far end refused closes the
    /// connection with nothing said.
    NotHttp,
    /// The body stopped before the length the head gave, so a link that
    /// dropped mid frame is not taken for a whole picture.
    CutShort { said: usize, got: usize },
    /// The head never ended, or the body is or says it will be more than
    /// `cap` bytes: nothing the remote's Horadric sends.
    TooLarge { cap: usize },
}

/// Where the end of the head is in `bytes`, if it is there yet.
fn head_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|w| w == b"\r\n\r\n")
}

/// The `Content-Length` a head gives, if it gives one that reads.
fn said_length(head: &[u8]) -> Option<usize> {
    std::str::from_utf8(head).ok()?.split("\r\n").find_map(|l| {
        let (name, value) = l.split_once(':')?;
        if name.trim().eq_ignore_ascii_case("content-length") {
            value.trim().parse().ok()
        } else {
            None
        }
    })
}

/// Whether `bytes`, what has come of an answer so far, is all of it: true
/// once the body is as long as the head said, so reading stops there and
/// a peer that never closes holds nothing up. An answer that gives no
/// length ends when the peer closes. Refused as soon as it is, or says it
/// will be, more than `cap` bytes of body, so a hostile peer fills no
/// memory.
pub fn whole_answer(bytes: &[u8], cap: usize) -> Result<bool, Unread> {
    let Some(end) = head_end(bytes) else {
        return if bytes.len() > HEAD_CAP {
            Err(Unread::TooLarge { cap: HEAD_CAP })
        } else {
            Ok(false)
        };
    };
    let body = bytes.len() - (end + 4);
    match said_length(&bytes[..end]) {
        Some(said) if said > cap => Err(Unread::TooLarge { cap }),
        Some(said) => Ok(body >= said),
        None if body > cap => Err(Unread::TooLarge { cap }),
        None => Ok(false),
    }
}

/// Reads what an HTTP/1.0 request got back. A body longer than its
/// `Content-Length` is cut to it; one shorter is refused.
pub fn parse_response(bytes: &[u8]) -> Result<Response, Unread> {
    let end = head_end(bytes).ok_or(Unread::NotHttp)?;
    let head = std::str::from_utf8(&bytes[..end]).map_err(|_| Unread::NotHttp)?;
    let mut lines = head.split("\r\n");
    let mut parts = lines.next().unwrap_or("").split_whitespace();
    if !parts.next().is_some_and(|p| p.starts_with("HTTP/")) {
        return Err(Unread::NotHttp);
    }
    let status = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or(Unread::NotHttp)?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let mut body = bytes[end + 4..].to_vec();
    let said = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok());
    if let Some(said) = said {
        if body.len() < said {
            return Err(Unread::CutShort {
                said,
                got: body.len(),
            });
        }
        body.truncate(said);
    }
    Ok(Response {
        status,
        headers,
        body,
    })
}

/// The line said beside a frame: how large the remote's screen is, which
/// the picture is a scaled copy of, and how to turn a spot in the picture
/// into the screen position the input tools take. `image` is the
/// picture's own size, when it could be read.
pub fn screen_line(r: &Response, image: Option<(u32, u32)>) -> String {
    screen_line_of("remote", r, image)
}

/// [`screen_line`] for the screen of `whose`, which is also what its
/// tools' names begin with: `remote`, or `desk` for the agent's desk,
/// whose hand answers a frame the same way. An answer that says it is
/// of a region only is worded for that: what the answer says counts, not
/// what was asked, since a server from before regions answers the whole
/// screen whatever it is asked.
pub fn screen_line_of(whose: &str, r: &Response, image: Option<(u32, u32)>) -> String {
    let n = |k: &str| r.header(k).and_then(|v| v.parse::<i64>().ok());
    let (Some(w), Some(h)) = (n("x-screen-width"), n("x-screen-height")) else {
        return format!("The {whose} did not say how large its screen is.");
    };
    let screen = format!("The {whose} screen is {w} x {h} physical pixels.");
    let factor = |of: i64, iw: u32| {
        let factor = format!("{:.3}", of as f64 / f64::from(iw));
        factor
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    };
    if let (Some(left), Some(top), Some(rw), Some(rh)) = (
        n("x-region-left"),
        n("x-region-top"),
        n("x-region-width"),
        n("x-region-height"),
    ) {
        let part = format!(
            "only the part of that screen from {left},{top} to {},{}",
            left + rw,
            top + rh
        );
        let then = format!(
            "add {left} to x and {top} to y, for the x and y the {whose}_ input tools take, \
             counted from the screen's top left."
        );
        return match image {
            Some((iw, ih)) if iw > 0 && i64::from(iw) == rw => format!(
                "{screen} This image is {iw} x {ih} and shows {part}, unscaled: to image \
                 coordinates {then}"
            ),
            Some((iw, ih)) if iw > 0 => format!(
                "{screen} This image is {iw} x {ih} and shows {part}: multiply image \
                 coordinates by {}, then {then}",
                factor(rw, iw)
            ),
            _ => format!(
                "{screen} This image shows {part}: scale image coordinates by {rw} / the \
                 image's width, then {then}"
            ),
        };
    }
    match image {
        Some((iw, ih)) if iw > 0 => {
            let factor = factor(w, iw);
            format!(
                "{screen} This image is {iw} x {ih} of that screen: multiply image \
                 coordinates by {factor} for the x and y the {whose}_ input tools take, \
                 counted from the screen's top left."
            )
        }
        _ => format!(
            "{screen} Scale image coordinates by {w} / the image's width for the x and y \
             the {whose}_ input tools take, counted from the screen's top left."
        ),
    }
}

/// The width and height a JPEG says it has, from its start of frame
/// marker. None when the bytes are not a JPEG with one.
pub fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(..2)? != [0xff, 0xd8] {
        return None;
    }
    let mut at = 2;
    loop {
        // Fill bytes may stand before a marker.
        while *bytes.get(at)? == 0xff && *bytes.get(at + 1)? == 0xff {
            at += 1;
        }
        if *bytes.get(at)? != 0xff {
            return None;
        }
        let marker = *bytes.get(at + 1)?;
        let len = usize::from(u16::from_be_bytes([
            *bytes.get(at + 2)?,
            *bytes.get(at + 3)?,
        ]));
        // Every start of frame but the three that are not one: DHT, JPG, DAC.
        if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            let h = u16::from_be_bytes([*bytes.get(at + 5)?, *bytes.get(at + 6)?]);
            let w = u16::from_be_bytes([*bytes.get(at + 7)?, *bytes.get(at + 8)?]);
            return Some((u32::from(w), u32::from(h)));
        }
        if marker == 0xda || len < 2 {
            return None;
        }
        at += 2 + len;
    }
}

/// A coordinate or a count from a tool's arguments: any JSON number,
/// rounded, since a spot in a picture times a factor such as 1.6 is
/// seldom whole. None for what is no number or fits no screen.
pub fn whole(v: &Value) -> Option<i32> {
    let n = v.as_f64()?.round();
    (n.is_finite() && n >= f64::from(i32::MIN) && n <= f64::from(i32::MAX)).then_some(n as i32)
}

/// The body of one input request: `events` as a JSON array, which the
/// remote runs in order as one sequence, letting go of everything itself
/// when it fails midway. Refused here when the remote would refuse it.
pub fn batch_body(events: &[Value]) -> Result<String, String> {
    if events.len() > EVENTS_MAX {
        return Err(format!(
            "{} events are more than the {EVENTS_MAX} the remote runs at once",
            events.len()
        ));
    }
    let body = Value::Array(events.to_vec()).to_string();
    if body.len() >= BODY_MAX {
        return Err("the events are more than the 64 KiB the remote reads at once".into());
    }
    Ok(body)
}

/// The body that lets go of every key and button the remote holds down:
/// posted after an input request failed, so nothing stays pressed there.
pub const RELEASE_BODY: &str = r#"[{"kind":"release"}]"#;

/// The events of a click at `x`, `y` with `button` (left, right or
/// middle; left when None), twice over for a double click: a move first,
/// so the program under it sees the pointer arrive as it would by hand.
pub fn click_events(
    x: i32,
    y: i32,
    button: Option<&str>,
    double: bool,
) -> Result<Vec<Value>, String> {
    let button = match button.unwrap_or("left") {
        b @ ("left" | "right" | "middle") => b,
        other => return Err(format!("button is left, right or middle, not {other:?}")),
    };
    let mut out = vec![serde_json::json!({ "kind": "move", "x": x, "y": y })];
    for _ in 0..if double { 2 } else { 1 } {
        for kind in ["down", "up"] {
            out.push(serde_json::json!({ "kind": kind, "x": x, "y": y, "button": button }));
        }
    }
    Ok(out)
}

/// One notch of a mouse wheel, as Windows counts it.
pub const WHEEL_NOTCH: i32 = 120;

/// The most notches one scroll sends either way, more than any page
/// needs, and what the remote holds a wheel event to.
pub const WHEEL_MOST: i32 = 100;

/// The event of scrolling `notches` at `x`, `y`, negative being down, and
/// the notches it really carries once held to [`WHEEL_MOST`], so the
/// caller says what was sent and not what was asked.
pub fn wheel_event(x: i32, y: i32, notches: i32) -> Result<(Value, i32), String> {
    if notches == 0 {
        return Err("delta is a number of notches other than 0: negative scrolls down".into());
    }
    let sent = notches.clamp(-WHEEL_MOST, WHEEL_MOST);
    Ok((
        serde_json::json!({ "kind": "wheel", "x": x, "y": y, "delta": sent * WHEEL_NOTCH }),
        sent,
    ))
}

/// The most characters one text event carries. The remote reads a body
/// below 64 KiB, and a character takes 6 bytes at most in the JSON
/// serde_json writes: raw UTF-8 of up to 4 bytes, but a control character
/// as a six byte escape.
const TEXT_CHUNK: usize = 4000;

/// The events that type `text`, in pieces small enough for the remote to
/// read each, split between characters. A line break and a tab go as they
/// are: the remote presses Enter and Tab for them itself.
pub fn text_events(text: &str) -> Result<Vec<Value>, String> {
    if text.is_empty() {
        return Err("say what to type: text".into());
    }
    let chars: Vec<char> = text.chars().collect();
    Ok(chars
        .chunks(TEXT_CHUNK)
        .map(|c| {
            let piece: String = c.iter().collect();
            serde_json::json!({ "kind": "text", "text": piece })
        })
        .collect())
}

/// The key a name stands for: a virtual key, or a character whose key
/// depends on the keyboard layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    Vk(u16),
    Char(char),
}

/// A key with the modifiers held while it is pressed, in the order they
/// go down.
#[derive(Clone, Debug, PartialEq)]
pub struct Chord {
    pub mods: Vec<u16>,
    pub key: Key,
}

/// The virtual key of a modifier name, any case.
fn modifier_vk(name: &str) -> Option<u16> {
    Some(match name.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => 0x11,
        "shift" => 0x10,
        "alt" => 0x12,
        "win" | "windows" | "meta" | "super" | "cmd" => 0x5b,
        _ => return None,
    })
}

/// The key of a key name, any case: a letter, a digit, F1 to F24, a named
/// key or a modifier as a virtual key, any other single character as
/// itself.
fn key_of(name: &str) -> Option<Key> {
    let lower = name.to_ascii_lowercase();
    let mut chars = lower.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Some(match c {
            c if c.is_ascii_lowercase() => Key::Vk(c.to_ascii_uppercase() as u16),
            c if c.is_ascii_digit() => Key::Vk(c as u16),
            ' ' => Key::Vk(0x20),
            // Not lowered: a character is typed as it was written.
            _ => Key::Char(name.chars().next()?),
        });
    }
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u16>().ok()) {
        return (1..=24).contains(&n).then_some(Key::Vk(0x6f + n));
    }
    Some(Key::Vk(match lower.as_str() {
        "enter" | "return" => 0x0d,
        "tab" => 0x09,
        "escape" | "esc" => 0x1b,
        "backspace" => 0x08,
        "delete" | "del" => 0x2e,
        "insert" | "ins" => 0x2d,
        "space" => 0x20,
        "up" | "arrowup" => 0x26,
        "down" | "arrowdown" => 0x28,
        "left" | "arrowleft" => 0x25,
        "right" | "arrowright" => 0x27,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" | "pgup" => 0x21,
        "pagedown" | "pgdn" => 0x22,
        "printscreen" | "prtsc" | "prtscn" => 0x2c,
        "pause" => 0x13,
        "capslock" => 0x14,
        "numlock" => 0x90,
        "scrolllock" => 0x91,
        "menu" | "apps" | "contextmenu" => 0x5d,
        // Named, since + is what joins a chord and the others are easier
        // said than typed.
        "plus" => return Some(Key::Char('+')),
        "comma" => return Some(Key::Char(',')),
        "minus" => return Some(Key::Char('-')),
        "period" => return Some(Key::Char('.')),
        "slash" => return Some(Key::Char('/')),
        _ => modifier_vk(name)?,
    }))
}

/// A key or chord such as `Enter`, `Ctrl+S`, `Ctrl+Shift+Esc` or `Ctrl++`
/// as its modifiers, in the order written, and its key. A modifier alone
/// (`Win`) is a key too. This is the one reading of key names for every
/// family of tools; which keys a family then refuses, asks about or can
/// not send is its own business.
pub fn parse_key(spec: &str) -> Result<Chord, String> {
    // A plus joins a chord, so as the key it is the last character.
    let (mods, key) = match spec {
        "+" => (None, "+"),
        s if s.ends_with("++") => (Some(&s[..s.len() - 2]), "+"),
        s => match s.rsplit_once('+') {
            Some((mods, key)) => (Some(mods), key),
            None => (None, s),
        },
    };
    let mut held = Vec::new();
    for m in mods.into_iter().flat_map(|m| m.split('+')).map(str::trim) {
        let vk = modifier_vk(m).ok_or_else(|| {
            format!("{m:?} in {spec:?} is not Ctrl, Shift, Alt or Win, which come before the key")
        })?;
        if held.contains(&vk) {
            return Err(format!("{m:?} is in {spec:?} twice"));
        }
        held.push(vk);
    }
    // A space is a key by itself; around a name it is only spacing.
    let name = if key == " " { key } else { key.trim() };
    if name.is_empty() {
        return Err(format!("no key in {spec:?}"));
    }
    let key = key_of(name).ok_or_else(|| {
        format!(
            "{name:?} is not a key: a character, F1 to F24, Enter, Tab, Escape, Backspace, Delete, Insert, Space, Up, Down, Left, Right, Home, End, PageUp, PageDown, PrintScreen, Pause, CapsLock, NumLock, ScrollLock, Menu, Minus, Plus, Comma, Period, Slash, or a modifier"
        )
    })?;
    if let Key::Vk(vk) = key {
        if held.contains(&vk) {
            return Err(format!("{name:?} is in {spec:?} twice"));
        }
    }
    Ok(Chord { mods: held, key })
}

/// The virtual keys of a key or chord, in the order they go down: the
/// modifiers, then the key. For a keyboard whose layout is not known (a
/// remote's, the desk's), so of the characters only those that sit in the
/// same place on most layouts are keys.
pub fn parse_chord(chord: &str) -> Result<Vec<u16>, String> {
    let Chord { mods: mut vks, key } = parse_key(chord)?;
    vks.push(match key {
        Key::Vk(vk) => vk,
        Key::Char('=' | '+') => 0xbb,
        Key::Char(',') => 0xbc,
        Key::Char('-') => 0xbd,
        Key::Char('.') => 0xbe,
        Key::Char('/') => 0xbf,
        Key::Char(c) => {
            return Err(format!(
                "{c:?} is not a key here, where the keyboard's layout is not known: of the punctuation only - = , . / are (or Minus, Plus, Comma, Period, Slash). Type it as text instead"
            ))
        }
    });
    Ok(vks)
}

/// The events that press `vks` together: each down in order, then each up
/// in reverse, so the modifiers hold until the key is let go.
pub fn press_events(vks: &[u16]) -> Vec<Value> {
    let event = |vk: u16, down: bool| serde_json::json!({ "kind": "key", "vk": vk, "down": down });
    vks.iter()
        .map(|&vk| event(vk, true))
        .chain(vks.iter().rev().map(|&vk| event(vk, false)))
        .collect()
}

/// The most characters of what a remote said that go into a tool's
/// answer: its words are shown to a model, and a remote is not trusted to
/// keep them short.
const SAID_MOST: usize = 500;

/// What the remote said, trimmed and held to [`SAID_MOST`] characters.
pub fn brief(said: &str) -> String {
    let said = said.trim();
    if said.chars().count() <= SAID_MOST {
        return said.to_string();
    }
    let mut out: String = said.chars().take(SAID_MOST).collect();
    out.push_str("...");
    out
}

/// Why the remote did not take an input request, in words for the caller.
/// A 409 comes with the real reason, found by the remote before it sent
/// anything, so its words are the answer.
pub fn input_refusal(status: u16, said: &str) -> String {
    let said = brief(said);
    match status {
        409 if said.is_empty() => {
            "the remote's Windows did not take the input, and gave no reason".to_string()
        }
        409 => format!("the remote's Windows did not take the input: {said}"),
        400 => format!("the remote did not take the events: {said}"),
        413 => "the remote found the events too large".to_string(),
        s => status_refusal(s, &said),
    }
}

/// Why an answer other than the one hoped for came back, in words for the
/// caller, with what the remote `said` where that is the reason.
pub fn status_refusal(status: u16, said: &str) -> String {
    let said = brief(said);
    match status {
        403 => "the remote refused the request (403): its token is not the one read from it"
            .to_string(),
        404 => "the remote's Horadric does not serve its screen at that path: it may be older"
            .to_string(),
        423 if said.is_empty() => "the remote can not show its screen now".to_string(),
        423 => format!("the remote can not show its screen now: {said}"),
        503 => "the remote has too many viewers connected at once".to_string(),
        s if said.is_empty() => format!("the remote answered {s}"),
        s => format!("the remote answered {s}: {said}"),
    }
}

/// What tells a process from another that was later given its pid: when
/// it was created, as Windows counts it, or when it started, as `ps`
/// prints it elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Identity {
    Created(u64),
    Started(String),
}

/// Whether the process seen `now` under a tunnel's pid is the one `kept`
/// with it. A tunnel kept without an identity, by a Horadric from before
/// there was one, is never taken for ours: it is forgotten, not killed.
pub fn same_process(kept: Option<&Identity>, now: Option<&Identity>) -> bool {
    matches!((kept, now), (Some(a), Some(b)) if a == b)
}

/// One tunnel kept open: `ssh` process `pid` forwards `port` here to
/// `remote` on `dest`. `identity` is what the process was when it was
/// kept, checked before the pid is killed; absent in a file written
/// before it existed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tunnel {
    pub dest: String,
    pub remote: u16,
    pub port: u16,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Identity>,
}

/// The tunnels remembered in [`TUNNELS_FILE`]. A file that does not read
/// is no tunnels, which only means a fresh one is started.
pub fn parse_tunnels(text: &str) -> Vec<Tunnel> {
    serde_json::from_str(text).unwrap_or_default()
}

/// The tunnels as the file keeps them.
pub fn tunnels_text(list: &[Tunnel]) -> String {
    let mut out = serde_json::to_string_pretty(list).unwrap_or_else(|_| "[]".into());
    out.push('\n');
    out
}

/// The tunnel remembered for `dest` and `remote`, if any.
pub fn find_tunnel<'a>(list: &'a [Tunnel], dest: &str, remote: u16) -> Option<&'a Tunnel> {
    list.iter().find(|t| t.dest == dest && t.remote == remote)
}

/// `list` with `t` in place of any other for the same destination and
/// remote port.
pub fn with_tunnel(list: &[Tunnel], t: Tunnel) -> Vec<Tunnel> {
    let mut out: Vec<Tunnel> = list
        .iter()
        .filter(|o| !(o.dest == t.dest && o.remote == t.remote))
        .cloned()
        .collect();
    out.push(t);
    out
}

/// `list` without the tunnel for `dest` and `remote`, for closing it.
pub fn without_tunnel(list: &[Tunnel], dest: &str, remote: u16) -> Vec<Tunnel> {
    list.iter()
        .filter(|t| !(t.dest == dest && t.remote == remote))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_bad_key_is_told_without_runs_of_spaces() {
        for spec in ["Florp", "Ctrl+;", "!"] {
            let said = parse_chord(spec).unwrap_err();
            assert!(!said.contains("  "), "{said}");
        }
    }

    use super::*;

    use serde_json::{json, Value};

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn values(events: &[Value]) -> Vec<Value> {
        events.to_vec()
    }

    #[test]
    fn dev_forwards_to_its_own_port() {
        assert_eq!(remote_port(false), 43119);
        assert_eq!(remote_port(true), 43120);
    }

    #[test]
    fn a_destination_ssh_would_misread_is_refused() {
        assert_eq!(refusal("me@vm"), None);
        assert!(refusal("-oProxyCommand=x").is_some());
        assert!(refusal("a b").is_some());
        assert!(refusal("").is_some());
    }

    #[test]
    fn a_destination_is_only_what_a_host_is_written_with() {
        for ok in [
            "vm",
            "VM",
            "me@vm.example.com",
            "build_box-2",
            "me@[fe80::1]",
            "10.0.0.4",
        ] {
            assert_eq!(refusal(ok), None, "{ok}");
        }
        for bad in [
            "-vm",
            "vm;calc",
            "vm|x",
            "vm&x",
            "vm$x",
            "vm`x`",
            "vm\"x",
            "vm'x",
            "vm%x",
            "vm\\x",
            "vm/x",
            "vm\tx",
            "vm\nx",
            "v\u{e6}m",
            "vm\u{202e}",
            "vm x",
            // Nothing is tidied: what is kept is what was written.
            " vm",
        ] {
            assert!(refusal(bad).is_some(), "{bad:?}");
        }
    }

    #[test]
    fn the_tunnel_forwards_to_the_remote_loopback_and_ends_on_failure() {
        let a = tunnel_args("vm", 50000, 43119);
        assert_eq!(a[0], "-N");
        assert!(a.contains(&"BatchMode=yes".to_string()));
        assert!(a.contains(&"ExitOnForwardFailure=yes".to_string()));
        let l = a.iter().position(|x| x == "-L").unwrap();
        assert_eq!(a[l + 1], "127.0.0.1:50000:127.0.0.1:43119");
        assert_eq!(&a[a.len() - 2..], ["--", "vm"]);
    }

    #[test]
    fn the_token_is_read_through_cmd_from_the_right_folder() {
        let a = token_args("vm", false);
        assert!(a.contains(&"BatchMode=yes".to_string()));
        assert_eq!(a[a.len() - 3..a.len() - 1], ["--", "vm"]);
        assert_eq!(
            a.last().unwrap(),
            r#"cmd /c type "%APPDATA%\Horadric\screen-token""#
        );
        assert!(token_args("vm", true)
            .last()
            .unwrap()
            .contains(r"%APPDATA%\Horadric-dev\screen-token"));
    }

    #[test]
    fn a_token_is_32_hex_and_nothing_else() {
        assert_eq!(parse_token(&format!("{TOKEN}\r\n")).as_deref(), Some(TOKEN));
        assert_eq!(
            parse_token(&format!("\u{feff}{TOKEN}\n")).as_deref(),
            Some(TOKEN)
        );
        // The remote writes lower case, and the proof is keyed byte for byte.
        assert_eq!(parse_token(&TOKEN.to_uppercase()), None);
        assert_eq!(parse_token(&TOKEN[..31]), None);
        assert_eq!(parse_token(&format!("{TOKEN}0")), None);
        assert_eq!(parse_token(&TOKEN.replace('a', "g")), None);
        assert_eq!(
            parse_token("The system cannot find the file specified."),
            None
        );
        assert_eq!(parse_token(""), None);
    }

    #[test]
    fn the_token_is_in_the_fragment_and_a_header_and_masking_hides_it() {
        let url = view_url(50123, TOKEN);
        assert_eq!(url, format!("http://127.0.0.1:50123/#t={TOKEN}"));
        assert_eq!(mask(&url, TOKEN), "http://127.0.0.1:50123/#t=<token>");
        assert_eq!(frame_path(800), "/frame.jpg?w=800");
        assert_eq!(mask("abc", ""), "abc");
        assert_eq!(
            request_head("GET", 50123, DISPLAY_PATH, Some(TOKEN), None),
            format!(
                "GET /display HTTP/1.0\r\nHost: 127.0.0.1:50123\r\nConnection: close\r\n\
                 Authorization: Bearer {TOKEN}\r\n\r\n"
            )
        );
        assert_eq!(
            request_head("POST", 7, INPUT_PATH, Some(TOKEN), Some(12)),
            format!(
                "POST /input HTTP/1.0\r\nHost: 127.0.0.1:7\r\nConnection: close\r\n\
                 Authorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\n\
                 Content-Length: 12\r\n\r\n"
            )
        );
    }

    #[test]
    fn the_proof_is_asked_without_the_token_and_checked_whole() {
        let nonce = hex(&[0xab; 16]);
        assert_eq!(nonce.len(), 32);
        assert_eq!(hello_path(&nonce), format!("/hello?n={nonce}"));
        let head = request_head("GET", 9, &hello_path(&nonce), None, None);
        assert!(!head.contains(TOKEN) && !head.contains("Authorization"));
        // RFC 4231, test case 2, so the proof is the HMAC the remote makes.
        assert_eq!(
            proof("Jefe", "what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        let right = proof(TOKEN, &nonce);
        assert_eq!(right.len(), 64);
        assert!(proven(TOKEN, &nonce, right.as_bytes()));
        assert!(proven(TOKEN, &nonce, format!("{right}\r\n").as_bytes()));
        assert!(!proven(TOKEN, &nonce, right.to_uppercase().as_bytes()));
        assert!(!proven(TOKEN, &nonce, &right.as_bytes()[..63]));
        assert!(!proven(TOKEN, &nonce, format!("{right}0").as_bytes()));
        assert!(!proven(TOKEN, &nonce, b""));
        assert!(!proven(TOKEN, &hex(&[0xac; 16]), right.as_bytes()));
        let other = proof(&TOKEN.replace('0', "1"), &nonce);
        assert!(!proven(TOKEN, &nonce, other.as_bytes()));
    }

    #[test]
    fn reading_stops_at_the_length_said_and_at_the_cap() {
        let with = |head: &[u8], body: &[u8]| [head, body].concat();
        assert_eq!(whole_answer(b"", 10), Ok(false));
        assert_eq!(
            whole_answer(b"HTTP/1.0 200 OK\r\nContent-Le", 10),
            Ok(false)
        );
        let head = b"HTTP/1.0 200 OK\r\nContent-Length: 5\r\n\r\n";
        assert_eq!(whole_answer(head, 10), Ok(false));
        assert_eq!(whole_answer(&with(head, b"1234"), 10), Ok(false));
        assert_eq!(whole_answer(&with(head, b"12345"), 10), Ok(true));
        assert_eq!(
            whole_answer(b"HTTP/1.0 204 No\r\ncontent-length:0\r\n\r\n", 10),
            Ok(true)
        );
        // A length over the cap is refused before a byte of it is read.
        assert_eq!(
            whole_answer(b"HTTP/1.0 200 OK\r\nContent-Length: 11\r\n\r\n", 10),
            Err(Unread::TooLarge { cap: 10 })
        );
        // With no length, or one that does not read, the peer's close
        // ends it, within the cap.
        for unsaid in [
            b"HTTP/1.0 200 OK\r\n\r\n".as_slice(),
            b"HTTP/1.0 200 OK\r\nContent-Length: 99999999999999999999999\r\n\r\n",
        ] {
            assert_eq!(whole_answer(&with(unsaid, &[b'x'; 10]), 10), Ok(false));
            assert_eq!(
                whole_answer(&with(unsaid, &[b'x'; 11]), 10),
                Err(Unread::TooLarge { cap: 10 })
            );
        }
        // A head that never ends is not waited for.
        assert_eq!(whole_answer(&vec![b'x'; HEAD_CAP], 10), Ok(false));
        assert_eq!(
            whole_answer(&vec![b'x'; HEAD_CAP + 1], 10),
            Err(Unread::TooLarge { cap: HEAD_CAP })
        );
    }

    #[test]
    fn an_http_answer_is_read_into_status_headers_and_body() {
        let raw = b"HTTP/1.0 200 OK\r\nContent-Type: image/jpeg\r\nX-Screen-Width: 2560\r\nX-Screen-Height:1440\r\nX-Screen-Left: -1920\r\n\r\n\xff\xd8\xff";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.header("content-type"), Some("image/jpeg"));
        assert_eq!(r.header("X-SCREEN-HEIGHT"), Some("1440"));
        assert_eq!(r.body, b"\xff\xd8\xff");
        assert_eq!(
            screen_line(&r, Some((1600, 900))),
            "The remote screen is 2560 x 1440 physical pixels. This image is 1600 x 900 of \
             that screen: multiply image coordinates by 1.6 for the x and y the remote_ input \
             tools take, counted from the screen's top left."
        );
        assert!(screen_line(&r, Some((2560, 1440))).contains("multiply image coordinates by 1 "));
        assert!(screen_line(&r, Some((1000, 563))).contains("by 2.56 "));
        assert!(screen_line(&r, None).contains("2560 / the image's width"));
    }

    #[test]
    fn a_frame_of_a_region_says_how_to_get_back_to_the_screen() {
        let raw = b"HTTP/1.0 200 OK\r\nX-Screen-Width: 3200\r\nX-Screen-Height: 1800\r\nX-Region-Left: 100\r\nX-Region-Top: 50\r\nX-Region-Width: 400\r\nX-Region-Height: 300\r\n\r\n\xff\xd8\xff";
        let r = parse_response(raw).unwrap();
        assert_eq!(
            screen_line_of("desk", &r, Some((400, 300))),
            "The desk screen is 3200 x 1800 physical pixels. This image is 400 x 300 and \
             shows only the part of that screen from 100,50 to 500,350, unscaled: to image \
             coordinates add 100 to x and 50 to y, for the x and y the desk_ input tools \
             take, counted from the screen's top left."
        );
        assert_eq!(
            screen_line_of("desk", &r, Some((200, 150))),
            "The desk screen is 3200 x 1800 physical pixels. This image is 200 x 150 and \
             shows only the part of that screen from 100,50 to 500,350: multiply image \
             coordinates by 2, then add 100 to x and 50 to y, for the x and y the desk_ \
             input tools take, counted from the screen's top left."
        );
        let blind = screen_line_of("desk", &r, None);
        assert!(
            blind.contains("by 400 / the image's width, then add 100 to x"),
            "{blind}"
        );
    }

    #[test]
    fn a_frame_is_asked_for_whole_or_in_part_scaled_or_not() {
        let region = Region {
            x: 100,
            y: 50,
            width: 400,
            height: 300,
        };
        assert_eq!(frame_path_of(None, None), "/frame.jpg");
        assert_eq!(frame_path_of(Some(800), None), frame_path(800));
        assert_eq!(
            frame_path_of(None, Some(region)),
            "/frame.jpg?rx=100&ry=50&rw=400&rh=300"
        );
        assert_eq!(
            frame_path_of(Some(200), Some(region)),
            "/frame.jpg?w=200&rx=100&ry=50&rw=400&rh=300"
        );
    }

    /// A JPEG head with a start of frame `marker` of `w` by `h`, after an
    /// APP0 segment, as an encoder writes them.
    fn jpeg(marker: u8, w: u16, h: u16) -> Vec<u8> {
        let mut b = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0xaa, 0xbb];
        b.extend([0xff, 0xc4, 0x00, 0x03, 0x00]);
        b.extend([0xff, marker, 0x00, 0x11, 0x08]);
        b.extend(h.to_be_bytes());
        b.extend(w.to_be_bytes());
        b.extend([0x03, 0x01]);
        b
    }

    #[test]
    fn a_jpeg_says_its_size_in_its_start_of_frame() {
        assert_eq!(jpeg_size(&jpeg(0xc0, 1600, 900)), Some((1600, 900)));
        assert_eq!(jpeg_size(&jpeg(0xc2, 800, 451)), Some((800, 451)));
        let mut filled = jpeg(0xc0, 10, 20);
        filled.insert(8, 0xff);
        assert_eq!(jpeg_size(&filled), Some((10, 20)));
        assert_eq!(jpeg_size(&jpeg(0xc0, 10, 20)[..15]), None);
        assert_eq!(jpeg_size(b"\x89PNG\r\n"), None);
        assert_eq!(jpeg_size(&[0xff, 0xd8, 0xff, 0xda, 0x00, 0x02]), None);
        assert_eq!(jpeg_size(&[]), None);
    }

    #[test]
    fn a_click_moves_then_presses_and_lets_go() {
        let one = values(&click_events(812, 440, None, false).unwrap());
        assert_eq!(
            one,
            [
                json!({ "kind": "move", "x": 812, "y": 440 }),
                json!({ "kind": "down", "x": 812, "y": 440, "button": "left" }),
                json!({ "kind": "up", "x": 812, "y": 440, "button": "left" }),
            ]
        );
        let double = values(&click_events(1, 2, Some("right"), true).unwrap());
        assert_eq!(double.len(), 5);
        assert_eq!(double[3], double[1]);
        assert_eq!(
            double[4],
            json!({ "kind": "up", "x": 1, "y": 2, "button": "right" })
        );
        assert!(click_events(1, 2, Some("back"), false).is_err());
    }

    #[test]
    fn a_scroll_counts_notches_down_as_negative() {
        assert_eq!(
            wheel_event(5, 6, -3).unwrap(),
            (
                json!({ "kind": "wheel", "x": 5, "y": 6, "delta": -360 }),
                -3
            )
        );
        let sent = |n| {
            let (event, notches) = wheel_event(0, 0, n).unwrap();
            (event["delta"].as_i64().unwrap(), notches)
        };
        assert_eq!(sent(1), (120, 1));
        assert_eq!(sent(1000), (12000, 100));
        assert_eq!(sent(-1000), (-12000, -100));
        assert!(wheel_event(0, 0, 0).is_err());
    }

    #[test]
    fn a_coordinate_is_any_number_rounded() {
        assert_eq!(whole(&json!(812)), Some(812));
        assert_eq!(whole(&json!(1299.2)), Some(1299));
        assert_eq!(whole(&json!(1299.5)), Some(1300));
        assert_eq!(whole(&json!(-0.4)), Some(0));
        assert_eq!(whole(&json!(-1920.6)), Some(-1921));
        assert_eq!(whole(&json!(1e12)), None);
        assert_eq!(whole(&json!("812")), None);
        assert_eq!(whole(&json!(null)), None);
    }

    #[test]
    fn a_sequence_is_one_body_the_remote_reads_whole() {
        let click = click_events(1, 2, None, true).unwrap();
        let body = batch_body(&click).unwrap();
        let read: Vec<Value> = serde_json::from_str(&body).unwrap();
        assert_eq!(read, click);
        assert!(batch_body(&vec![json!({ "kind": "alive" }); EVENTS_MAX]).is_ok());
        assert!(batch_body(&vec![json!({ "kind": "alive" }); EVENTS_MAX + 1]).is_err());
        let large = json!({ "kind": "text", "text": "x".repeat(BODY_MAX) });
        assert!(batch_body(&[large]).is_err());
        assert_eq!(
            serde_json::from_str::<Value>(RELEASE_BODY).unwrap(),
            json!([{ "kind": "release" }])
        );
    }

    #[test]
    fn text_goes_in_pieces_the_remote_reads_whole() {
        assert_eq!(
            values(&text_events("æøå \"x\"").unwrap()),
            [json!({ "kind": "text", "text": "æøå \"x\"" })]
        );
        let long = "\u{1F600}".repeat(TEXT_CHUNK + 1);
        let pieces = text_events(&long).unwrap();
        assert_eq!(pieces.len(), 2);
        assert!(pieces.iter().all(|p| batch_body(&[p.clone()]).is_ok()));
        // The worst a character costs: a control character, escaped.
        let worst = text_events(&"\u{1}".repeat(TEXT_CHUNK)).unwrap();
        assert_eq!(worst.len(), 1);
        assert!(batch_body(&worst).unwrap().len() > TEXT_CHUNK * 6);
        // Enter and Tab are the remote's to press.
        assert_eq!(
            values(&text_events("a\tb\nc").unwrap()),
            [json!({ "kind": "text", "text": "a\tb\nc" })]
        );
        assert!(text_events("").is_err());
    }

    #[test]
    fn keys_are_named_as_on_the_keyboard() {
        assert_eq!(parse_chord("a"), Ok(vec![0x41]));
        assert_eq!(parse_chord("Z"), Ok(vec![0x5a]));
        assert_eq!(parse_chord("0"), Ok(vec![0x30]));
        assert_eq!(parse_chord("9"), Ok(vec![0x39]));
        assert_eq!(parse_chord("F1"), Ok(vec![0x70]));
        assert_eq!(parse_chord("f5"), Ok(vec![0x74]));
        assert_eq!(parse_chord("F24"), Ok(vec![0x87]));
        assert!(parse_chord("F0").is_err());
        assert!(parse_chord("F25").is_err());
        for (name, vk) in [
            ("Enter", 0x0d),
            ("Return", 0x0d),
            ("Tab", 0x09),
            ("Escape", 0x1b),
            ("Esc", 0x1b),
            ("Backspace", 0x08),
            ("Delete", 0x2e),
            ("Insert", 0x2d),
            ("Space", 0x20),
            ("Up", 0x26),
            ("Down", 0x28),
            ("Left", 0x25),
            ("ArrowRight", 0x27),
            ("Home", 0x24),
            ("End", 0x23),
            ("PageUp", 0x21),
            ("PageDown", 0x22),
            ("Win", 0x5b),
            ("PrintScreen", 0x2c),
            ("Pause", 0x13),
            ("CapsLock", 0x14),
            ("NumLock", 0x90),
            ("ScrollLock", 0x91),
            ("Menu", 0x5d),
            ("-", 0xbd),
            ("Minus", 0xbd),
            ("=", 0xbb),
            ("Plus", 0xbb),
            (",", 0xbc),
            (".", 0xbe),
            ("/", 0xbf),
        ] {
            // What the remote refuses must not be reachable by a name.
            let refused = matches!(vk, 0 | 1 | 2 | 4 | 5 | 6 | 0x5f) || (0xa6..=0xb7).contains(&vk);
            assert!(!refused, "{name}");
            assert_eq!(parse_chord(name), Ok(vec![vk]), "{name}");
            assert_eq!(parse_chord(&name.to_uppercase()), Ok(vec![vk]), "{name}");
        }
    }

    #[test]
    fn chords_hold_their_modifiers_in_order() {
        assert_eq!(parse_chord("Ctrl+S"), Ok(vec![0x11, 0x53]));
        assert_eq!(parse_chord("Alt+F4"), Ok(vec![0x12, 0x73]));
        assert_eq!(parse_chord("Ctrl+Shift+Esc"), Ok(vec![0x11, 0x10, 0x1b]));
        assert_eq!(parse_chord("Win+R"), Ok(vec![0x5b, 0x52]));
        assert_eq!(
            parse_chord("ctrl + alt + delete"),
            Ok(vec![0x11, 0x12, 0x2e])
        );
        assert_eq!(parse_chord("Ctrl+-"), Ok(vec![0x11, 0xbd]));
        assert_eq!(parse_chord("Ctrl+Plus"), Ok(vec![0x11, 0xbb]));
        assert_eq!(parse_chord("Ctrl+/"), Ok(vec![0x11, 0xbf]));
        assert!(parse_chord("S+Ctrl").is_err());
        assert!(parse_chord("Ctrl+Ctrl+S").is_err());
        assert!(parse_chord("Ctrl+Ctrl").is_err());
        assert!(parse_chord("Ctrl+").is_err());
        // A plus as the key, which only its name gave before.
        assert_eq!(parse_chord("Ctrl++"), Ok(vec![0x11, 0xbb]));
        assert_eq!(parse_chord("Cmd+ContextMenu"), Ok(vec![0x5b, 0x5d]));
        // A character whose key depends on the layout is no key here.
        assert!(parse_chord("?")
            .unwrap_err()
            .contains("layout is not known"));
        assert!(parse_chord("Ctrl+NotAKey")
            .unwrap_err()
            .contains("is not a key"));
    }

    #[test]
    fn one_parser_reads_a_key_for_every_family() {
        let (ctrl, shift, alt) = (0x11, 0x10, 0x12);
        let k = |s: &str| parse_key(s).unwrap();
        assert_eq!(
            k("Control+S"),
            Chord {
                mods: vec![ctrl],
                key: Key::Vk(b'S' as u16)
            }
        );
        assert_eq!(
            k("Alt+F4"),
            Chord {
                mods: vec![alt],
                key: Key::Vk(0x73)
            }
        );
        assert_eq!(k("Enter").key, Key::Vk(0x0d));
        assert_eq!(k("F5").key, Key::Vk(0x74));
        assert_eq!(k("f24").key, Key::Vk(0x87));
        assert_eq!(k("Shift+Tab").mods, vec![shift]);
        assert_eq!(k("a").key, Key::Vk(b'A' as u16));
        assert_eq!(k("7").key, Key::Vk(b'7' as u16));
        // Any other character is itself, for whoever knows the layout,
        // and in the case it was written.
        assert_eq!(k("?").key, Key::Char('?'));
        assert_eq!(k("Æ").key, Key::Char('Æ'));
        assert_eq!(k("+").key, Key::Char('+'));
        assert_eq!(k("Plus").key, Key::Char('+'));
        assert_eq!(k("minus").key, Key::Char('-'));
        assert_eq!(
            k("ctrl++"),
            Chord {
                mods: vec![ctrl],
                key: Key::Char('+')
            }
        );
        assert_eq!(k(" ").key, Key::Vk(0x20));
        assert_eq!(k("Ctrl+ "), k("Ctrl+Space"));
        assert_eq!(k("ctrl + shift + down"), k("Ctrl+Shift+Down"));
        assert_eq!(k("Control+Shift+down").mods, vec![ctrl, shift]);
        // A modifier alone is a key: Alt opens the menu bar.
        assert_eq!(
            k("Alt"),
            Chord {
                mods: vec![],
                key: Key::Vk(alt)
            }
        );
        assert!(parse_key("Control+Control+S")
            .unwrap_err()
            .contains("twice"));
        assert!(parse_key("Hyper+a").is_err());
        assert!(parse_key("NotAKey").is_err());
        assert!(parse_key("F25").is_err());
        assert!(parse_key("Control+").is_err());
        assert!(parse_key("+S").is_err());
        assert!(parse_key("").is_err());
        assert!(parse_chord("").is_err());
        assert!(parse_chord("Hyper+S").is_err());
        assert!(parse_chord("Ctrl+Enterr").is_err());
    }

    #[test]
    fn a_chord_goes_down_in_order_and_up_in_reverse() {
        let key = |vk: u16, down: bool| json!({ "kind": "key", "vk": vk, "down": down });
        assert_eq!(
            values(&press_events(&[0x11, 0x53])),
            [
                key(0x11, true),
                key(0x53, true),
                key(0x53, false),
                key(0x11, false)
            ]
        );
    }

    #[test]
    fn input_refusals_pass_on_what_the_remote_said() {
        assert_eq!(
            input_refusal(409, "the desktop is locked or shows a UAC prompt\n"),
            "the remote's Windows did not take the input: the desktop is locked or shows a \
             UAC prompt"
        );
        assert!(input_refusal(409, "").contains("gave no reason"));
        assert!(input_refusal(400, "no such key: 0").ends_with("no such key: 0"));
        assert!(input_refusal(413, "").contains("too large"));
        assert!(input_refusal(403, "").contains("token"));
    }

    #[test]
    fn what_a_remote_says_is_held_short() {
        assert_eq!(brief("  locked \r\n"), "locked");
        let long = "\u{e6}".repeat(5000);
        let held = brief(&long);
        assert_eq!(held.chars().count(), SAID_MOST + 3);
        assert!(held.ends_with("..."));
        assert!(input_refusal(409, &long).chars().count() < SAID_MOST + 100);
        assert!(status_refusal(500, &long).chars().count() < SAID_MOST + 100);
    }

    #[test]
    fn nothing_or_not_http_is_no_answer() {
        assert_eq!(parse_response(b""), Err(Unread::NotHttp));
        assert_eq!(parse_response(b"HTTP/1.0 200 OK\r\n"), Err(Unread::NotHttp));
        assert_eq!(
            parse_response(b"SSH-2.0-OpenSSH\r\n\r\n"),
            Err(Unread::NotHttp)
        );
        assert_eq!(parse_response(b"HTTP/1.0\r\n\r\n"), Err(Unread::NotHttp));
        let r = parse_response(b"HTTP/1.1 403 Forbidden\r\n\r\n").unwrap();
        assert_eq!(r.status, 403);
        assert!(r.body.is_empty());
        assert_eq!(
            screen_line(&r, Some((10, 10))),
            "The remote did not say how large its screen is."
        );
    }

    #[test]
    fn a_body_is_held_to_its_content_length() {
        let short = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n12345";
        assert_eq!(
            parse_response(short),
            Err(Unread::CutShort { said: 10, got: 5 })
        );
        let exact = b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\n12345";
        assert_eq!(parse_response(exact).unwrap().body, b"12345");
        let long = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n12345";
        assert_eq!(parse_response(long).unwrap().body, b"123");
        let unsaid = b"HTTP/1.0 200 OK\r\n\r\n12345";
        assert_eq!(parse_response(unsaid).unwrap().body, b"12345");
    }

    #[test]
    fn refusals_say_why() {
        assert!(status_refusal(403, "").contains("token"));
        assert!(status_refusal(404, "").contains("older"));
        assert_eq!(status_refusal(500, ""), "the remote answered 500");
        assert_eq!(
            status_refusal(500, " broke \n"),
            "the remote answered 500: broke"
        );
        assert!(status_refusal(423, "the desktop is locked").ends_with(": the desktop is locked"));
        assert!(status_refusal(503, "").contains("too many"));
    }

    #[test]
    fn tunnels_are_kept_one_per_destination_and_port() {
        let a = Tunnel {
            dest: "vm".into(),
            remote: PORT,
            port: 50000,
            pid: 10,
            identity: Some(Identity::Created(133_800_000_000_000_000)),
        };
        let dev = Tunnel {
            remote: DEV_PORT,
            port: 50001,
            pid: 11,
            identity: Some(Identity::Started("Thu Nov 26 09:14:02 2026".into())),
            ..a.clone()
        };
        let list = with_tunnel(&with_tunnel(&[], a.clone()), dev.clone());
        assert_eq!(list.len(), 2);
        let newer = Tunnel {
            port: 50002,
            pid: 12,
            ..a.clone()
        };
        let list = with_tunnel(&list, newer.clone());
        assert_eq!(list.len(), 2);
        assert_eq!(find_tunnel(&list, "vm", PORT), Some(&newer));
        assert_eq!(find_tunnel(&list, "vm", DEV_PORT), Some(&dev));
        assert_eq!(find_tunnel(&list, "other", PORT), None);
        assert_eq!(parse_tunnels(&tunnels_text(&list)), list);
        let closed = without_tunnel(&list, "vm", PORT);
        assert_eq!(closed, vec![dev.clone()]);
        assert_eq!(without_tunnel(&closed, "other", PORT), closed);
    }

    #[test]
    fn a_tunnel_keeps_what_its_process_was_and_an_old_one_still_reads() {
        let old = r#"[{"dest":"vm","remote":43119,"port":50000,"pid":10}]"#;
        let read = parse_tunnels(old);
        assert_eq!(read.len(), 1);
        assert_eq!((read[0].pid, &read[0].identity), (10, &None));
        // Written back as it was: no field where there is no identity.
        assert!(!tunnels_text(&read).contains("identity"));
        assert_eq!(parse_tunnels(&tunnels_text(&read)), read);

        let windows =
            r#"[{"dest":"vm","remote":43119,"port":50000,"pid":10,"identity":133800000000000000}]"#;
        let created = Identity::Created(133_800_000_000_000_000);
        assert_eq!(parse_tunnels(windows)[0].identity, Some(created.clone()));
        let unix = r#"[{"dest":"vm","remote":43119,"port":50000,"pid":10,"identity":"Thu Nov 26 09:14:02 2026"}]"#;
        let started = Identity::Started("Thu Nov 26 09:14:02 2026".into());
        assert_eq!(parse_tunnels(unix)[0].identity, Some(started.clone()));
        for text in [windows, unix] {
            let list = parse_tunnels(text);
            assert_eq!(parse_tunnels(&tunnels_text(&list)), list);
        }

        assert!(same_process(Some(&created), Some(&created)));
        assert!(!same_process(Some(&created), Some(&Identity::Created(1))));
        assert!(!same_process(Some(&created), Some(&started)));
        assert!(!same_process(Some(&created), None), "the process is gone");
        assert!(
            !same_process(None, Some(&created)),
            "an old record is never ours"
        );
        assert!(!same_process(None, None));
    }

    #[test]
    fn an_unreadable_tunnels_file_is_no_tunnels() {
        assert!(parse_tunnels("").is_empty());
        assert!(parse_tunnels("{not json").is_empty());
        assert!(parse_tunnels(r#"{"dest":"vm"}"#).is_empty());
    }
}
