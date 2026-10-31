//! A machine's link to the relay as a brother, for a machine whose
//! platform has no link of its own: what `crates/horadric/src/node_relay.rs`
//! does for a Linux node, with the sign in, the human and the listener
//! handed in, so the Mac runs the same threads. The joining, the handshake
//! and the sealed frames are `horadric_core::brothers`; the backoff, ping,
//! silence and token refresh rules are `horadric_core::relaying`. What a
//! linked channel carries goes to the [`Listener`] the caller gave, which
//! must hand it on and return, as it is called with the brothers held.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use horadric_core::brothers::{Brothers, Hands, Human, Listener, Relay};
use horadric_core::joining::{Crypto, Listed, Store};
use horadric_core::portal::{unframe, Config};
use horadric_core::relaying::{
    after_failure, after_link, closed_word, pc_dial, refresh_message, Refresh, PING_SECS,
    SILENT_SECS,
};
use serde_json::{json, Value};

use crate::ws::Socket;

/// The crypto joining needs, from this crate, byte for byte what a
/// Windows PC makes with CNG.
pub struct LinkCrypto;

impl Crypto for LinkCrypto {
    fn random(&self, buf: &mut [u8]) {
        crate::random(buf);
    }

    fn pair(&self) -> Option<[u8; 96]> {
        Some(crate::ecdh::pair())
    }

    fn ecdh(&self, private: &[u8; 96], peer: &[u8]) -> Option<[u8; 32]> {
        crate::ecdh::agree(private, peer).ok()
    }

    fn seal_bound(&self, key: &[u8; 32], plain: &[u8], aad: &[u8]) -> Vec<u8> {
        crate::seal_bound(key, plain, aad)
    }

    fn open_bound(&self, key: &[u8; 32], sealed: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
        crate::open_bound(key, sealed, aad)
    }

    fn sign(&self, private: &[u8; 96], msg: &[u8]) -> Option<[u8; 64]> {
        crate::ecdsa::sign(private, msg)
    }

    fn verify(&self, public: &[u8], msg: &[u8], sig: &[u8]) -> bool {
        crate::ecdsa::verify(public, msg, sig)
    }
}

/// Who the link dials as: a machine not signed in has no brothers to find,
/// so the link waits for one.
pub trait SignIn: Send + Sync {
    fn signed_in(&self) -> bool;
    /// A good access token, refreshed if it must be. May block on WorkOS.
    fn access_token(&self) -> Result<String, String>;
    /// Drops the token in hand, so the next one is fresh: for one the
    /// relay refused.
    fn forget_access(&self);
}

/// What a link is started with.
pub struct Setup {
    /// The state folder, where `brothers.json` and `roster.json` live.
    pub dir: PathBuf,
    /// This machine's name as its brothers see it.
    pub name: String,
    pub sign_in: Arc<dyn SignIn>,
    pub human: Box<dyn Human>,
    pub listener: Box<dyn Listener>,
    /// Where the link says what happened to it, a log line each.
    pub log: fn(&str),
}

/// This machine's secret and relay address, made and kept (owner only)
/// the first time, as `portal.json` keeps a PC's. A machine that pairs no
/// phones keeps it switched off for phones: the relay's address is all it
/// needs.
pub fn config(dir: &Path) -> Result<Config, String> {
    if let Some(c) = Config::load(dir) {
        return Ok(c);
    }
    let mut secret = [0u8; 32];
    crate::random(&mut secret);
    let mut c = Config::fresh(&secret);
    c.enabled = false;
    if let Some(relay) = relay_from_env(std::env::var("HORADRIC_RELAY").ok().as_deref()) {
        c.relay = relay;
    }
    c.save(dir)
        .map_err(|e| format!("cannot keep this machine's secret: {e}"))?;
    Ok(c)
}

/// A relay named in the environment, for a dev relay: blank means none.
fn relay_from_env(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// This machine's relay id, which brothers and the roster know it by.
pub fn me(dir: &Path) -> Option<String> {
    Config::load(dir).map(|c| horadric_core::portal::id(&c.secret))
}

/// The socket while connected, and the sign in it was dialled under.
type Slot = Arc<Mutex<Option<(Socket, u64)>>>;

struct Shared {
    brothers: Mutex<Option<Brothers>>,
    up: AtomicBool,
    /// Goes up on every sign in and sign out, so the link connects again
    /// as the machine it now is.
    signed: AtomicU64,
    /// Where the in-band token refresh stands.
    refresh: Mutex<Refresh>,
    sign_in: Arc<dyn SignIn>,
    log: fn(&str),
}

/// A running relay link. Clones share it.
#[derive(Clone)]
pub struct Link(Arc<Shared>);

impl Link {
    /// Starts the link: made once, kept up for as long as the process
    /// runs, connected while the machine is signed in.
    pub fn start(setup: Setup) -> Result<Link, String> {
        let config = config(&setup.dir)?;
        let (out, rx) = mpsc::channel::<(bool, Vec<u8>)>();
        let hands = Hands {
            crypto: Box::new(LinkCrypto),
            relay: Box::new(ToRelay(out.clone())),
            human: setup.human,
            listener: setup.listener,
            clock: now,
            process: horadric_core::mesh::process(),
        };
        let me = horadric_core::portal::id(&config.secret);
        let link = Link(Arc::new(Shared {
            brothers: Mutex::new(Some(Brothers::new(
                &me,
                &setup.name,
                Some(setup.dir.clone()),
                hands,
            ))),
            up: AtomicBool::new(false),
            signed: AtomicU64::new(0),
            refresh: Mutex::new(Refresh::default()),
            sign_in: setup.sign_in,
            log: setup.log,
        }));
        let slot: Slot = Arc::default();
        {
            let slot = Arc::clone(&slot);
            thread::Builder::new()
                .name("relay-send".into())
                .spawn(move || write_loop(rx, slot))
                .map_err(|e| e.to_string())?;
        }
        {
            let (slot, out, link) = (Arc::clone(&slot), out.clone(), link.clone());
            thread::Builder::new()
                .name("relay-ping".into())
                .spawn(move || link.ping_loop(&slot, &out))
                .map_err(|e| e.to_string())?;
        }
        {
            let link = link.clone();
            let name = setup.name;
            thread::Builder::new()
                .name("relay-read".into())
                .spawn(move || link.read_loop(&config.relay, &config.secret, &name, &slot))
                .map_err(|e| e.to_string())?;
        }
        Ok(link)
    }

    fn brothers(&self) -> MutexGuard<'_, Option<Brothers>> {
        self.0.brothers.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs `f` on the brothers.
    pub fn with<R>(&self, f: impl FnOnce(&mut Brothers) -> R) -> Option<R> {
        self.brothers().as_mut().map(f)
    }

    /// Whether the relay is reached now.
    pub fn up(&self) -> bool {
        self.0.up.load(Ordering::SeqCst)
    }

    /// Makes the link connect again, signed in now or not: after a sign in
    /// or out.
    pub fn renew(&self) {
        self.0.sign_in.forget_access();
        self.0.signed.fetch_add(1, Ordering::SeqCst);
    }

    /// Seals `msg` for the brother on relay channel `ch` and sends it.
    /// False when that link is gone.
    pub fn reply(&self, ch: u32, msg: &Value) -> bool {
        self.with(|b| b.reply(ch, msg)).unwrap_or(false)
    }

    /// The brothers the relay lists now, None while it is not reached.
    pub fn listed(&self) -> Option<Vec<Listed>> {
        self.with(|b| b.listed()).flatten()
    }

    /// The joined brothers as kept.
    pub fn store(&self) -> Store {
        self.with(|b| b.store().clone()).unwrap_or_default()
    }

    fn log(&self, line: &str) {
        (self.0.log)(line);
    }

    /// Pings while connected, and closes a link gone quiet or dialled
    /// under an older sign in, which sends the reader round to connect
    /// again.
    fn ping_loop(&self, slot: &Slot, out: &Sender<(bool, Vec<u8>)>) {
        let mut since = 0;
        loop {
            thread::sleep(Duration::from_secs(1));
            let held = slot
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(|(s, g)| (s.clone(), *g));
            let Some((socket, signed)) = held else {
                since = 0;
                continue;
            };
            since += 1;
            if since >= PING_SECS {
                since = 0;
                let _ = out.send((true, br#"{"ping":1}"#.to_vec()));
            }
            let at = now();
            let due = *self.0.refresh.lock().unwrap_or_else(|e| e.into_inner());
            if due.due(at) {
                let fresh = self
                    .0
                    .sign_in
                    .access_token()
                    .ok()
                    .filter(|t| horadric_core::workos::expiry(t).is_some_and(|e| e > due.exp));
                if let Some(token) = fresh {
                    self.0
                        .refresh
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .sent(at);
                    let _ = out.send((true, refresh_message(&token)));
                }
            }
            let renewed = signed != self.0.signed.load(Ordering::SeqCst);
            if renewed || socket.quiet() > Duration::from_secs(SILENT_SECS) {
                socket.close();
            }
        }
    }

    fn read_loop(&self, relay: &str, secret: &str, name: &str, slot: &Slot) {
        let mut backoff = 1;
        loop {
            let signed = self.0.signed.load(Ordering::SeqCst);
            if !self.0.sign_in.signed_in() {
                self.wait_for_sign_in(signed);
                continue;
            }
            let dial = match self.0.sign_in.access_token() {
                Ok(token) => {
                    let exp = horadric_core::workos::expiry(&token).unwrap_or(0);
                    *self.0.refresh.lock().unwrap_or_else(|e| e.into_inner()) =
                        Refresh::dialled(exp);
                    pc_dial(relay, secret, Some((&token, name)))
                }
                Err(e) => {
                    self.log(&e);
                    let (wait, next) = after_failure(backoff.max(5));
                    backoff = next;
                    self.sleep_unless_renewed(wait, signed);
                    continue;
                }
            };
            let headers: Vec<(&str, &str)> =
                dial.headers.iter().map(|(n, v)| (*n, v.as_str())).collect();
            let wait = match Socket::connect_with(&dial.url, &headers) {
                Ok(socket) => {
                    *slot.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some((socket.clone(), signed));
                    let began = Instant::now();
                    self.0.up.store(true, Ordering::SeqCst);
                    self.with(|b| b.connected(true));
                    while let Some((text, data)) = socket.receive() {
                        self.heard(&socket, text, &data);
                    }
                    let code = socket.close_code();
                    *slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    self.0.up.store(false, Ordering::SeqCst);
                    self.with(|b| b.connected(false));
                    if let Some(word) = closed_word(code) {
                        self.log(word);
                    }
                    let (wait, next, stale) = after_link(began.elapsed().as_secs(), code, backoff);
                    if stale {
                        self.0.sign_in.forget_access();
                    }
                    self.log(&format!(
                        "the relay link closed ({code:?}), again in {wait} s"
                    ));
                    backoff = next;
                    wait
                }
                Err(e) => {
                    self.log(&format!("cannot reach the relay: {e}"));
                    let (wait, next) = after_failure(backoff);
                    backoff = next;
                    wait
                }
            };
            self.sleep_unless_renewed(wait, signed);
        }
    }

    /// One frame from the relay: what is for brothers to them; a phone,
    /// which this link never pairs, is closed.
    fn heard(&self, socket: &Socket, text: bool, data: &[u8]) {
        if text {
            let Ok(v) = serde_json::from_slice::<Value>(data) else {
                return;
            };
            // The relay's answer to a fresh token: a refused one is
            // forgotten, so the next is new.
            let answer = self
                .0
                .refresh
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .answer(&v, now());
            if let Some(refused) = answer {
                if refused {
                    self.0.sign_in.forget_access();
                }
                return;
            }
            if self.with(|b| b.text(&v)).unwrap_or(false) {
                return;
            }
            if let Some(ch) = v.get("open").and_then(Value::as_u64) {
                socket.send(true, json!({ "close": ch }).to_string().as_bytes());
            }
            return;
        }
        if let Some((ch, payload)) = unframe(data) {
            self.with(|b| b.binary(ch, payload));
        }
    }

    fn sleep_unless_renewed(&self, secs: u64, signed: u64) {
        for _ in 0..secs * 10 {
            if self.0.signed.load(Ordering::SeqCst) != signed {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn wait_for_sign_in(&self, signed: u64) {
        while self.0.signed.load(Ordering::SeqCst) == signed {
            thread::sleep(Duration::from_millis(500));
            if self.0.sign_in.signed_in() {
                return;
            }
        }
    }
}

/// Hands the brothers' frames to the writer thread, never blocking on the
/// network.
struct ToRelay(Sender<(bool, Vec<u8>)>);

impl Relay for ToRelay {
    fn text(&self, msg: &Value) {
        let _ = self.0.send((true, msg.to_string().into_bytes()));
    }

    fn binary(&self, frame: Vec<u8>) {
        let _ = self.0.send((false, frame));
    }
}

fn write_loop(rx: Receiver<(bool, Vec<u8>)>, slot: Slot) {
    for (text, frame) in rx {
        let socket = slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(s, _)| s.clone());
        if let Some(s) = socket {
            s.send(text, &frame);
        }
    }
}

/// Now, in Unix seconds.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_relay_in_the_environment_names_none() {
        assert_eq!(relay_from_env(None), None);
        assert_eq!(relay_from_env(Some("  ")), None);
        assert_eq!(
            relay_from_env(Some(" wss://dev.example ")).as_deref(),
            Some("wss://dev.example")
        );
    }

    #[test]
    fn a_secret_is_made_once_and_kept() {
        let dir = std::env::temp_dir().join(format!("horadric-link-relay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = config(&dir).unwrap();
        assert!(!first.enabled, "a link pairs no phones");
        let again = config(&dir).unwrap();
        assert_eq!(first.secret, again.secret);
        assert_eq!(me(&dir), Some(horadric_core::portal::id(&first.secret)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
