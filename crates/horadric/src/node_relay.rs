//! A node's link to the relay, as a Windows PC keeps one (see
//! docs/plan/the-brotherhood.md, "Linux nodes on the relay"): the same
//! `/pc` endpoint, secret and id, signed in with its WorkOS access token,
//! so a PC cannot tell it from another PC but by what its hello says. The
//! link, its threads and the ping, silence, backoff and refresh rules are
//! `horadric_link::relay`, which the Mac runs too; this is what a node
//! hands it: its sign in, its human and the phones it serves.
//!
//! What linked channels carry goes to the serving thread: a brother's asks
//! into the node's one `Server`, as a viewer's on the unix socket do, and
//! its answers to the node's client of it (`kin`). What only a human can
//! answer goes to a terminal attached over the control socket
//! (`control`), and with none attached the node says no.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use horadric_core::brothers::{Brothers, Event as LinkEvent, Listener};
use horadric_core::joining::{Listed, Store};
use horadric_core::phoning::{self, Chans, Heard};
use horadric_core::portal::frame;
use horadric_link::relay::{self, Link, Setup};
use serde_json::Value;

pub(super) use horadric_link::relay::{now, LinkCrypto};

use super::{control, signin, Event};

/// Where a relay channel sits among the serving thread's channels: past
/// every number the unix socket's viewers get, so the two never meet.
pub(super) const RELAY: u32 = 1 << 31;

/// The serving thread's channel for relay channel `ch`.
pub(super) fn inner(ch: u32) -> u32 {
    ch | RELAY
}

/// What the relay link says to the serving thread.
pub(super) enum Said {
    /// A brother's channel is linked.
    Up {
        ch: u32,
        id: String,
        name: String,
    },
    /// Something the brother asks of this node, for the server.
    Asked {
        ch: u32,
        id: String,
        msg: Value,
    },
    /// An answer to what this node asked of the brother.
    Heard {
        ch: u32,
        msg: Value,
    },
    Down {
        ch: u32,
    },
    /// Brother `id` was cast out, here or by the roster.
    Gone {
        id: String,
    },
    /// Sessions of this node a roster says another owns now.
    Moved {
        owners: Vec<(String, horadric_core::brotherhood::Owner)>,
    },
    /// Something a phone the word vouches for asks, for the server.
    Phone {
        ch: u32,
        id: String,
        msg: Value,
    },
    /// A phone's channel closed, or its phone was forgotten.
    PhoneDown {
        ch: u32,
    },
}

/// Who a phone is to the server: apart from every brother's id and every
/// viewer's name, so control can tell it from them.
pub(super) const PHONE: &str = "phone:";

/// The node's relay link, once started.
static LINK: OnceLock<Link> = OnceLock::new();

/// The phones on the relay link: a node pairs none, but serves each the
/// signed word vouches for (docs/plan/one-portal.md). Shared by the link,
/// which hears them, and the serving thread, which answers them.
struct Phones {
    chans: Mutex<Chans>,
    events: Mutex<Sender<Event>>,
    name: String,
}

static PHONES: OnceLock<Arc<Phones>> = OnceLock::new();

impl Phones {
    fn chans(&self) -> MutexGuard<'_, Chans> {
        self.chans.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn tell(&self, said: Said) {
        let tx = self.events.lock().unwrap_or_else(|e| e.into_inner());
        let _ = tx.send(Event::Relay(said));
    }
}

/// Lets the link hand the channels no brother takes to the one [`Phones`].
struct ToPhones(Arc<Phones>);

impl relay::Phones for ToPhones {
    fn open(&self, ch: u32) {
        self.0.chans().open(ch);
    }

    fn close(&self, ch: u32) {
        let was = self.0.chans().close(ch);
        if was {
            self.0.tell(Said::PhoneDown { ch });
        }
    }

    /// What a phone asks goes to the serving thread; a forget is signed
    /// here, so every brother drops the phone, and cut off at once.
    fn frame(&self, link: &Link, ch: u32, payload: &[u8]) {
        let heard = self.0.chans().frame(ch, payload, &LinkCrypto);
        match heard {
            Heard::Nothing => {}
            Heard::Asked { id, msg } => self.0.tell(Said::Phone { ch, id, msg }),
            Heard::Forget { id, public } => {
                if let (Some(dir), Some(me)) = (horadric_hooks::state_dir(), me()) {
                    let signer = horadric_core::said::Signer::of(&dir, &me, &LinkCrypto);
                    if !public.is_empty() {
                        horadric_core::mesh::forget(&dir, signer.as_ref(), &id, &public, now());
                    }
                }
                self.look(link);
            }
        }
    }

    /// Reads the signed word for the phones it serves: each channel of one
    /// it no longer serves is told so and dropped, and every greeted phone
    /// hears the brothers when they changed.
    fn look(&self, link: &Link) {
        let (Some(dir), Some(me)) = (horadric_hooks::state_dir(), me()) else {
            return;
        };
        let held = horadric_core::mesh::load_held(&dir);
        let signer = horadric_core::said::Signer::of(&dir, &me, &LinkCrypto);
        let linked: Vec<String> = link
            .with(|b| b.linked().into_iter().map(|(_, id)| id).collect())
            .unwrap_or_default();
        let roster =
            horadric_core::rostering::served(&dir, &me, &self.0.name, &LinkCrypto, &linked)
                .unwrap_or_default();
        let state = |id: &str| -> &'static str {
            if linked.iter().any(|l| l == id) {
                "live"
            } else {
                "away"
            }
        };
        let list = phoning::brothers_message(&roster, &me, state, |_| None);
        let mut chans = self.0.chans();
        let cut = chans.look(&held, &me, signer.as_ref().map(|s| &s.private), &LinkCrypto);
        let told = chans.tell(&list, &LinkCrypto);
        drop(chans);
        for (ch, sealed) in cut {
            link.send_binary(frame(ch, &sealed));
            self.0.tell(Said::PhoneDown { ch });
        }
        for (ch, sealed) in told {
            link.send_binary(frame(ch, &sealed));
        }
    }
}

/// Seals `msg` for the phone on relay channel `ch` and sends it.
pub(super) fn phone_reply(ch: u32, msg: &Value) {
    let (Some(p), Some(link)) = (PHONES.get(), LINK.get()) else {
        return;
    };
    let sealed = p.chans().seal(ch, msg, &LinkCrypto);
    if let Some(sealed) = sealed {
        link.send_binary(frame(ch, &sealed));
    }
}

/// Hands linked channels' doings to the serving thread.
struct ToServing(Mutex<Sender<Event>>);

impl Listener for ToServing {
    fn event(&self, e: LinkEvent) {
        let said = match e {
            LinkEvent::Up { ch, id, name } => Said::Up { ch, id, name },
            LinkEvent::Asked { ch, id, msg } => Said::Asked { ch, id, msg },
            LinkEvent::Heard { ch, msg } => Said::Heard { ch, msg },
            LinkEvent::Down { ch } => Said::Down { ch },
            LinkEvent::Gone { id } => Said::Gone { id },
            LinkEvent::Moved { owners } => Said::Moved { owners },
        };
        let tx = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let _ = tx.send(Event::Relay(said));
    }
}

/// Runs `f` on the brothers, None before the link started.
pub(super) fn with<R>(f: impl FnOnce(&mut Brothers) -> R) -> Option<R> {
    LINK.get()?.with(f)
}

/// This node's relay id, which brothers and the roster know it by.
pub(super) fn me() -> Option<String> {
    relay::me(&horadric_hooks::state_dir()?)
}

/// Starts the relay link: made once, kept up for as long as the node
/// runs, connected while the node is signed in.
pub(super) fn start(name: String, events: Sender<Event>) -> Result<(), String> {
    let dir = horadric_hooks::state_dir().ok_or("cannot find the state folder")?;
    let phones = Arc::new(Phones {
        chans: Mutex::new(Chans::default()),
        events: Mutex::new(events.clone()),
        name: name.clone(),
    });
    let _ = PHONES.set(Arc::clone(&phones));
    let setup = Setup {
        dir,
        name,
        sign_in: signin::workos()?,
        human: Box::new(control::Human),
        listener: Box::new(ToServing(Mutex::new(events))),
        phones: Some(Box::new(ToPhones(phones))),
        log: |line| eprintln!("horadric node: {line}"),
    };
    let link = Link::start(setup)?;
    let _ = LINK.set(link);
    Ok(())
}

/// Makes the link connect again, signed in now or not: after
/// `horadric node signin` or `signout` in a terminal.
pub(super) fn renew() {
    if let Some(link) = LINK.get() {
        link.renew();
    }
}

/// Whether the relay is reached now.
pub(super) fn up() -> bool {
    LINK.get().is_some_and(Link::up)
}

/// Seals `msg` for the brother on relay channel `ch` and sends it. False
/// when that link is gone.
pub(super) fn reply(ch: u32, msg: &Value) -> bool {
    LINK.get().is_some_and(|l| l.reply(ch, msg))
}

/// The brothers the relay lists now, None while it is not reached.
pub(super) fn listed() -> Option<Vec<Listed>> {
    LINK.get()?.listed()
}

/// The joined brothers as kept, read from the folder before the link
/// starts.
pub(super) fn store() -> Store {
    LINK.get()
        .map(Link::store)
        .or_else(|| horadric_hooks::state_dir().and_then(|d| Store::load(&d)))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_channels_and_viewers_never_share_a_number() {
        assert_eq!(inner(7) & !RELAY, 7);
        assert_ne!(inner(1), 1);
        assert!(inner(0) > u32::MAX / 2);
    }
}
