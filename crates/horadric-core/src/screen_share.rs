//! The pure half of screen sharing: the app serving its own desktop to a
//! viewer page, for a machine that watches it through an SSH tunnel. What
//! a request asks for, whether it may be answered, what its input events
//! say and where they land. Capturing and injecting are Win32 and live in
//! the UI crate.

use serde::{Deserialize, Serialize};

/// The user environment variable that turns serving on. Off unless it is
/// exactly `1`, so a normal install serves nothing new.
pub const ENV: &str = "HORADRIC_SCREEN_SHARE";

/// The installed app's port, on 127.0.0.1 only.
pub const PORT: u16 = 43119;

/// A dev instance's port, so it serves beside the installed one.
pub const DEV_PORT: u16 = 43120;

/// The token's file in the state folder.
pub const TOKEN_FILE: &str = "screen-token";

/// The longest body `/input` reads. One event is a few dozen bytes; text
/// pasted in one go is the only thing that grows.
pub const MAX_BODY: usize = 64 * 1024;

/// The most events one `/input` body carries.
pub const MAX_EVENTS: usize = 256;

/// The furthest one wheel event turns, a hundred notches either way. A
/// page cannot ask for a scroll that takes the program minutes to play.
pub const MAX_WHEEL: i32 = 12_000;

/// How long keys and buttons stay down with nothing heard on `/input`,
/// in milliseconds. A viewer that died mid drag must not leave the
/// button down for whoever sits at the machine next.
pub const HELD_FOR_MS: u64 = 5_000;

/// The seconds a new resolution lasts unless it is kept.
pub const KEEP_WITHIN: u64 = 15;

/// The silence, in milliseconds, after which a request counts as a
/// viewer arriving, and an input as driving beginning, once more.
pub const QUIET_MS: u64 = 60_000;

/// The audit log in the state folder.
pub const LOG_FILE: &str = "screen-share.log";

/// The size the audit log is put aside at, for a new one to start.
pub const LOG_MAX: u64 = 1024 * 1024;

/// Moves a log that grew past [`LOG_MAX`] aside before the next line,
/// keeping one old file beside it (`name.1`), so an audit log is never
/// more than twice that. Shared by the logs of the state folder that grow
/// for as long as Horadric is installed, and by the elevated helper's, so
/// it lives here where the app and the binary both reach it.
pub fn roll(path: &std::path::Path) {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > LOG_MAX) {
        let mut aside = path.as_os_str().to_owned();
        aside.push(".1");
        let _ = std::fs::rename(path, aside);
    }
}

/// The most connections served at once.
pub const MAX_CONNECTIONS: usize = 16;

/// Whether serving is on, from [`ENV`]'s value.
pub fn enabled(value: Option<&str>) -> bool {
    value == Some("1")
}

/// The port to serve on.
pub fn port(dev: bool) -> u16 {
    if dev {
        DEV_PORT
    } else {
        PORT
    }
}

/// Sixteen random bytes as the 32 hex characters the token file holds.
pub fn token_hex(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The token in an `Authorization` header's value, which is the only
/// place one is read from: a query lands in histories and logs.
pub fn bearer(value: &str) -> Option<&str> {
    let (scheme, token) = value.trim().split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

/// The nonce `/hello` was asked with, when it is one: 32 hex characters.
pub fn hello_nonce(query: &str) -> Option<String> {
    query_param(query, "n").filter(|n| n.len() == 32 && n.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// What `/hello` answers: proof of knowing the token without saying it,
/// so a viewer can tell this server from whatever else took the port
/// before it sends the token there.
pub fn hello_proof(token: &str, nonce: &str) -> String {
    crate::webpush::hmac_sha256(token.as_bytes(), nonce.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Whether a Host header names this machine by its loopback name. A page
/// on another site whose name was pointed at 127.0.0.1 arrives with that
/// site's name here.
pub fn host_ok(host: &str) -> bool {
    let host = host.trim();
    let (name, port) = match host.rsplit_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (host, None),
    };
    (name == "127.0.0.1" || name.eq_ignore_ascii_case("localhost"))
        && port.is_none_or(|p| (1..=5).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether a request may be looked at at all, from its Host and Origin.
/// A browser names the page a request came from in Origin, so one from
/// any page but the viewer itself stops here.
pub fn request_allowed(host: Option<&str>, origin: Option<&str>) -> bool {
    let Some(host) = host.filter(|h| host_ok(h)) else {
        return false;
    };
    origin.is_none_or(|o| {
        (o.trim().strip_prefix("http://")).is_some_and(|o| o.eq_ignore_ascii_case(host.trim()))
    })
}

/// Whether this is the first sign of life after [`QUIET_MS`] of none,
/// from when the one before came and now, both in milliseconds.
pub fn after_quiet(last: Option<u64>, now: u64) -> bool {
    last.is_none_or(|l| now.saturating_sub(l) >= QUIET_MS)
}

/// Whether a request's token is the right one. It compares every byte
/// whatever the first difference, so the time an answer takes says
/// nothing about how much of a guess was right.
pub fn token_ok(given: Option<&str>, expected: &str) -> bool {
    let Some(given) = given else {
        return false;
    };
    let (a, b) = (given.as_bytes(), expected.as_bytes());
    if a.len() != b.len() || b.is_empty() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A request target split into its path and its query.
pub fn split_target(target: &str) -> (&str, &str) {
    target.split_once('?').unwrap_or((target, ""))
}

/// One parameter of a query string, percent decoded. The first one of a
/// name wins.
pub fn query_param(query: &str, name: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('=').or(Some((pair, ""))))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| percent_decode(v))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(hi), Some(lo)) => {
                    out.push((hi << 4) | lo);
                    i += 2;
                }
                _ => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// The widest picture a viewer asked for, None when it did not say or
/// said nonsense. Below 16 pixels nothing is readable, so that is the
/// floor.
pub fn max_width(query: &str) -> Option<u32> {
    query_param(query, "w")?
        .parse::<u32>()
        .ok()
        .filter(|w| *w > 0)
        .map(|w| w.max(16))
}

/// The size a `width` by `height` screen is sent at: as it is, or scaled
/// down to `max` wide with its shape kept.
pub fn scaled(width: u32, height: u32, max: Option<u32>) -> (u32, u32) {
    match max {
        Some(m) if m < width && width > 0 => {
            let h = (u64::from(height) * u64::from(m) / u64::from(width)).max(1);
            (m, h as u32)
        }
        _ => (width, height),
    }
}

/// A part of the screen, in virtual screen pixels from its top left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// The names a frame's region is asked by, in the order x, y, width,
/// height.
pub const REGION_PARAMS: [&str; 4] = ["rx", "ry", "rw", "rh"];

/// The part of the screen a frame was asked for, None when the whole of
/// it was, or why what was asked is no region. It is as the viewer said
/// it: [`Region::on`] holds it to a screen.
pub fn region(query: &str) -> Result<Option<Region>, String> {
    let [x, y, w, h] = REGION_PARAMS.map(|name| query_param(query, name));
    if x.is_none() && y.is_none() && w.is_none() && h.is_none() {
        return Ok(None);
    }
    const ALL: &str = "a region is rx, ry, rw and rh, in screen pixels from the screen's top left";
    let at = |v: Option<String>, name: &str| {
        let v = v.ok_or_else(|| format!("{ALL}: {name} is missing"))?;
        v.parse::<i32>()
            .map_err(|_| format!("{ALL}: {name} is not a whole number"))
    };
    let (x, y) = (at(x, "rx")?, at(y, "ry")?);
    let (width, height) = (at(w, "rw")?, at(h, "rh")?);
    if width < 1 || height < 1 {
        return Err(format!("{ALL}: rw and rh are at least 1"));
    }
    Ok(Some(Region {
        x,
        y,
        width: width as u32,
        height: height as u32,
    }))
}

impl Region {
    /// The part of this region that is on a `width` by `height` screen,
    /// None when none of it is.
    pub fn on(self, width: u32, height: u32) -> Option<Region> {
        let (x, y) = (i64::from(self.x), i64::from(self.y));
        let (left, top) = (x.max(0), y.max(0));
        let right = (x + i64::from(self.width)).min(i64::from(width));
        let bottom = (y + i64::from(self.height)).min(i64::from(height));
        (right > left && bottom > top).then(|| Region {
            x: left as i32,
            y: top as i32,
            width: (right - left) as u32,
            height: (bottom - top) as u32,
        })
    }

    /// The query that asks for this region, without a question mark.
    pub fn query(&self) -> String {
        let [x, y, w, h] = REGION_PARAMS;
        format!(
            "{x}={}&{y}={}&{w}={}&{h}={}",
            self.x, self.y, self.width, self.height
        )
    }
}

/// Why a region that is nowhere on the screen is refused.
pub fn off_screen(width: u32, height: u32) -> String {
    format!("the region is not on the screen, which is {width} x {height} pixels")
}

/// What a frame that could not be made answers. The error alone, a code
/// and a word or two, tells an agent nothing about what to do next, and
/// the usual reason is a session nobody is connected to.
pub fn capture_failed(error: &str) -> String {
    format!(
        "The screen could not be captured. A disconnected or locked session has no          picture to take. Windows said: {error}"
    )
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Button {
    Left,
    Right,
    Middle,
}

/// One event the viewer posts. Coordinates are virtual screen pixels from
/// its top left.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Input {
    Move {
        x: i32,
        y: i32,
    },
    Down {
        x: i32,
        y: i32,
        button: Button,
    },
    Up {
        x: i32,
        y: i32,
        button: Button,
    },
    /// The wheel turned up or down.
    Wheel {
        x: i32,
        y: i32,
        delta: i32,
    },
    /// The wheel tilted, or two fingers moved sideways.
    Hwheel {
        x: i32,
        y: i32,
        delta: i32,
    },
    Key {
        vk: u16,
        down: bool,
        /// Whether the key is of the extended part of the keyboard, for
        /// the keys a virtual key alone does not tell apart: the number
        /// pad's Enter from the main one. None leaves it to
        /// [`extended_key`].
        #[serde(default)]
        ext: Option<bool>,
    },
    Text {
        text: String,
    },
    /// Nothing, but that the viewer still holds what it holds.
    Alive,
    /// Let go of every key and button held down.
    Release,
}

impl Input {
    /// Whether the event does something to the desktop, where `alive` and
    /// `release` only look after what earlier ones did.
    pub fn drives(&self) -> bool {
        !matches!(self, Input::Alive | Input::Release)
    }
}

/// Whether a key is never pressed for a viewer: no key at all, the mouse
/// buttons under their key codes, sleep, and the browser, media and
/// launch keys, which act on the machine and not on what is watched.
pub fn refused_key(vk: u16) -> bool {
    matches!(vk, 0 | 1 | 2 | 4 | 5 | 6 | 0x5F | 0xA6..=0xB7) || vk > 0xFE
}

/// The events in the body `/input` was posted, one object or an array of
/// them, or why it is not that. Wheel turns come back clamped.
pub fn parse_inputs(body: &str) -> Result<Vec<Input>, String> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let values = match value {
        serde_json::Value::Array(all) if all.len() > MAX_EVENTS => {
            return Err(format!("more than {MAX_EVENTS} events"));
        }
        serde_json::Value::Array(all) => all,
        one => vec![one],
    };
    values.into_iter().map(checked).collect()
}

fn checked(value: serde_json::Value) -> Result<Input, String> {
    let input: Input = serde_json::from_value(value).map_err(|e| e.to_string())?;
    match input {
        Input::Key { vk, .. } if refused_key(vk) => Err(format!("no such key: {vk}")),
        Input::Text { text } if text.is_empty() => Err("no text".into()),
        Input::Wheel { x, y, delta } => Ok(Input::Wheel {
            x,
            y,
            delta: delta.clamp(-MAX_WHEEL, MAX_WHEEL),
        }),
        Input::Hwheel { x, y, delta } => Ok(Input::Hwheel {
            x,
            y,
            delta: delta.clamp(-MAX_WHEEL, MAX_WHEEL),
        }),
        other => Ok(other),
    }
}

/// A stretch of a `text` event as it is typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    /// Characters sent as themselves, in UTF-16 units.
    Units(Vec<u16>),
    /// A press of the Enter key. A line feed sent as a character is not
    /// one: a terminal or a form waiting for Enter does not take it.
    Enter,
    /// A press of the Tab key, which a character would not move focus with.
    Tab,
}

/// A text as the runs of characters and the key presses that type it.
/// Carriage returns are dropped, so a Windows line end is one Enter.
pub fn typed(text: &str) -> Vec<Typed> {
    let mut parts = Vec::new();
    let mut run = String::new();
    for c in text.chars() {
        let press = match c {
            '\n' => Typed::Enter,
            '\t' => Typed::Tab,
            '\r' => continue,
            c => {
                run.push(c);
                continue;
            }
        };
        if !run.is_empty() {
            parts.push(Typed::Units(run.encode_utf16().collect()));
            run.clear();
        }
        parts.push(press);
    }
    if !run.is_empty() {
        parts.push(Typed::Units(run.encode_utf16().collect()));
    }
    parts
}

/// A pixel `at` on a virtual screen `extent` pixels across, as the 0 to
/// 65535 SendInput takes for an absolute position on the virtual desk.
/// It aims at the pixel's centre: Windows turns the number back into a
/// pixel by scaling down and dropping the fraction, so aiming at an edge
/// can land on the pixel before. Off the screen it is held to the edge.
pub fn absolute(at: i32, extent: i32) -> i32 {
    if extent <= 0 {
        return 0;
    }
    let at = i64::from(at.clamp(0, extent - 1));
    ((2 * at + 1) * 65536 / (2 * i64::from(extent))).clamp(0, 65535) as i32
}

/// Whether a virtual key sits in the extended part of the keyboard, which
/// SendInput must be told: without it an arrow arrives as a number pad
/// key, and right Ctrl as left.
pub fn extended_key(vk: u16) -> bool {
    matches!(
        vk,
        0x21..=0x28 // page up, page down, end, home, arrows
            | 0x2C // print screen
            | 0x2D | 0x2E // insert, delete
            | 0x5B..=0x5D // windows keys, menu
            | 0x6F // number pad divide
            | 0x90 // num lock
            | 0xA3 | 0xA5 // right ctrl, right alt
    )
}

/// A display resolution in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

/// One mode the display driver offers, as EnumDisplaySettingsW lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub bits: u32,
    pub refresh: u32,
}

impl Mode {
    pub fn size(&self) -> Size {
        Size {
            width: self.width,
            height: self.height,
        }
    }
}

/// Below this nothing a program lays out fits, and some drivers offer
/// 640 by 480 still.
const SMALLEST: Size = Size {
    width: 800,
    height: 600,
};

/// The sizes worth offering out of every mode the driver lists: those at
/// the bits per pixel in use, none smaller than [`SMALLEST`], each size
/// once however many refresh rates it comes in, widest first.
pub fn display_sizes(modes: &[Mode], bits: u32) -> Vec<Size> {
    let mut sizes: Vec<Size> = modes
        .iter()
        .filter(|m| m.bits == bits && m.width >= SMALLEST.width && m.height >= SMALLEST.height)
        .map(Mode::size)
        .collect();
    sizes.sort_by_key(|s| std::cmp::Reverse((s.width, s.height)));
    sizes.dedup();
    sizes
}

/// What `GET /display` answers.
pub fn display_json(current: Size, modes: &[Size]) -> String {
    // A struct, not `json!`, so the keys keep the order the docs show.
    #[derive(Serialize)]
    struct Display<'a> {
        current: Size,
        modes: &'a [Size],
    }
    serde_json::to_string(&Display { current, modes }).unwrap_or_else(|_| "{}".into())
}

/// The size `POST /display` asks for, or why the body is not one.
pub fn parse_size(body: &str) -> Result<Size, String> {
    serde_json::from_str(body).map_err(|e| e.to_string())
}

/// Which of the driver's modes to switch to for `want`: one at the bits
/// per pixel in use, at the refresh rate in use when the size comes in
/// it, else at the highest it does. A mode the driver listed is one it
/// takes, where the current mode with a new size edited in may not be.
pub fn pick_mode(modes: &[Mode], want: Size, bits: u32, refresh: u32) -> Option<Mode> {
    modes
        .iter()
        .filter(|m| m.size() == want && m.bits == bits)
        .max_by_key(|m| (m.refresh == refresh, m.refresh))
        .copied()
}

/// What `POST /display` answers once the new resolution shows.
pub fn keep_json() -> String {
    format!(r#"{{"keep_within":{KEEP_WITHIN}}}"#)
}

/// Why Windows did not change the resolution, from the code
/// ChangeDisplaySettingsExW answered. None when it did.
pub fn disp_change_refusal(code: i32) -> Option<String> {
    match code {
        0 => None,
        // Not a refusal like the others: the mode is fine and nothing is
        // wrong, so it gets words of its own.
        1 => Some("Windows wants a restart before it shows this resolution".into()),
        code => Some(format!(
            "Windows refused the resolution: {}",
            disp_change_name(code)
        )),
    }
}

/// A resolution shown but not yet kept. Each change is numbered, so the
/// timer of a change that was replaced or kept meanwhile finds nothing
/// left to take back.
#[derive(Debug)]
pub struct Trial<T> {
    number: u64,
    pending: Option<T>,
}

impl<T> Default for Trial<T> {
    fn default() -> Self {
        Trial {
            number: 0,
            pending: None,
        }
    }
}

impl<T> Trial<T> {
    /// A change now shows and waits to be kept. The number is the one its
    /// timer hands to [`Trial::expire`]. One waiting before is replaced.
    pub fn begin(&mut self, what: T) -> u64 {
        self.number += 1;
        self.pending = Some(what);
        self.number
    }

    /// The change to keep, which then waits no longer.
    pub fn keep(&mut self) -> Option<T> {
        self.pending.take()
    }

    /// The change to take back when the timer of change `number` runs
    /// out, None when it was kept or replaced since.
    pub fn expire(&mut self, number: u64) -> Option<T> {
        if self.number == number {
            self.pending.take()
        } else {
            None
        }
    }
}

/// The name of a code ChangeDisplaySettingsExW answers, for the reason a
/// refused change gives.
pub fn disp_change_name(code: i32) -> &'static str {
    match code {
        0 => "DISP_CHANGE_SUCCESSFUL",
        1 => "DISP_CHANGE_RESTART",
        -1 => "DISP_CHANGE_FAILED",
        -2 => "DISP_CHANGE_BADMODE",
        -3 => "DISP_CHANGE_NOTUPDATED",
        -4 => "DISP_CHANGE_BADFLAGS",
        -5 => "DISP_CHANGE_BADPARAM",
        -6 => "DISP_CHANGE_BADDUALVIEW",
        _ => "an unknown DISP_CHANGE code",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_is_put_aside_only_once_it_is_past_the_limit() {
        let dir = std::env::temp_dir().join(format!("horadric-roll-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("a.log");
        let aside = dir.join("a.log.1");
        std::fs::write(&log, vec![b'x'; LOG_MAX as usize]).unwrap();
        roll(&log);
        assert!(log.exists() && !aside.exists(), "at the limit it stays");
        std::fs::write(&aside, "older").unwrap();
        std::fs::write(&log, vec![b'x'; LOG_MAX as usize + 1]).unwrap();
        roll(&log);
        let rolled = std::fs::metadata(&aside).map(|m| m.len());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(rolled.ok(), Some(LOG_MAX + 1), "past it, it replaces the old one");
        assert!(!log.exists());
    }

    #[test]
    fn only_one_turns_it_on() {
        assert!(enabled(Some("1")));
        assert!(!enabled(Some("0")));
        assert!(!enabled(Some("yes")));
        assert!(!enabled(Some("")));
        assert!(!enabled(None));
    }

    #[test]
    fn a_dev_instance_has_its_own_port() {
        assert_eq!(port(false), 43119);
        assert_eq!(port(true), 43120);
    }

    #[test]
    fn a_token_is_32_hex_characters() {
        assert_eq!(token_hex(&[0xab; 16]), "ab".repeat(16));
        assert_eq!(
            token_hex(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 255]),
            "000102030405060708090a0b0c0d0eff"
        );
    }

    #[test]
    fn the_token_comes_from_a_bearer_header_only() {
        assert_eq!(bearer("Bearer abc"), Some("abc"));
        assert_eq!(bearer("bearer  abc "), Some("abc"));
        assert_eq!(bearer("Basic abc"), None);
        assert_eq!(bearer("abc"), None);
        assert_eq!(bearer(""), None);
        let t = "0123456789abcdef0123456789abcdef";
        assert!(token_ok(bearer(&format!("Bearer {t}")), t));
        assert!(!token_ok(bearer("Bearer"), t));
    }

    #[test]
    fn hello_proves_the_token_without_saying_it() {
        let token = "0123456789abcdef0123456789abcdef";
        let nonce = "fedcba9876543210fedcba9876543210";
        // HMAC-SHA256 of the nonce's bytes under the token's, worked out
        // with another implementation.
        assert_eq!(
            hello_proof(token, nonce),
            "faf2b0c0c70f848e8c8fd0f5139bdfad89a1608a63359fd0e7677baa6e73eaa3"
        );
        assert_ne!(hello_proof(token, nonce), hello_proof(nonce, token));
    }

    #[test]
    fn a_nonce_is_32_hex_characters() {
        let n = "fedcba9876543210fedcba9876543210";
        assert_eq!(hello_nonce(&format!("n={n}")).as_deref(), Some(n));
        assert_eq!(hello_nonce(&format!("x=1&n={n}")).as_deref(), Some(n));
        assert_eq!(hello_nonce("n=abc"), None);
        assert_eq!(hello_nonce(&format!("n={n}0")), None);
        assert_eq!(hello_nonce(&format!("n={}", "g".repeat(32))), None);
        assert_eq!(hello_nonce("n="), None);
        assert_eq!(hello_nonce(""), None);
    }

    #[test]
    fn only_a_loopback_host_passes() {
        for host in ["127.0.0.1", "127.0.0.1:43119", "localhost:5000", "LocalHost"] {
            assert!(host_ok(host), "{host}");
        }
        for host in [
            "",
            "example.com",
            "example.com:43119",
            "127.0.0.1.example.com",
            "127.0.0.2",
            "localhost:",
            "localhost:http",
            "localhost:123456",
            "[::1]:43119",
            "evil.test:127.0.0.1",
        ] {
            assert!(!host_ok(host), "{host}");
        }
    }

    #[test]
    fn an_origin_must_be_the_viewer_itself() {
        let host = Some("127.0.0.1:43119");
        assert!(request_allowed(host, None));
        assert!(request_allowed(host, Some("http://127.0.0.1:43119")));
        assert!(request_allowed(
            Some("localhost:9000"),
            Some("http://localhost:9000")
        ));
        assert!(!request_allowed(host, Some("http://localhost:43119")));
        assert!(!request_allowed(host, Some("http://127.0.0.1:8080")));
        assert!(!request_allowed(host, Some("https://127.0.0.1:43119")));
        assert!(!request_allowed(host, Some("http://example.com")));
        assert!(!request_allowed(host, Some("null")));
        assert!(!request_allowed(None, None));
        assert!(!request_allowed(
            Some("example.com"),
            Some("http://example.com")
        ));
    }

    #[test]
    fn a_minute_of_silence_makes_the_next_one_new() {
        assert!(after_quiet(None, 0));
        assert!(!after_quiet(Some(1_000), 1_500));
        assert!(!after_quiet(Some(1_000), 60_999));
        assert!(after_quiet(Some(1_000), 61_000));
        assert!(!after_quiet(Some(5_000), 1_000));
    }

    #[test]
    fn only_the_right_token_passes() {
        let t = "0123456789abcdef0123456789abcdef";
        assert!(token_ok(Some(t), t));
        assert!(!token_ok(Some("0123456789abcdef0123456789abcdee"), t));
        assert!(!token_ok(Some("0123"), t));
        assert!(!token_ok(Some(""), t));
        assert!(!token_ok(None, t));
        assert!(!token_ok(Some(""), ""));
    }

    #[test]
    fn a_target_splits_at_the_question_mark() {
        assert_eq!(
            split_target("/frame.jpg?t=a&w=3"),
            ("/frame.jpg", "t=a&w=3")
        );
        assert_eq!(split_target("/"), ("/", ""));
    }

    #[test]
    fn parameters_are_found_and_decoded() {
        let q = "t=abc&w=800&x=a%20b+c&flag";
        assert_eq!(query_param(q, "t").as_deref(), Some("abc"));
        assert_eq!(query_param(q, "w").as_deref(), Some("800"));
        assert_eq!(query_param(q, "x").as_deref(), Some("a b c"));
        assert_eq!(query_param(q, "flag").as_deref(), Some(""));
        assert_eq!(query_param(q, "nope"), None);
        assert_eq!(query_param("t=a&t=b", "t").as_deref(), Some("a"));
        assert_eq!(query_param("x=%zz%4", "x").as_deref(), Some("%zz%4"));
        assert_eq!(query_param("x=%", "x").as_deref(), Some("%"));
        assert_eq!(query_param("x=%c3%a6", "x").as_deref(), Some("æ"));
    }

    #[test]
    fn a_width_is_a_positive_number() {
        assert_eq!(max_width("w=1280"), Some(1280));
        assert_eq!(max_width("w=3"), Some(16));
        assert_eq!(max_width("w=0"), None);
        assert_eq!(max_width("w=-5"), None);
        assert_eq!(max_width("w=wide"), None);
        assert_eq!(max_width("t=x"), None);
    }

    #[test]
    fn a_picture_scales_down_only() {
        assert_eq!(scaled(3840, 1080, Some(1920)), (1920, 540));
        assert_eq!(scaled(1920, 1080, Some(4000)), (1920, 1080));
        assert_eq!(scaled(1920, 1080, None), (1920, 1080));
        assert_eq!(scaled(1920, 1, Some(16)), (16, 1));
        assert_eq!(scaled(0, 0, Some(16)), (0, 0));
    }

    fn reg(x: i32, y: i32, width: u32, height: u32) -> Region {
        Region {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn a_region_is_four_whole_numbers_or_none_at_all() {
        assert_eq!(region(""), Ok(None));
        assert_eq!(region("w=800"), Ok(None));
        assert_eq!(
            region("rx=100&ry=50&rw=400&rh=300"),
            Ok(Some(reg(100, 50, 400, 300)))
        );
        assert_eq!(
            region("w=800&rh=1&rw=2&ry=-3&rx=-4"),
            Ok(Some(reg(-4, -3, 2, 1)))
        );
        for bad in [
            "rx=1",
            "rx=1&ry=2&rw=3",
            "rx=1&ry=2&rw=3&rh=",
            "rx=a&ry=2&rw=3&rh=4",
            "rx=1.5&ry=2&rw=3&rh=4",
            "rx=1&ry=2&rw=0&rh=4",
            "rx=1&ry=2&rw=3&rh=-4",
            "rx=1&ry=2&rw=99999999999&rh=4",
        ] {
            assert!(region(bad).is_err(), "{bad}");
        }
        assert!(region("rx=1&ry=2&rh=4").unwrap_err().contains("rw is missing"));
    }

    #[test]
    fn a_region_is_held_to_the_screen() {
        assert_eq!(reg(100, 50, 400, 300).on(1920, 1080), Some(reg(100, 50, 400, 300)));
        assert_eq!(reg(0, 0, 1920, 1080).on(1920, 1080), Some(reg(0, 0, 1920, 1080)));
        assert_eq!(reg(1800, 1000, 400, 300).on(1920, 1080), Some(reg(1800, 1000, 120, 80)));
        assert_eq!(reg(-50, -20, 100, 100).on(1920, 1080), Some(reg(0, 0, 50, 80)));
        assert_eq!(
            reg(i32::MIN, i32::MIN, u32::MAX, u32::MAX).on(1920, 1080),
            Some(reg(0, 0, 1920, 1080))
        );
        assert_eq!(reg(1919, 1079, 5, 5).on(1920, 1080), Some(reg(1919, 1079, 1, 1)));
        assert_eq!(reg(1920, 0, 10, 10).on(1920, 1080), None);
        assert_eq!(reg(0, 1080, 10, 10).on(1920, 1080), None);
        assert_eq!(reg(-10, 0, 10, 10).on(1920, 1080), None);
        assert_eq!(reg(i32::MAX, i32::MAX, u32::MAX, u32::MAX).on(1920, 1080), None);
        assert_eq!(reg(0, 0, 10, 10).on(0, 0), None);
    }

    #[test]
    fn a_region_asks_for_itself() {
        let r = reg(-4, 50, 400, 300);
        assert_eq!(r.query(), "rx=-4&ry=50&rw=400&rh=300");
        assert_eq!(region(&format!("w=640&{}", r.query())), Ok(Some(r)));
    }

    #[test]
    fn refusals_of_a_frame_say_what_to_make_of_them() {
        assert!(off_screen(1920, 1080).contains("1920 x 1080"));
        let said = capture_failed("The handle is invalid. (0x80070006)");
        assert!(said.starts_with("The screen could not be captured."));
        assert!(said.contains("disconnected or locked"));
        assert!(said.ends_with("Windows said: The handle is invalid. (0x80070006)"));
    }

    /// The one event of a body holding one.
    fn parse_input(body: &str) -> Result<Input, String> {
        let mut all = parse_inputs(body)?;
        assert_eq!(all.len(), 1);
        Ok(all.remove(0))
    }

    #[test]
    fn a_body_is_one_event_or_an_array_of_them() {
        let one = r#"{"kind":"move","x":1,"y":2}"#;
        assert_eq!(parse_inputs(one), Ok(vec![Input::Move { x: 1, y: 2 }]));
        assert_eq!(
            parse_inputs(&format!(r#"[{one},{{"kind":"alive"}}]"#)),
            Ok(vec![Input::Move { x: 1, y: 2 }, Input::Alive])
        );
        assert_eq!(parse_inputs("[]"), Ok(Vec::new()));
        let many = |n: usize| format!("[{}]", vec![one; n].join(","));
        assert_eq!(parse_inputs(&many(MAX_EVENTS)).map(|v| v.len()), Ok(256));
        assert!(parse_inputs(&many(MAX_EVENTS + 1)).is_err());
        // One bad event refuses the whole sequence: half a shortcut is
        // worse than none.
        assert!(parse_inputs(&format!(r#"[{one},{{"kind":"fly"}}]"#)).is_err());
        assert!(parse_inputs(&format!("[[{one}]]")).is_err());
        assert!(parse_inputs("7").is_err());
    }

    #[test]
    fn keys_that_act_on_the_machine_are_refused() {
        for vk in [0, 1, 2, 4, 5, 6, 0x5F, 0xA6, 0xAD, 0xB3, 0xB7, 0xFF, 300] {
            assert!(refused_key(vk), "{vk:#x}");
            assert!(
                parse_inputs(&format!(r#"{{"kind":"key","vk":{vk},"down":true}}"#)).is_err(),
                "{vk:#x}"
            );
        }
        for vk in [3, 8, 0x0D, 0x41, 0x5B, 0x5E, 0x60, 0xA5, 0xB8, 0xFE] {
            assert!(!refused_key(vk), "{vk:#x}");
        }
    }

    #[test]
    fn wheel_turns_are_clamped() {
        let turn = |kind: &str, delta: i64| {
            parse_input(&format!(
                r#"{{"kind":"{kind}","x":0,"y":0,"delta":{delta}}}"#
            ))
        };
        assert_eq!(
            turn("wheel", 999_999),
            Ok(Input::Wheel {
                x: 0,
                y: 0,
                delta: 12_000
            })
        );
        assert_eq!(
            turn("hwheel", -999_999),
            Ok(Input::Hwheel {
                x: 0,
                y: 0,
                delta: -12_000
            })
        );
        assert_eq!(
            turn("wheel", -120),
            Ok(Input::Wheel {
                x: 0,
                y: 0,
                delta: -120
            })
        );
        assert!(turn("wheel", 99_999_999_999).is_err());
    }

    #[test]
    fn text_is_typed_as_runs_and_key_presses() {
        let units = |s: &str| Typed::Units(s.encode_utf16().collect());
        assert_eq!(typed("abc"), [units("abc")]);
        assert_eq!(
            typed("a\r\nb\tc\n"),
            [
                units("a"),
                Typed::Enter,
                units("b"),
                Typed::Tab,
                units("c"),
                Typed::Enter
            ]
        );
        assert_eq!(typed("\n\n"), [Typed::Enter, Typed::Enter]);
        assert_eq!(typed("\r"), []);
        assert_eq!(typed(""), []);
        // Outside the basic plane a character is two units, kept together.
        assert_eq!(typed("\u{e6}\u{1F600}"), [Typed::Units(vec![0xE6, 0xD83D, 0xDE00])]);
    }

    #[test]
    fn only_some_events_drive() {
        assert!(Input::Move { x: 0, y: 0 }.drives());
        assert!(Input::Text { text: "a".into() }.drives());
        assert!(!Input::Alive.drives());
        assert!(!Input::Release.drives());
    }

    #[test]
    fn every_kind_of_event_parses() {
        assert_eq!(
            parse_input(r#"{"kind":"move","x":10,"y":20}"#),
            Ok(Input::Move { x: 10, y: 20 })
        );
        assert_eq!(
            parse_input(r#"{"kind":"down","x":1,"y":2,"button":"right"}"#),
            Ok(Input::Down {
                x: 1,
                y: 2,
                button: Button::Right
            })
        );
        assert_eq!(
            parse_input(r#"{"kind":"up","x":1,"y":2,"button":"middle"}"#),
            Ok(Input::Up {
                x: 1,
                y: 2,
                button: Button::Middle
            })
        );
        assert_eq!(
            parse_input(r#"{"kind":"wheel","x":1,"y":2,"delta":-120}"#),
            Ok(Input::Wheel {
                x: 1,
                y: 2,
                delta: -120
            })
        );
        assert_eq!(
            parse_input(r#"{"kind":"hwheel","x":1,"y":2,"delta":240}"#),
            Ok(Input::Hwheel {
                x: 1,
                y: 2,
                delta: 240
            })
        );
        assert_eq!(
            parse_input(r#"{"kind":"key","vk":13,"down":true}"#),
            Ok(Input::Key {
                vk: 13,
                down: true,
                ext: None
            })
        );
        assert_eq!(
            parse_input(r#"{"kind":"key","vk":13,"down":false,"ext":true}"#),
            Ok(Input::Key {
                vk: 13,
                down: false,
                ext: Some(true)
            })
        );
        assert_eq!(parse_input(r#"{"kind":"alive"}"#), Ok(Input::Alive));
        assert_eq!(parse_input(r#"{"kind":"release"}"#), Ok(Input::Release));
        assert_eq!(
            parse_input(r#"{"kind":"text","text":"æøå"}"#),
            Ok(Input::Text {
                text: "æøå".into()
            })
        );
    }

    #[test]
    fn a_bad_event_says_why() {
        assert!(parse_inputs("").is_err());
        assert!(parse_input(r#"{"kind":"fly"}"#).is_err());
        assert!(parse_input(r#"{"kind":"move","x":1}"#).is_err());
        assert!(parse_input(r#"{"kind":"down","x":1,"y":2,"button":"thumb"}"#).is_err());
        assert!(parse_input(r#"{"kind":"key","vk":0,"down":true}"#).is_err());
        assert!(parse_input(r#"{"kind":"key","vk":300,"down":true}"#).is_err());
        assert!(parse_input(r#"{"kind":"key","vk":65,"down":true,"ext":1}"#).is_err());
        assert!(parse_input(r#"{"kind":"text","text":""}"#).is_err());
    }

    #[test]
    fn positions_aim_at_the_middle_of_their_pixel() {
        assert_eq!(absolute(0, 1920), 17);
        assert_eq!(absolute(1919, 1920), 65518);
        assert_eq!(absolute(5000, 1920), 65518);
        assert_eq!(absolute(-3, 1920), 17);
        assert_eq!(absolute(960, 1921), 32768);
        assert_eq!(absolute(5, 1), 32768);
        assert_eq!(absolute(5, 0), 0);
        // Scaled back as Windows does, every pixel lands on itself.
        for extent in [1, 3, 1920, 2561, 7680] {
            for at in 0..extent {
                let back = i64::from(absolute(at, extent)) * i64::from(extent) / 65536;
                assert_eq!(back, i64::from(at), "{at} of {extent}");
            }
        }
    }

    #[test]
    fn arrows_and_right_modifiers_are_extended() {
        for vk in [0x25, 0x26, 0x27, 0x28, 0x2E, 0x24, 0xA3, 0xA5, 0x5B] {
            assert!(extended_key(vk), "{vk:#x}");
        }
        for vk in [0x0D, 0x41, 0xA2, 0xA4, 0x10, 0x60] {
            assert!(!extended_key(vk), "{vk:#x}");
        }
    }

    fn mode(width: u32, height: u32, bits: u32, refresh: u32) -> Mode {
        Mode {
            width,
            height,
            bits,
            refresh,
        }
    }

    fn size(width: u32, height: u32) -> Size {
        Size { width, height }
    }

    #[test]
    fn sizes_are_merged_filtered_and_widest_first() {
        let modes = [
            mode(640, 480, 32, 60),
            mode(800, 600, 32, 60),
            mode(1920, 1080, 32, 60),
            mode(1920, 1080, 32, 144),
            mode(2560, 1440, 32, 60),
            mode(1920, 1200, 32, 60),
            mode(3840, 2160, 16, 60),
            mode(1024, 576, 32, 60),
            mode(1680, 1050, 32, 60),
        ];
        assert_eq!(
            display_sizes(&modes, 32),
            [
                size(2560, 1440),
                size(1920, 1200),
                size(1920, 1080),
                size(1680, 1050),
                size(800, 600),
            ]
        );
        assert_eq!(display_sizes(&modes, 16), [size(3840, 2160)]);
        assert!(display_sizes(&[], 32).is_empty());
    }

    #[test]
    fn the_display_answer_has_current_and_modes() {
        let json = display_json(size(2560, 1440), &[size(2560, 1440), size(1920, 1080)]);
        assert_eq!(
            json,
            r#"{"current":{"width":2560,"height":1440},"modes":[{"width":2560,"height":1440},{"width":1920,"height":1080}]}"#
        );
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["current"]["width"], 2560);
        assert_eq!(v["modes"][1]["height"], 1080);
        assert_eq!(
            display_json(size(1, 2), &[]),
            r#"{"current":{"width":1,"height":2},"modes":[]}"#
        );
    }

    #[test]
    fn a_size_parses_or_says_why() {
        assert_eq!(
            parse_size(r#"{"width":1920,"height":1080}"#),
            Ok(size(1920, 1080))
        );
        assert!(parse_size(r#"{"width":1920}"#).is_err());
        assert!(parse_size(r#"{"width":-1,"height":2}"#).is_err());
        assert!(parse_size("").is_err());
    }

    #[test]
    fn the_mode_keeps_bits_and_refresh_where_it_can() {
        let modes = [
            mode(1920, 1080, 32, 60),
            mode(1920, 1080, 32, 144),
            mode(1920, 1080, 32, 120),
            mode(1920, 1080, 16, 60),
            mode(1280, 720, 32, 30),
            mode(1280, 720, 32, 50),
        ];
        let want = size(1920, 1080);
        assert_eq!(pick_mode(&modes, want, 32, 60), Some(modes[0]));
        assert_eq!(pick_mode(&modes, want, 32, 120), Some(modes[2]));
        assert_eq!(pick_mode(&modes, want, 32, 75), Some(modes[1]));
        assert_eq!(pick_mode(&modes, want, 16, 144), Some(modes[3]));
        assert_eq!(pick_mode(&modes, size(1280, 720), 32, 60), Some(modes[5]));
        assert_eq!(pick_mode(&modes, size(1024, 768), 32, 60), None);
        assert_eq!(pick_mode(&modes, want, 24, 60), None);
    }

    #[test]
    fn a_new_resolution_asks_to_be_kept() {
        assert_eq!(keep_json(), r#"{"keep_within":15}"#);
    }

    #[test]
    fn a_refusal_says_which_and_a_restart_says_so() {
        assert_eq!(disp_change_refusal(0), None);
        assert_eq!(
            disp_change_refusal(-2).as_deref(),
            Some("Windows refused the resolution: DISP_CHANGE_BADMODE")
        );
        let restart = disp_change_refusal(1).unwrap();
        assert!(restart.contains("restart"));
        assert!(!restart.contains("refused"));
    }

    #[test]
    fn a_trial_is_taken_back_unless_kept_or_replaced() {
        let mut trial = Trial::default();
        assert_eq!(trial.keep(), None::<&str>);
        assert_eq!(trial.expire(0), None);

        let first = trial.begin("first");
        assert_eq!(trial.expire(first), Some("first"));
        assert_eq!(trial.expire(first), None);
        assert_eq!(trial.keep(), None);

        let kept = trial.begin("kept");
        assert_eq!(trial.keep(), Some("kept"));
        assert_eq!(trial.expire(kept), None);

        // The first timer must not take back the second change early.
        let old = trial.begin("old");
        let new = trial.begin("new");
        assert_eq!(trial.expire(old), None);
        assert_eq!(trial.expire(new), Some("new"));
    }

    #[test]
    fn display_change_codes_have_names() {
        assert_eq!(disp_change_name(0), "DISP_CHANGE_SUCCESSFUL");
        assert_eq!(disp_change_name(1), "DISP_CHANGE_RESTART");
        assert_eq!(disp_change_name(-2), "DISP_CHANGE_BADMODE");
        assert_eq!(disp_change_name(-6), "DISP_CHANGE_BADDUALVIEW");
        assert_eq!(disp_change_name(7), "an unknown DISP_CHANGE code");
    }
}
