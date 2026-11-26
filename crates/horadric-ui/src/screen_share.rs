//! Serves this desktop to a viewer page on 127.0.0.1, for a machine that
//! watches it through an SSH tunnel: frames as JPEG, input posted back,
//! and the display's resolution. Off unless `HORADRIC_SCREEN_SHARE=1`.
//! What it parses and checks is in `horadric_core::screen_share`.
//!
//! It runs in the app because the app is in the interactive desktop
//! session: a service or an SSH login sees no screen and can send it no
//! keys.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use horadric_core::screen_share::{self as share, Button, Input, Mode, Size, Trial, Typed};
use windows::core::{Error, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, E_FAIL, HANDLE, HWND, POINT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, ChangeDisplaySettingsExW, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC,
    DeleteObject, EnumDisplayDevicesW, EnumDisplaySettingsW, GetDC, ReleaseDC, SelectObject,
    SetBrushOrgEx, SetStretchBltMode, StretchBlt, CAPTUREBLT, CDS_TEST, CDS_TYPE,
    CDS_UPDATEREGISTRY, DEVMODEW, DISPLAY_DEVICEW, DISPLAY_DEVICE_PRIMARY_DEVICE, DM_BITSPERPEL,
    DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_CURRENT_SETTINGS,
    ENUM_DISPLAY_SETTINGS_MODE, HALFTONE, HBITMAP, HDC, HGDIOBJ, HPALETTE, SRCCOPY,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_WICPixelFormat24bppBGR,
    IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapEncoderNoCache, WICBitmapIgnoreAlpha,
    WICBitmapPaletteTypeCustom,
};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Com::StructuredStorage::{IPropertyBag2, PROPBAG2};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    STREAM_SEEK_END, STREAM_SEEK_SET,
};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, DESKTOP_CONTROL_FLAGS,
    DESKTOP_READOBJECTS, HDESK, UOI_NAME,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
    KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT,
    MOUSE_EVENT_FLAGS, VIRTUAL_KEY, VK_RETURN, VK_TAB,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::{
    DrawIconEx, GetCursorInfo, GetForegroundWindow, GetIconInfo, GetSystemMetrics,
    GetWindowThreadProcessId, WindowFromPoint, CURSORINFO, CURSOR_SHOWING, DI_NORMAL, HICON,
    ICONINFO, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

const VIEWER: &str = include_str!("screen_share.html");

/// What the viewer page is sent with besides the usual: it may not be
/// framed by another page, which could then lay its own over the screen
/// and have clicks land where it likes, and it leaks no address onward.
const PAGE_HEADERS: &str =
    "X-Frame-Options: DENY\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n";

const LOCKED: &str = "the desktop is locked or shows a UAC prompt";
const ELEVATED: &str = "the window there runs as administrator and this Horadric does not";

/// How long a request may take to send its head, all of it. A limit on
/// each read alone lets a byte every few seconds hold a connection for
/// good.
const HEAD_TIME: Duration = Duration::from_secs(10);
const BODY_TIME: Duration = Duration::from_secs(10);

/// What the server threads have to tell whoever sits at this machine,
/// kept until the UI thread asks: toasts are shown from there alone.
static TOLD: AtomicU8 = AtomicU8::new(0);
const WATCHED: u8 = 1;
const DRIVEN: u8 = 2;

/// The next thing to say in a toast, as its title and text, when there
/// is one. For the UI thread to ask every tick: with sharing off, and
/// whenever nothing happened, it is one load of a byte.
pub fn told() -> Option<(&'static str, &'static str)> {
    if TOLD.load(Ordering::Relaxed) == 0 {
        return None;
    }
    [
        (WATCHED, "This desktop is being watched from another machine"),
        (DRIVEN, "This desktop is being driven from another machine"),
    ]
    .into_iter()
    .find(|(bit, _)| TOLD.fetch_and(!bit, Ordering::Relaxed) & bit != 0)
    .map(|(_, text)| ("Screen share", text))
}

/// What the server and its connections share.
struct Shared {
    token: String,
    started: Instant,
    connections: AtomicUsize,
    /// When the last request with the right token came, and the last
    /// input that drove, as [`Shared::now`] plus one. 0 is never.
    seen: AtomicU64,
    driven: AtomicU64,
    /// When `/input` was last heard from, the same way, for the watchdog.
    heard: AtomicU64,
    /// What is held down, and the one lock a whole sequence of input is
    /// played under, so no other request's events land in its middle.
    held: Mutex<Held>,
    frame: Mutex<Option<Cached>>,
    trial: Mutex<Trial<(Display, DEVMODEW)>>,
}

impl Shared {
    /// Milliseconds since the server started.
    fn now(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// Notes that something happened now, and says whether it is the
    /// first of its kind after a quiet minute.
    fn first_in_a_while(&self, at: &AtomicU64) -> bool {
        let now = self.now();
        let last = at.swap(now + 1, Ordering::Relaxed);
        share::after_quiet(last.checked_sub(1), now)
    }

    fn hear(&self) {
        self.heard.store(self.now() + 1, Ordering::Relaxed);
    }
}

/// A lock taken even when a thread died holding it: what is behind these
/// locks stays usable, and a server that panics on every request after
/// one bad one is worse.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Starts serving when [`share::ENV`] says so, on threads of its own.
pub fn start() {
    if !share::enabled(std::env::var(share::ENV).ok().as_deref()) {
        return;
    }
    let Some(token) = token() else {
        eprintln!("horadric: screen share: no token, not serving");
        return;
    };
    let port = share::port(horadric_hooks::dev());
    let spawned = thread::Builder::new()
        .name("screen-share".into())
        .spawn(move || {
            if let Err(e) = serve(port, token) {
                eprintln!("horadric: screen share stopped: {e}");
                log(&format!("stopped: {e}"));
            }
        });
    if let Err(e) = spawned {
        eprintln!("horadric: screen share: no thread to serve on: {e}");
        log(&format!("could not start: {e}"));
    }
}

/// A new token for this start, written over the one before. A token that
/// leaked is then good until the next start at most, and a viewer left
/// open in a tab somewhere does not come back to life.
fn token() -> Option<String> {
    let path = horadric_hooks::state_dir()?.join(share::TOKEN_FILE);
    let mut bytes = [0u8; 16];
    crate::portal_crypto::random(&mut bytes);
    let token = share::token_hex(&bytes);
    write_token(&path, &token).ok()?;
    Some(token)
}

fn write_token(path: &Path, token: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, token)
}

/// One line in the audit log, so whoever owns this machine can read
/// afterwards when it was watched and driven. Never the token.
fn log(line: &str) {
    static ONE: Mutex<()> = Mutex::new(());
    let _one = lock(&ONE);
    let Some(dir) = horadric_hooks::state_dir() else {
        return;
    };
    let path = dir.join(share::LOG_FILE);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > share::LOG_MAX) {
        let _ = std::fs::rename(&path, dir.join(format!("{}.1", share::LOG_FILE)));
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path);
    if let Ok(mut file) = file {
        let _ = writeln!(file, "{} {line}", crate::crashlog::stamp());
    }
}

/// A connection's place among the few served at once, given back when
/// its thread ends or could not be started.
struct Slot(Arc<Shared>);

impl Slot {
    fn take(shared: &Arc<Shared>) -> Option<Slot> {
        if shared.connections.fetch_add(1, Ordering::Relaxed) >= share::MAX_CONNECTIONS {
            shared.connections.fetch_sub(1, Ordering::Relaxed);
            return None;
        }
        Some(Slot(Arc::clone(shared)))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::Relaxed);
    }
}

fn serve(port: u16, token: String) -> io::Result<()> {
    let listener = match bind(port) {
        Ok(l) => l,
        Err(e) => {
            log(&format!("could not bind port {port}: {e}"));
            return Err(e);
        }
    };
    let shared = Arc::new(Shared {
        token,
        started: Instant::now(),
        connections: AtomicUsize::new(0),
        seen: AtomicU64::new(0),
        driven: AtomicU64::new(0),
        heard: AtomicU64::new(0),
        held: Mutex::new(Held::default()),
        frame: Mutex::new(None),
        trial: Mutex::new(Trial::default()),
    });
    // Without the watchdog a viewer that dies leaves its keys down, so
    // there is no serving without it.
    let watched = Arc::clone(&shared);
    thread::Builder::new()
        .name("screen-share-watch".into())
        .spawn(move || watch(&watched))?;
    log(&format!("started on port {port}"));

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            // Out of handles or memory fails every accept alike: a pause
            // keeps that from taking a core while it lasts.
            Err(_) => {
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let Some(slot) = Slot::take(&shared) else {
            refuse(&stream);
            continue;
        };
        let spawned = thread::Builder::new()
            .name("screen-share-conn".into())
            .spawn(move || {
                let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
                let _ = handle(&stream, &slot.0);
            });
        // The closure was dropped with the stream and the slot in it, so
        // the connection is closed and its place free again.
        if spawned.is_err() {
            thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(())
}

/// Turns away a connection over the limit, on the accepting thread, so
/// it may take no time to speak of.
fn refuse(stream: &TcpStream) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let _ = respond(stream, "503 Service Unavailable", "text/plain", b"busy");
    // One short read of what was sent: closing with it unread makes
    // Windows reset the connection, and the answer is lost with it.
    let _ = stream.shutdown(Shutdown::Write);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
    let mut sink = [0u8; 4096];
    let mut stream = stream;
    let _ = stream.read(&mut sink);
}

/// COM on this thread for as long as it lives, for WIC. Left again when
/// dropped, and only when joining succeeded, as COM counts them in pairs.
struct Com(bool);

impl Com {
    fn join() -> Com {
        // SAFETY: no pointers; the pair is kept by `Drop`.
        Com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: matches the successful CoInitializeEx above, on the
            // same thread, since a `Com` is never sent anywhere.
            unsafe { CoUninitialize() };
        }
    }
}

/// Binds, retrying for a few seconds: after a reload the Horadric before
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

const MAX_LINE: u64 = 8 * 1024;
const MAX_HEADERS: usize = 64;

/// A connection read against a deadline for the whole of what is being
/// read, however many reads that takes.
struct Timed<'a> {
    stream: &'a TcpStream,
    until: Instant,
}

impl Timed<'_> {
    fn allow(&mut self, more: Duration) {
        self.until = Instant::now() + more;
    }
}

impl Read for Timed<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(left))?;
        let mut stream = self.stream;
        stream.read(buf)
    }
}

fn handle(stream: &TcpStream, shared: &Arc<Shared>) -> io::Result<()> {
    // A borrow, never a `try_clone`: the clone is an inheritable handle a
    // session started meanwhile would keep open.
    let mut reader = BufReader::new(Timed {
        stream,
        until: Instant::now() + HEAD_TIME,
    });
    let Some(request_line) = read_line(&mut reader)? else {
        return respond(stream, "431 Request Header Fields Too Large", "", b"");
    };
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();

    let mut content_length = 0usize;
    let (mut host, mut origin, mut authorization) = (None, None, None);
    for count in 0.. {
        if count > MAX_HEADERS {
            return respond(stream, "431 Request Header Fields Too Large", "", b"");
        }
        let Some(line) = read_line(&mut reader)? else {
            return respond(stream, "431 Request Header Fields Too Large", "", b"");
        };
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let (name, value) = (name.trim(), value.trim());
        // The first of a name counts, as for the query: a second one
        // cannot take back what the first was checked for.
        let slot = if name.eq_ignore_ascii_case("host") {
            &mut host
        } else if name.eq_ignore_ascii_case("origin") {
            &mut origin
        } else if name.eq_ignore_ascii_case("authorization") {
            &mut authorization
        } else {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse().unwrap_or(0);
            }
            continue;
        };
        slot.get_or_insert_with(|| value.to_string());
    }
    reader.get_mut().allow(BODY_TIME);

    let (path, query) = share::split_target(&target);
    // A page on any site can make this machine's browser ask 127.0.0.1
    // for things. Such a request names its own site in Host (when the
    // site's name was pointed here) or in Origin, and stops at this.
    if !share::request_allowed(host.as_deref(), origin.as_deref()) {
        return respond(stream, "403 Forbidden", "text/plain", b"forbidden");
    }
    // The page and the proof hold nothing worth guarding and are what a
    // viewer needs before it has, or dares send, the token.
    match (method.as_str(), path) {
        ("GET", "/") => {
            return respond_with(
                stream,
                "200 OK",
                "text/html; charset=utf-8",
                PAGE_HEADERS,
                VIEWER.as_bytes(),
            );
        }
        ("GET", "/hello") => {
            return match share::hello_nonce(query) {
                Some(nonce) => respond(
                    stream,
                    "200 OK",
                    "text/plain",
                    share::hello_proof(&shared.token, &nonce).as_bytes(),
                ),
                None => respond(stream, "400 Bad Request", "text/plain", b"bad nonce"),
            };
        }
        _ => {}
    }
    let given = authorization.as_deref().and_then(share::bearer);
    if !share::token_ok(given, &shared.token) {
        return respond(stream, "403 Forbidden", "text/plain", b"forbidden");
    }
    if shared.first_in_a_while(&shared.seen) {
        log("a viewer arrived");
        TOLD.fetch_or(WATCHED, Ordering::Relaxed);
    }
    match (method.as_str(), path) {
        ("GET", "/frame.jpg") => {
            // A capture of a desktop that is not shown is black or stale,
            // and the viewer cannot tell that from a screen gone dark.
            if let Err(why) = own_desktop() {
                return respond(stream, "423 Locked", "text/plain", why.as_bytes());
            }
            let _com = Com::join();
            match latest(&shared.frame, share::max_width(query)) {
                Ok(f) => respond_frame(stream, &f),
                Err(e) => respond(
                    stream,
                    "500 Internal Server Error",
                    "text/plain",
                    e.to_string().as_bytes(),
                ),
            }
        }
        ("POST", "/input") => {
            shared.hear();
            let Some(body) = read_body(stream, &mut reader, content_length)? else {
                return Ok(());
            };
            match body.and_then(|b| share::parse_inputs(&b)) {
                Ok(events) => {
                    if events.iter().any(Input::drives) && shared.first_in_a_while(&shared.driven)
                    {
                        log("input began");
                        TOLD.fetch_or(DRIVEN, Ordering::Relaxed);
                    }
                    match run(shared, &events) {
                        Ok(()) => respond(stream, "204 No Content", "", b""),
                        Err(why) => respond(stream, "409 Conflict", "text/plain", why.as_bytes()),
                    }
                }
                Err(why) => respond(stream, "400 Bad Request", "text/plain", why.as_bytes()),
            }
        }
        ("GET", "/display") => {
            let display = Display::primary();
            let current = display.current();
            let sizes = share::display_sizes(&display.modes(), current.bits);
            respond(
                stream,
                "200 OK",
                "application/json",
                share::display_json(current.size(), &sizes).as_bytes(),
            )
        }
        ("POST", "/display") => {
            let Some(body) = read_body(stream, &mut reader, content_length)? else {
                return Ok(());
            };
            match body.and_then(|b| share::parse_size(&b)) {
                Ok(size) => match resize(shared, size) {
                    Ok(()) => respond(
                        stream,
                        "200 OK",
                        "application/json",
                        share::keep_json().as_bytes(),
                    ),
                    Err(refused) => respond_refused(stream, &refused),
                },
                Err(why) => respond(stream, "400 Bad Request", "text/plain", why.as_bytes()),
            }
        }
        ("POST", "/display/keep") => {
            // It has no body, but one sent anyway is read, or closing on
            // it would lose the answer.
            if read_body(stream, &mut reader, content_length)?.is_none() {
                return Ok(());
            }
            match keep(shared) {
                Ok(()) => respond(stream, "204 No Content", "", b""),
                Err(refused) => respond_refused(stream, &refused),
            }
        }
        ("POST", "/display/revert") => {
            if read_body(stream, &mut reader, content_length)?.is_none() {
                return Ok(());
            }
            revert(shared);
            respond(stream, "204 No Content", "", b"")
        }
        _ => respond(stream, "404 Not Found", "text/plain", b"not found"),
    }
}

fn respond_refused(stream: &TcpStream, refused: &Refused) -> io::Result<()> {
    let (status, why) = match refused {
        Refused::NotOffered => ("400 Bad Request", "not a resolution this display offers"),
        Refused::Windows(why) => ("409 Conflict", why.as_str()),
        Refused::Broken(why) => ("500 Internal Server Error", why.as_str()),
    };
    respond(stream, status, "text/plain", why.as_bytes())
}

/// A posted body as text, or why it is not text. None when it was too big,
/// which is answered here already.
fn read_body(
    stream: &TcpStream,
    reader: &mut BufReader<Timed>,
    length: usize,
) -> io::Result<Option<Result<String, String>>> {
    if length > share::MAX_BODY {
        respond(stream, "413 Payload Too Large", "", b"")?;
        drain(stream, reader);
        return Ok(None);
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    Ok(Some(String::from_utf8(body).map_err(|e| e.to_string())))
}

/// Reads and drops what is left of a body not read, a bounded amount for
/// a short while. Closing with bytes unread makes Windows reset the
/// connection, and the client then sees the reset, not the answer sent.
fn drain(stream: &TcpStream, reader: &mut BufReader<Timed>) {
    const MOST: u64 = 1024 * 1024;
    let _ = stream.shutdown(Shutdown::Write);
    reader.get_mut().allow(Duration::from_secs(1));
    let _ = io::copy(&mut reader.take(MOST), &mut io::sink());
}

fn read_line(reader: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    let read = reader.take(MAX_LINE).read_until(b'\n', &mut line)?;
    Ok((read < MAX_LINE as usize || line.ends_with(b"\n"))
        .then(|| String::from_utf8_lossy(&line).into_owned()))
}

fn respond(stream: &TcpStream, status: &str, kind: &str, body: &[u8]) -> io::Result<()> {
    respond_with(stream, status, kind, "", body)
}

/// An answer with `more` header lines of its own, each ended by CR LF.
fn respond_with(
    mut stream: &TcpStream,
    status: &str,
    kind: &str,
    more: &str,
    body: &[u8],
) -> io::Result<()> {
    let kind = if kind.is_empty() {
        String::new()
    } else {
        format!("Content-Type: {kind}\r\n")
    };
    let head = format!(
        "HTTP/1.1 {status}\r\n{kind}Content-Length: {}\r\n{more}Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn respond_frame(stream: &TcpStream, f: &Frame) -> io::Result<()> {
    // The viewer reads these from its own origin, which needs no leave
    // to see a header.
    let more = format!(
        "X-Screen-Left: {}\r\nX-Screen-Top: {}\r\nX-Screen-Width: {}\r\nX-Screen-Height: {}\r\n",
        f.screen.left, f.screen.top, f.screen.width, f.screen.height,
    );
    respond_with(stream, "200 OK", "image/jpeg", &more, &f.jpeg)
}

/// The virtual screen: every monitor, in physical pixels, since the app is
/// per monitor DPI aware.
#[derive(Debug, Clone, Copy)]
struct Screen {
    left: i32,
    top: i32,
    width: i32,
    height: i32,
}

fn screen() -> Screen {
    // SAFETY: GetSystemMetrics takes a number and hands one back.
    unsafe {
        Screen {
            left: GetSystemMetrics(SM_XVIRTUALSCREEN),
            top: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

struct Frame {
    screen: Screen,
    jpeg: Vec<u8>,
}

/// The frame made last, kept for a viewer asking again at once.
struct Cached {
    at: Instant,
    max: Option<u32>,
    frame: Arc<Frame>,
}

/// A frame this young is handed out again: several viewers, or one that
/// asks faster than a frame is made, cost one capture between them.
const FRESH: Duration = Duration::from_millis(100);

/// The newest frame at most `max` wide. The lock is held while one is
/// made, so there is one capture at a time and whoever waited takes the
/// frame the capture before it made.
fn latest(cache: &Mutex<Option<Cached>>, max: Option<u32>) -> windows::core::Result<Arc<Frame>> {
    let mut cached = lock(cache);
    if let Some(c) = cached.as_ref() {
        if c.max == max && c.at.elapsed() < FRESH {
            return Ok(Arc::clone(&c.frame));
        }
    }
    let frame = Arc::new(frame(max)?);
    *cached = Some(Cached {
        at: Instant::now(),
        max,
        frame: Arc::clone(&frame),
    });
    Ok(frame)
}

/// GDI objects made for one frame, freed however the frame ends.
struct Gdi {
    screen: HDC,
    dcs: Vec<HDC>,
    bitmaps: Vec<HBITMAP>,
}

impl Drop for Gdi {
    fn drop(&mut self) {
        // SAFETY: every handle here was made by this `Gdi` and is freed
        // nowhere else but in `frame`, which takes the DCs it deletes
        // out of the list first.
        unsafe {
            for dc in &self.dcs {
                let _ = DeleteDC(*dc);
            }
            for b in &self.bitmaps {
                let _ = DeleteObject(HGDIOBJ(b.0));
            }
            ReleaseDC(None, self.screen);
        }
    }
}

impl Gdi {
    /// A memory DC holding a new `w` by `h` bitmap.
    ///
    /// SAFETY: `self.screen` must be a live screen DC, which it is from
    /// the check in `frame` until the `Gdi` drops.
    unsafe fn canvas(&mut self, w: i32, h: i32) -> windows::core::Result<(HDC, HBITMAP)> {
        let dc = CreateCompatibleDC(Some(self.screen));
        if dc.is_invalid() {
            return Err(Error::from(E_FAIL));
        }
        self.dcs.push(dc);
        let bitmap = CreateCompatibleBitmap(self.screen, w, h);
        if bitmap.is_invalid() {
            return Err(Error::from(E_FAIL));
        }
        self.bitmaps.push(bitmap);
        SelectObject(dc, HGDIOBJ(bitmap.0));
        Ok((dc, bitmap))
    }
}

/// The whole virtual screen with the cursor drawn in, at most `max` wide,
/// as JPEG.
fn frame(max: Option<u32>) -> windows::core::Result<Frame> {
    let screen = screen();
    if screen.width <= 0 || screen.height <= 0 {
        return Err(Error::from(E_FAIL));
    }
    let (w, h) = share::scaled(screen.width as u32, screen.height as u32, max);
    let (w, h) = (w as i32, h as i32);
    // SAFETY: every DC and bitmap used below is made here, checked, and
    // owned by `gdi`, which outlives each use and frees them on every way
    // out. The sizes handed to the blits are those the bitmaps were made
    // with.
    unsafe {
        let mut gdi = Gdi {
            screen: GetDC(None),
            dcs: Vec::new(),
            bitmaps: Vec::new(),
        };
        if gdi.screen.is_invalid() {
            return Err(Error::from(E_FAIL));
        }
        let (full, full_bitmap) = gdi.canvas(screen.width, screen.height)?;
        // CAPTUREBLT takes layered windows too: menus, tooltips, toasts.
        BitBlt(
            full,
            0,
            0,
            screen.width,
            screen.height,
            Some(gdi.screen),
            screen.left,
            screen.top,
            SRCCOPY | CAPTUREBLT,
        )?;
        draw_cursor(full, screen);
        let bitmap = if (w, h) == (screen.width, screen.height) {
            full_bitmap
        } else {
            let (small, small_bitmap) = gdi.canvas(w, h)?;
            SetStretchBltMode(small, HALFTONE);
            let _ = SetBrushOrgEx(small, 0, 0, None);
            // A failed stretch leaves the small bitmap black, and a black
            // frame looks like a screen that is off.
            if !StretchBlt(
                small,
                0,
                0,
                w,
                h,
                Some(full),
                0,
                0,
                screen.width,
                screen.height,
                SRCCOPY,
            )
            .as_bool()
            {
                return Err(Error::from(E_FAIL));
            }
            small_bitmap
        };
        // WIC reads a bitmap that no DC has selected more surely.
        for dc in gdi.dcs.drain(..) {
            let _ = DeleteDC(dc);
        }
        let jpeg = encode_jpeg(bitmap)?;
        Ok(Frame { screen, jpeg })
    }
}

/// The cursor where it is, so the viewer sees what the mouse points at:
/// a screen copy leaves it out.
///
/// SAFETY: `dc` must be a live DC, as `frame` hands it.
unsafe fn draw_cursor(dc: HDC, screen: Screen) {
    let mut info = CURSORINFO {
        cbSize: std::mem::size_of::<CURSORINFO>() as u32,
        ..Default::default()
    };
    if GetCursorInfo(&mut info).is_err()
        || info.flags.0 & CURSOR_SHOWING.0 == 0
        || info.hCursor.is_invalid()
    {
        return;
    }
    let icon = HICON(info.hCursor.0);
    let mut icon_info = ICONINFO::default();
    let (hot_x, hot_y) = if GetIconInfo(icon, &mut icon_info).is_ok() {
        // GetIconInfo hands over copies of the cursor's bitmaps.
        if !icon_info.hbmMask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(icon_info.hbmMask.0));
        }
        if !icon_info.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(icon_info.hbmColor.0));
        }
        (icon_info.xHotspot as i32, icon_info.yHotspot as i32)
    } else {
        (0, 0)
    };
    let _ = DrawIconEx(
        dc,
        info.ptScreenPos.x - screen.left - hot_x,
        info.ptScreenPos.y - screen.top - hot_y,
        icon,
        0,
        0,
        0,
        None,
        DI_NORMAL,
    );
}

/// JPEG through the Windows Imaging Component, which ships with Windows
/// and so costs no dependency.
///
/// SAFETY: `bitmap` must be a live bitmap no DC has selected, and COM
/// must be joined on this thread, as `Com` does for a frame request.
unsafe fn encode_jpeg(bitmap: HBITMAP) -> windows::core::Result<Vec<u8>> {
    let factory: IWICImagingFactory =
        CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
    let source =
        factory.CreateBitmapFromHBITMAP(bitmap, HPALETTE::default(), WICBitmapIgnoreAlpha)?;
    let converter = factory.CreateFormatConverter()?;
    converter.Initialize(
        &source,
        &GUID_WICPixelFormat24bppBGR,
        WICBitmapDitherTypeNone,
        None,
        0.0,
        WICBitmapPaletteTypeCustom,
    )?;
    let stream = SHCreateMemStream(None).ok_or_else(|| Error::from(E_FAIL))?;
    let encoder = factory.CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null())?;
    encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
    let mut frame = None;
    let mut options: Option<IPropertyBag2> = None;
    encoder.CreateNewFrame(&mut frame, &mut options)?;
    let frame = frame.ok_or_else(|| Error::from(E_FAIL))?;
    if let Some(options) = &options {
        // Text stays readable at 0.7, and a frame is a third of what the
        // default 0.9 makes.
        let mut name: Vec<u16> = "ImageQuality\0".encode_utf16().collect();
        let bag = PROPBAG2 {
            pstrName: PWSTR(name.as_mut_ptr()),
            ..Default::default()
        };
        let _ = options.Write(1, &bag, &VARIANT::from(0.7f32));
    }
    frame.Initialize(options.as_ref())?;
    frame.WriteSource(&converter, std::ptr::null())?;
    frame.Commit()?;
    encoder.Commit()?;

    let mut size = 0u64;
    stream.Seek(0, STREAM_SEEK_END, Some(&mut size))?;
    stream.Seek(0, STREAM_SEEK_SET, None)?;
    let mut jpeg = vec![0u8; size as usize];
    let mut read = 0u32;
    stream
        .Read(jpeg.as_mut_ptr().cast(), jpeg.len() as u32, Some(&mut read))
        .ok()?;
    jpeg.truncate(read as usize);
    Ok(jpeg)
}

/// A desktop handle, closed however its use ends.
struct Desk(HDESK);

impl Drop for Desk {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenInputDesktop and is closed
        // nowhere else.
        let _ = unsafe { CloseDesktop(self.0) };
    }
}

/// A process or token handle, closed however its use ends.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenProcess or OpenProcessToken
        // and is closed nowhere else.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Whether the desktop that takes input now is the user's own. While the
/// machine is locked or a UAC prompt shows, it is Winlogon's, which this
/// process may not even open: input sent then goes nowhere, and Windows
/// does not say so.
fn own_desktop() -> Result<(), String> {
    // SAFETY: no pointers go in, and the handle that comes out is owned
    // by `Desk` from the next line on.
    let desk = unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) }
        .map_err(|_| LOCKED.to_string())?;
    let desk = Desk(desk);
    let mut name = [0u16; 64];
    let mut needed = 0u32;
    // SAFETY: the buffer is `name`, its length given in bytes as asked,
    // and both it and `needed` outlive the call. The handle is open.
    unsafe {
        GetUserObjectInformationW(
            HANDLE(desk.0 .0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            std::mem::size_of_val(&name) as u32,
            Some(&mut needed),
        )
    }
    .map_err(|_| LOCKED.to_string())?;
    let end = name.iter().position(|c| *c == 0).unwrap_or(name.len());
    if String::from_utf16_lossy(&name[..end]).eq_ignore_ascii_case("Default") {
        Ok(())
    } else {
        Err(LOCKED.into())
    }
}

/// Whether the token of `process` is an elevated one, None when the
/// token will not say.
fn token_elevated(process: HANDLE) -> Option<bool> {
    let mut token = HANDLE::default();
    // SAFETY: `process` is an open process handle (or the pseudo handle
    // of this one) and `token` outlives the call.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.ok()?;
    let token = Handle(token);
    let mut info = TOKEN_ELEVATION::default();
    let mut length = 0u32;
    // SAFETY: the buffer is `info`, a TOKEN_ELEVATION as this class
    // fills, with its own size given; it and `length` outlive the call.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some((&raw mut info).cast()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut length,
        )
    }
    .ok()?;
    Some(info.TokenIsElevated != 0)
}

/// Whether this Horadric runs as administrator, asked once: it does not
/// change while a process lives.
fn we_are_elevated() -> bool {
    static OURS: OnceLock<bool> = OnceLock::new();
    *OURS.get_or_init(|| {
        // SAFETY: GetCurrentProcess hands a pseudo handle that needs no
        // closing and is always good.
        token_elevated(unsafe { GetCurrentProcess() }).unwrap_or(false)
    })
}

/// Whether input to `window` would be dropped because its process runs
/// as administrator and this one does not. Windows drops it silently, so
/// it is found out before sending. A process that cannot even be asked
/// counts as one: that is what being above this one looks like.
fn above_us(window: HWND) -> bool {
    if window.0.is_null() || we_are_elevated() {
        return false;
    }
    let mut pid = 0u32;
    // SAFETY: `pid` outlives the call; a window gone meanwhile leaves it 0.
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    if pid == 0 {
        return false;
    }
    // SAFETY: no pointers go in, and the handle that comes out is owned
    // by `Handle` at once.
    let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        return true;
    };
    let process = Handle(process);
    token_elevated(process.0).unwrap_or(true)
}

/// What an input is aimed at, which decides what must be checked before
/// it is sent.
#[derive(Clone, Copy)]
enum Target {
    /// Nothing in particular: a pointer only moving reaches no program
    /// that could refuse it.
    Desktop,
    /// The window with the keyboard.
    Foreground,
    /// The window at a point, in pixels of the virtual screen.
    At(POINT),
}

/// Whether input aimed at `target` would arrive, or why not.
fn reachable(target: Target) -> Result<(), String> {
    own_desktop()?;
    let window = match target {
        Target::Desktop => return Ok(()),
        // SAFETY: takes nothing; a null answer is no window, which
        // `above_us` lets through.
        Target::Foreground => unsafe { GetForegroundWindow() },
        // SAFETY: takes a point by value; a null answer as above.
        Target::At(point) => unsafe { WindowFromPoint(point) },
    };
    if above_us(window) {
        Err(ELEVATED.into())
    } else {
        Ok(())
    }
}

/// The keys and buttons this server put down and has not let up, so they
/// can be let up when the viewer that pressed them is gone.
#[derive(Default)]
struct Held {
    /// Each key with whether it was pressed as an extended one, to let
    /// up the very key that went down.
    keys: Vec<(u16, bool)>,
    buttons: Vec<Button>,
}

/// Plays a sequence of events as one, under the one lock, or says why it
/// stopped. A sequence that stops midway leaves nothing held down: half
/// a shortcut with Ctrl still down types into whatever comes next.
fn run(shared: &Shared, events: &[Input]) -> Result<(), String> {
    let mut held = lock(&shared.held);
    let mut done = Ok(());
    for event in events {
        if let Err(why) = play(&mut held, event) {
            release(&mut held);
            done = Err(why);
            break;
        }
    }
    // A long text takes longer to type than the watchdog waits, and what
    // was held before it must not be let go the moment it ends.
    shared.hear();
    done
}

/// Lets go of what is held once the viewer has been silent too long. It
/// runs for as long as the app does.
fn watch(shared: &Shared) {
    loop {
        thread::sleep(Duration::from_secs(1));
        let mut held = lock(&shared.held);
        if held.keys.is_empty() && held.buttons.is_empty() {
            continue;
        }
        let heard = shared.heard.load(Ordering::Relaxed).saturating_sub(1);
        if shared.now().saturating_sub(heard) >= share::HELD_FOR_MS {
            release(&mut held);
        }
    }
}

/// Lets up every key and button held. Nothing is checked first and a
/// refusal is not acted on: there is nothing better to do with one.
fn release(held: &mut Held) {
    let mut inputs: Vec<INPUT> = (held.buttons.drain(..))
        // Where the pointer is, since the viewer is not saying where.
        .map(|b| mouse_here(button_flag(b, false)))
        .collect();
    inputs.extend(held.keys.drain(..).map(|(vk, ext)| stroke(vk, ext, false)));
    for piece in inputs.chunks(40) {
        // SAFETY: the slice and the size describe the same INPUTs.
        unsafe { SendInput(piece, std::mem::size_of::<INPUT>() as i32) };
    }
}

/// Plays one event into this desktop, or says why it would not arrive.
fn play(held: &mut Held, event: &Input) -> Result<(), String> {
    let screen = screen();
    let at = |x: i32, y: i32| {
        (
            share::absolute(x, screen.width),
            share::absolute(y, screen.height),
        )
    };
    // The same pixel `absolute` aims at, as the window lookup wants it.
    let window_at = |x: i32, y: i32| {
        Target::At(POINT {
            x: screen.left + x.clamp(0, (screen.width - 1).max(0)),
            y: screen.top + y.clamp(0, (screen.height - 1).max(0)),
        })
    };
    match event {
        Input::Alive => Ok(()),
        Input::Release => {
            release(held);
            Ok(())
        }
        Input::Move { x, y } => send(
            &[mouse(at(*x, *y), MOUSE_EVENT_FLAGS(0), 0)],
            Target::Desktop,
        ),
        Input::Down { x, y, button } => {
            // Noted before it is sent: a send that fails halfway is then
            // undone with the rest.
            if !held.buttons.contains(button) {
                held.buttons.push(*button);
            }
            send(
                &[mouse(at(*x, *y), button_flag(*button, true), 0)],
                window_at(*x, *y),
            )
        }
        Input::Up { x, y, button } => {
            // A viewer that retries, or whose button the watchdog let up
            // meanwhile, sends an up too many. It is nothing to refuse.
            if !held.buttons.contains(button) {
                return Ok(());
            }
            send(
                &[mouse(at(*x, *y), button_flag(*button, false), 0)],
                window_at(*x, *y),
            )?;
            held.buttons.retain(|b| b != button);
            Ok(())
        }
        // The wheel turns what is under the pointer, so it goes there first.
        Input::Wheel { x, y, delta } => send(
            &[mouse(at(*x, *y), MOUSEEVENTF_WHEEL, *delta)],
            window_at(*x, *y),
        ),
        Input::Hwheel { x, y, delta } => send(
            &[mouse(at(*x, *y), MOUSEEVENTF_HWHEEL, *delta)],
            window_at(*x, *y),
        ),
        Input::Key { vk, down, ext } => {
            let key = (*vk, ext.unwrap_or_else(|| share::extended_key(*vk)));
            if *down && !held.keys.contains(&key) {
                held.keys.push(key);
            }
            // An up too many is let pass, as for a button.
            if !*down && !held.keys.contains(&key) {
                return Ok(());
            }
            send(&[stroke(key.0, key.1, *down)], Target::Foreground)?;
            if !*down {
                held.keys.retain(|k| *k != key);
            }
            Ok(())
        }
        Input::Text { text } => {
            let mut inputs = Vec::new();
            for part in share::typed(text) {
                match part {
                    Typed::Units(units) => inputs.extend(units.into_iter().flat_map(|unit| {
                        [
                            key(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE),
                            key(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
                        ]
                    })),
                    Typed::Enter => inputs.extend(press(VK_RETURN)),
                    Typed::Tab => inputs.extend(press(VK_TAB)),
                }
            }
            send(&inputs, Target::Foreground)
        }
    }
}

/// Sends inputs aimed at `target`, checking before each piece that they
/// would arrive: a long text is minutes of pieces, and the window with
/// the keyboard can change under it.
fn send(inputs: &[INPUT], target: Target) -> Result<(), String> {
    // A long text in one SendInput outruns the program reading its queue:
    // Notepad dropped the rest of the text, or repeated its last character.
    // Forty keeps a key's down and up in one piece, as each is two.
    for (i, piece) in inputs.chunks(40).enumerate() {
        if i > 0 {
            thread::sleep(Duration::from_millis(15));
        }
        reachable(target)?;
        // SAFETY: the slice and the size describe the same INPUTs.
        let sent = unsafe { SendInput(piece, std::mem::size_of::<INPUT>() as i32) };
        // The checks above name the reasons known. A short count is
        // another program blocking input, which Windows does not name.
        if (sent as usize) < piece.len() {
            return Err("Windows took only part of the input: something blocks it".into());
        }
    }
    Ok(())
}

fn button_flag(button: Button, down: bool) -> MOUSE_EVENT_FLAGS {
    match (button, down) {
        (Button::Left, true) => MOUSEEVENTF_LEFTDOWN,
        (Button::Left, false) => MOUSEEVENTF_LEFTUP,
        (Button::Right, true) => MOUSEEVENTF_RIGHTDOWN,
        (Button::Right, false) => MOUSEEVENTF_RIGHTUP,
        (Button::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
        (Button::Middle, false) => MOUSEEVENTF_MIDDLEUP,
    }
}

fn mouse((dx, dy): (i32, i32), flags: MOUSE_EVENT_FLAGS, wheel: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: wheel as u32,
                dwFlags: flags | MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// A mouse event where the pointer already is.
fn mouse_here(flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dwFlags: flags,
                ..Default::default()
            },
        },
    }
}

/// One half of a key press, by virtual key.
fn stroke(vk: u16, extended: bool, down: bool) -> INPUT {
    let mut flags = if down {
        KEYBD_EVENT_FLAGS(0)
    } else {
        KEYEVENTF_KEYUP
    };
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    // Some programs read the scan code, not the virtual key.
    // SAFETY: MapVirtualKeyW takes two numbers and hands one back.
    let scan = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) } as u16;
    key(VIRTUAL_KEY(vk), scan, flags)
}

/// A key of the main block going down and up.
fn press(vk: VIRTUAL_KEY) -> [INPUT; 2] {
    [stroke(vk.0, false, true), stroke(vk.0, false, false)]
}

/// The primary display, by its device name, so the resolution read and
/// the one set are of the same monitor.
struct Display {
    /// The device name, NUL ended, or empty when no device said it was
    /// primary: Windows then takes its default display, which is it.
    name: Vec<u16>,
}

/// Why a resolution was not set or kept.
enum Refused {
    NotOffered,
    Windows(String),
    /// Nothing about the resolution: this server could not do its part.
    Broken(String),
}

impl Display {
    fn primary() -> Display {
        let mut device = DISPLAY_DEVICEW {
            cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        for i in 0.. {
            // SAFETY: `device` is a DISPLAY_DEVICEW with `cb` set, as asked.
            if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), i, &mut device, 0) }.as_bool() {
                break;
            }
            if device.StateFlags.0 & DISPLAY_DEVICE_PRIMARY_DEVICE.0 != 0 {
                let end = (device.DeviceName.iter().position(|c| *c == 0))
                    .unwrap_or(device.DeviceName.len());
                let mut name = device.DeviceName[..end].to_vec();
                name.push(0);
                return Display { name };
            }
        }
        Display { name: Vec::new() }
    }

    fn device(&self) -> PCWSTR {
        if self.name.is_empty() {
            PCWSTR::null()
        } else {
            PCWSTR(self.name.as_ptr())
        }
    }

    fn settings(&self, which: ENUM_DISPLAY_SETTINGS_MODE) -> Option<DEVMODEW> {
        let mut mode = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        // SAFETY: the name outlives the call and ends in NUL, and `mode`
        // is a DEVMODEW with `dmSize` set, as asked.
        unsafe { EnumDisplaySettingsW(self.device(), which, &mut mode) }
            .as_bool()
            .then_some(mode)
    }

    /// The mode in use, all zero when Windows will not say.
    fn current(&self) -> Mode {
        self.settings(ENUM_CURRENT_SETTINGS)
            .as_ref()
            .map(mode_of)
            .unwrap_or(Mode {
                width: 0,
                height: 0,
                bits: 0,
                refresh: 0,
            })
    }

    /// Every mode the driver lists, refresh rates and all.
    fn all(&self) -> Vec<DEVMODEW> {
        (0..)
            .map_while(|i| self.settings(ENUM_DISPLAY_SETTINGS_MODE(i)))
            .collect()
    }

    fn modes(&self) -> Vec<Mode> {
        self.all().iter().map(mode_of).collect()
    }

    /// The driver's own mode for `size`, ready to switch to. Its own
    /// rather than the current one with a new size edited in, which the
    /// driver may not take.
    fn mode_for(&self, size: Size) -> Result<DEVMODEW, Refused> {
        let current = self.current();
        let all = self.all();
        let modes: Vec<Mode> = all.iter().map(mode_of).collect();
        if !share::display_sizes(&modes, current.bits).contains(&size) {
            return Err(Refused::NotOffered);
        }
        let picked = share::pick_mode(&modes, size, current.bits, current.refresh)
            .and_then(|m| modes.iter().position(|o| *o == m))
            .ok_or(Refused::NotOffered)?;
        let mut mode = all[picked];
        mode.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT;
        if mode.dmBitsPerPel != 0 {
            mode.dmFields |= DM_BITSPERPEL;
        }
        // 0 and 1 stand for the hardware's default rate, not a rate.
        if mode.dmDisplayFrequency > 1 {
            mode.dmFields |= DM_DISPLAYFREQUENCY;
        }
        Ok(mode)
    }

    /// Asks Windows for `mode`, or with None for the mode the registry
    /// holds, and hands back why it refused, None when it did not.
    fn change(&self, mode: Option<&DEVMODEW>, flags: CDS_TYPE) -> Option<String> {
        // SAFETY: the name outlives the call and ends in NUL (or is
        // null), and a mode given came from EnumDisplaySettingsW with
        // its size set and outlives the call too.
        let code = unsafe {
            ChangeDisplaySettingsExW(
                self.device(),
                mode.map(std::ptr::from_ref),
                None,
                flags,
                None,
            )
        };
        share::disp_change_refusal(code.0)
    }

    /// Back to the mode the registry holds, which a change that was not
    /// kept never touched.
    fn revert(&self) -> Option<String> {
        self.change(None, CDS_TYPE(0))
    }
}

/// Shows `size` on trial: tested first, then switched to without being
/// saved, and taken back unless [`keep`] comes in time. A resolution the
/// viewer cannot work in, or cannot see at all, then mends itself.
fn resize(shared: &Arc<Shared>, size: Size) -> Result<(), Refused> {
    let display = Display::primary();
    let mode = display.mode_for(size)?;
    // Held to the end, so a keep or a timer running out sees the change
    // before or after this one, never half of it.
    let mut trial = lock(&shared.trial);
    if let Some(why) = display.change(Some(&mode), CDS_TEST) {
        return Err(Refused::Windows(why));
    }
    // No flags: a change for now that the registry does not hear of.
    if let Some(why) = display.change(Some(&mode), CDS_TYPE(0)) {
        return Err(Refused::Windows(why));
    }
    // The change before, if one still waited, is replaced: the registry
    // holds what was there before both, so that is what comes back. Only
    // when the primary display itself changed between them is the other
    // one's trial ended here, as nothing else would end it.
    if let Some((before, _)) = trial.keep() {
        if before.name != display.name {
            before.revert();
        }
    }
    let number = trial.begin((display, mode));
    log(&format!(
        "display changed to {}x{}, on trial",
        size.width, size.height
    ));

    let timed = Arc::clone(shared);
    let spawned = thread::Builder::new()
        .name("screen-share-display".into())
        .spawn(move || {
            thread::sleep(Duration::from_secs(share::KEEP_WITHIN));
            if let Some((display, _)) = lock(&timed.trial).expire(number) {
                log(&match display.revert() {
                    None => format!(
                        "display reverted, {}x{} was not kept",
                        size.width, size.height
                    ),
                    Some(why) => format!("display could not be reverted: {why}"),
                });
            }
        });
    if let Err(e) = spawned {
        // Nothing would take it back, so it is taken back now.
        if let Some((display, _)) = trial.expire(number) {
            display.revert();
        }
        log("display reverted, no thread for the timer");
        return Err(Refused::Broken(format!(
            "no timer to take the resolution back with: {e}"
        )));
    }
    Ok(())
}

/// Saves the resolution on trial, so it stays.
fn keep(shared: &Shared) -> Result<(), Refused> {
    let mut trial = lock(&shared.trial);
    let Some((display, mode)) = trial.keep() else {
        return Err(Refused::Windows(
            "too late: the resolution went back".into(),
        ));
    };
    let (width, height) = (mode.dmPelsWidth, mode.dmPelsHeight);
    match display.change(Some(&mode), CDS_UPDATEREGISTRY) {
        None => {
            log(&format!("display {width}x{height} kept"));
            Ok(())
        }
        Some(why) => {
            // Not saved, and its timer is spent: left as it is, it would
            // last until the next sign out with nobody having chosen that.
            display.revert();
            log(&format!(
                "display reverted, {width}x{height} could not be kept: {why}"
            ));
            Err(Refused::Windows(why))
        }
    }
}

/// Takes the resolution on trial back now, without waiting for its
/// timer, which then finds nothing left to do. With none on trial the
/// display is at the registry's mode already.
fn revert(shared: &Shared) {
    if let Some((display, mode)) = lock(&shared.trial).keep() {
        let (width, height) = (mode.dmPelsWidth, mode.dmPelsHeight);
        log(&match display.revert() {
            None => format!("display reverted, {width}x{height} was given up"),
            Some(why) => format!("display could not be reverted: {why}"),
        });
    }
}

fn mode_of(m: &DEVMODEW) -> Mode {
    Mode {
        width: m.dmPelsWidth,
        height: m.dmPelsHeight,
        bits: m.dmBitsPerPel,
        refresh: m.dmDisplayFrequency,
    }
}

fn key(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}
