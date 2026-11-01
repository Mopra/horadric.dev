//! Pairing v4, the PC's half (docs/PORTAL.md, "Pairing v4: signed in, let
//! in on a PC"): a phone signed in as this PC's human asks on a channel
//! the relay opened as an ask, and the PC lets it in only on its human's
//! yes, given after comparing a six digit code. The login found the PC;
//! the code is what a relay in the middle cannot make match, so it is
//! what is trusted. The phone commits to its key before it hears the
//! PC's, so a relay cannot try keys until a code matches.
//!
//! What happens on each channel is here; the box, the relay and the
//! pairing itself are the app's.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use crate::joining::{code_of, derive, public_of, Crypto};
use crate::portal::{b64url, unb64url};

/// The handshake this build speaks.
pub const ASK_V: u64 = 1;
/// How long an ask lives from its channel opening, in seconds.
pub const EXPIRE_SECS: u64 = 300;
/// How many asks are held at once; more are refused, so a flood of
/// asks cannot pile boxes on the human's screen.
pub const MAX_ASKS: usize = 3;

/// What the phone sends first: its key and nonce, hidden until it hears
/// the PC's.
pub fn commit(public: &[u8], nonce: &[u8]) -> [u8; 32] {
    derive("horadric ask commit v1", &[public, nonce])
}

/// The code both humans compare and the key `paired` is sealed with,
/// from the ECDH secret `s` and each side's key and nonce, the phone's
/// first.
pub fn agree(s: &[u8; 32], phone: (&[u8], &[u8]), pc: (&[u8], &[u8])) -> (String, [u8; 32]) {
    let parts: [&[u8]; 5] = [s, phone.0, phone.1, pc.0, pc.1];
    let code = code_of(&derive("horadric ask code v1", &parts));
    (code, derive("horadric ask key v1", &parts))
}

/// What the PC's human is asked.
pub fn question_text(phone: &str, email: &str, code: &str) -> String {
    format!(
        "{phone} wants to use Town Portal, signed in as {email}. Does the phone show {code}? \
         Yes lets it see and type in this PC's sessions and every brother's."
    )
}

/// This PC as an ask needs it: its relay id, its name and its key pair.
pub struct Me {
    pub id: String,
    pub name: String,
    pub private: [u8; 96],
}

enum Stage {
    /// The channel is open; the phone's `ask` is due.
    Opened,
    /// `asking` went; the phone's `reveal` is due.
    Asking {
        name: String,
        commit: [u8; 32],
        private: [u8; 96],
        nonce: [u8; 16],
    },
    /// The human is asked, in the box with this token.
    Waiting {
        name: String,
        public: Vec<u8>,
        key: [u8; 32],
        token: u64,
    },
}

struct Ask {
    email: String,
    opened: u64,
    stage: Stage,
}

impl Ask {
    fn token(&self) -> Option<u64> {
        match self.stage {
            Stage::Waiting { token, .. } => Some(token),
            _ => None,
        }
    }
}

/// The question for the human, once the code is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub token: u64,
    pub code: String,
    pub text: String,
}

/// What one frame on an ask channel calls for.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Nothing,
    /// Send this, plain.
    Send(Value),
    /// Send `{"t":"refused"}`, plain, and close the channel. The ask is
    /// gone.
    Refuse,
    /// Show the human this question.
    Ask(Question),
    /// The ask is gone; the box with this token, if one is up, goes too.
    Dropped(Option<u64>),
}

/// A phone the human let in, for the app to pair as a v3 pairing does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LetIn {
    pub name: String,
    /// Its public key, b64url.
    pub public: String,
    /// What `paired` is sealed with.
    pub key: [u8; 32],
}

/// The human's answer, as the ask it was for takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Yes(LetIn),
    /// Send `{"t":"refused"}`, plain.
    No,
}

/// Every ask channel and the asks held on them.
#[derive(Default)]
pub struct Asks {
    /// Channels the relay opened as asks, held or not, so none of their
    /// frames is taken for a paired phone's.
    channels: HashSet<u32>,
    held: HashMap<u32, Ask>,
    next: u64,
}

impl Asks {
    /// Asks whose box tokens start at `first`, so an answer to a box of
    /// asks held before (a relay link since gone) is never taken for one
    /// of these.
    pub fn tokens_from(first: u64) -> Asks {
        Asks {
            next: first,
            ..Asks::default()
        }
    }

    /// Whether the relay opened `ch` as an ask.
    pub fn is_ask(&self, ch: u32) -> bool {
        self.channels.contains(&ch)
    }

    /// The relay opened `ch` as an ask from a phone signed in as `email`.
    /// False when [`MAX_ASKS`] are held already, for the caller to refuse.
    /// An ask held on `ch` already is replaced, its token given back.
    pub fn open(&mut self, ch: u32, email: &str, now: u64) -> (bool, Option<u64>) {
        self.channels.insert(ch);
        let old = self.held.remove(&ch).and_then(|a| a.token());
        if self.held.len() >= MAX_ASKS {
            return (false, old);
        }
        let email: String = email.trim().chars().take(254).collect();
        self.held.insert(
            ch,
            Ask {
                email,
                opened: now,
                stage: Stage::Opened,
            },
        );
        (true, old)
    }

    /// The channel closed: the token of a box it leaves up.
    pub fn close(&mut self, ch: u32) -> Option<u64> {
        self.channels.remove(&ch);
        self.held.remove(&ch).and_then(|a| a.token())
    }

    /// The relay went: every token of a box left up.
    pub fn clear(&mut self) -> Vec<u64> {
        self.channels.clear();
        self.held.drain().filter_map(|(_, a)| a.token()).collect()
    }

    /// Drops the asks [`EXPIRE_SECS`] old by `now`: each channel, with
    /// the token of a box it leaves up.
    pub fn expire(&mut self, now: u64) -> Vec<(u32, Option<u64>)> {
        let old: Vec<u32> = self
            .held
            .iter()
            .filter(|(_, a)| now >= a.opened.saturating_add(EXPIRE_SECS))
            .map(|(ch, _)| *ch)
            .collect();
        old.into_iter()
            .map(|ch| (ch, self.held.remove(&ch).and_then(|a| a.token())))
            .collect()
    }

    /// One plain frame on ask channel `ch`. `me` is asked for this PC's
    /// key pair only when the phone's `ask` comes, so a PC with none
    /// makes one only then.
    pub fn heard(
        &mut self,
        ch: u32,
        frame: &[u8],
        crypto: &dyn Crypto,
        me: impl FnOnce() -> Option<Me>,
    ) -> Step {
        let Some(ask) = self.held.remove(&ch) else {
            return Step::Nothing;
        };
        let token = ask.token();
        let msg = serde_json::from_slice::<Value>(frame).unwrap_or(Value::Null);
        let t = msg["t"].as_str().unwrap_or_default();
        let next = match (ask.stage, t) {
            (Stage::Opened, "ask") => Self::asking(&msg, crypto, me),
            (stage @ Stage::Asking { .. }, "reveal") => {
                let token = self.next;
                self.next += 1;
                Self::revealed(&msg, stage, crypto, &ask.email, token)
            }
            _ => Err(Step::Dropped(token)),
        };
        match next {
            Ok((stage, step)) => {
                self.held.insert(ch, Ask { stage, ..ask });
                step
            }
            Err(step) => step,
        }
    }

    fn asking(
        msg: &Value,
        crypto: &dyn Crypto,
        me: impl FnOnce() -> Option<Me>,
    ) -> Result<(Stage, Step), Step> {
        let commit: [u8; 32] = msg["commit"]
            .as_str()
            .and_then(unb64url)
            .and_then(|c| c.try_into().ok())
            .ok_or(Step::Dropped(None))?;
        if msg["v"].as_u64() != Some(ASK_V) {
            return Err(Step::Dropped(None));
        }
        let name: String = msg["name"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(60)
            .collect();
        let me = me().ok_or(Step::Refuse)?;
        let mut nonce = [0u8; 16];
        crypto.random(&mut nonce);
        let send = json!({
            "t": "asking",
            "v": ASK_V,
            "name": me.name,
            "brother": me.id,
            "pub": b64url(&public_of(&me.private)),
            "nonce": b64url(&nonce),
        });
        let stage = Stage::Asking {
            name: if name.is_empty() { "Phone".into() } else { name },
            commit,
            private: me.private,
            nonce,
        };
        Ok((stage, Step::Send(send)))
    }

    fn revealed(
        msg: &Value,
        stage: Stage,
        crypto: &dyn Crypto,
        email: &str,
        token: u64,
    ) -> Result<(Stage, Step), Step> {
        let Stage::Asking {
            name,
            commit: promised,
            private,
            nonce: pc_nonce,
        } = stage
        else {
            return Err(Step::Dropped(None));
        };
        let public = msg["pub"].as_str().and_then(unb64url).ok_or(Step::Refuse)?;
        let nonce = msg["nonce"].as_str().and_then(unb64url).ok_or(Step::Refuse)?;
        let mine = public_of(&private);
        // ECDH with its own public key agrees too, so a relay that hands
        // the PC's key back to it would otherwise get a matching code.
        if commit(&public, &nonce) != promised || public == mine || nonce.len() != 16 {
            return Err(Step::Refuse);
        }
        let s = crypto.ecdh(&private, &public).ok_or(Step::Refuse)?;
        let (code, key) = agree(&s, (&public, &nonce), (&mine, &pc_nonce));
        let question = Question {
            token,
            text: question_text(&name, email, &code),
            code,
        };
        let stage = Stage::Waiting {
            name,
            public,
            key,
            token,
        };
        Ok((stage, Step::Ask(question)))
    }

    /// The human answered the box with `token` for channel `ch`. None when
    /// that ask is gone or the box is not its own.
    pub fn answer(&mut self, ch: u32, token: u64, yes: bool) -> Option<Answer> {
        if self.held.get(&ch)?.token() != Some(token) {
            return None;
        }
        let Stage::Waiting {
            name, public, key, ..
        } = self.held.remove(&ch)?.stage
        else {
            return None;
        };
        Some(if yes {
            Answer::Yes(LetIn {
                name,
                public: b64url(&public),
                key,
            })
        } else {
            Answer::No
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::joining::fake::Fake;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn the_pinned_vector_gives_the_pages_commit_code_and_key() {
        let s = [0x11u8; 32];
        let mut phone = vec![4u8];
        phone.extend([0x22u8; 64]);
        let phone_nonce = [0x33u8; 16];
        let mut pc = vec![4u8];
        pc.extend([0x44u8; 64]);
        let pc_nonce = [0x55u8; 16];
        assert_eq!(
            hex(&commit(&phone, &phone_nonce)),
            "aee2fd1a606cd5cbf15ee8f00eda999ad2887d7c8b1cd08ce50e0463e887cac3"
        );
        let (code, key) = agree(&s, (&phone, &phone_nonce), (&pc, &pc_nonce));
        assert_eq!(code, "835 086");
        assert_eq!(
            hex(&key),
            "e18619d9e6d4de5e13c7da7a75d3de6013e9ca2f014c2efeec51e0d127c1611f"
        );
    }

    struct Phone {
        private: [u8; 96],
        nonce: [u8; 16],
    }

    impl Phone {
        fn new(f: &Fake) -> Phone {
            let mut nonce = [0u8; 16];
            f.random(&mut nonce);
            Phone {
                private: f.pair().unwrap(),
                nonce,
            }
        }

        fn public(&self) -> Vec<u8> {
            public_of(&self.private)
        }

        fn ask(&self) -> Vec<u8> {
            let c = commit(&self.public(), &self.nonce);
            json!({"t": "ask", "v": 1, "name": "iPhone, Safari", "commit": b64url(&c)})
                .to_string()
                .into_bytes()
        }

        fn reveal(&self) -> Vec<u8> {
            json!({"t": "reveal", "pub": b64url(&self.public()), "nonce": b64url(&self.nonce)})
                .to_string()
                .into_bytes()
        }
    }

    fn me(private: [u8; 96]) -> impl FnOnce() -> Option<Me> {
        move || {
            Some(Me {
                id: "pc1".into(),
                name: "Desktop".into(),
                private,
            })
        }
    }

    fn sent(step: Step) -> Value {
        match step {
            Step::Send(v) => v,
            other => panic!("expected a send, got {other:?}"),
        }
    }

    #[test]
    fn a_phone_is_let_in_on_its_humans_yes_with_the_code_both_see() {
        let f = Fake::new();
        let pc = f.pair().unwrap();
        let phone = Phone::new(&f);
        let mut asks = Asks::default();
        assert_eq!(asks.open(7, "x@y.com", 100), (true, None));
        assert!(asks.is_ask(7) && !asks.is_ask(8));
        let asking = sent(asks.heard(7, &phone.ask(), &f, me(pc)));
        assert_eq!(asking["t"], "asking");
        assert_eq!(asking["v"], 1);
        assert_eq!(asking["name"], "Desktop");
        assert_eq!(asking["brother"], "pc1");
        assert_eq!(asking["pub"], b64url(&public_of(&pc)));
        let pc_nonce = unb64url(asking["nonce"].as_str().unwrap()).unwrap();
        assert_eq!(pc_nonce.len(), 16);
        let Step::Ask(q) = asks.heard(7, &phone.reveal(), &f, || None) else {
            panic!("no question");
        };
        // What the phone works out for itself.
        let s = f.ecdh(&phone.private, &public_of(&pc)).unwrap();
        let (code, key) = agree(
            &s,
            (&phone.public(), &phone.nonce),
            (&public_of(&pc), &pc_nonce),
        );
        assert_eq!(q.code, code);
        assert_eq!(
            q.text,
            format!(
                "iPhone, Safari wants to use Town Portal, signed in as x@y.com. Does the \
                 phone show {code}? Yes lets it see and type in this PC's sessions and every \
                 brother's."
            )
        );
        assert_eq!(asks.answer(7, q.token + 1, true), None);
        assert_eq!(
            asks.answer(7, q.token, true),
            Some(Answer::Yes(LetIn {
                name: "iPhone, Safari".into(),
                public: b64url(&phone.public()),
                key,
            }))
        );
        // Answered once: the ask is gone.
        assert_eq!(asks.answer(7, q.token, true), None);
        assert_eq!(asks.heard(7, &phone.reveal(), &f, || None), Step::Nothing);
    }

    fn to_the_box(asks: &mut Asks, f: &Fake, ch: u32) -> Question {
        let phone = Phone::new(f);
        asks.open(ch, "x@y.com", 100);
        sent(asks.heard(ch, &phone.ask(), f, me(f.pair().unwrap())));
        match asks.heard(ch, &phone.reveal(), f, || None) {
            Step::Ask(q) => q,
            other => panic!("no question: {other:?}"),
        }
    }

    #[test]
    fn no_refuses_and_keeps_nothing() {
        let f = Fake::new();
        let mut asks = Asks::default();
        let q = to_the_box(&mut asks, &f, 3);
        assert_eq!(asks.answer(3, q.token, false), Some(Answer::No));
        assert_eq!(asks.answer(3, q.token, true), None);
    }

    #[test]
    fn a_reveal_that_breaks_its_promise_or_is_the_pcs_own_key_is_refused() {
        let f = Fake::new();
        let pc = f.pair().unwrap();
        let phone = Phone::new(&f);
        let other = Phone::new(&f);
        let mut asks = Asks::default();
        asks.open(1, "x@y.com", 0);
        sent(asks.heard(1, &phone.ask(), &f, me(pc)));
        assert_eq!(asks.heard(1, &other.reveal(), &f, || None), Step::Refuse);
        assert_eq!(asks.heard(1, &phone.reveal(), &f, || None), Step::Nothing);

        // A relay hands the PC its own key, committed to properly.
        let mirror = Phone { private: pc, nonce: [9; 16] };
        asks.open(2, "x@y.com", 0);
        sent(asks.heard(2, &mirror.ask(), &f, me(pc)));
        assert_eq!(asks.heard(2, &mirror.reveal(), &f, || None), Step::Refuse);
    }

    #[test]
    fn frames_out_of_order_drop_the_ask() {
        let f = Fake::new();
        let pc = f.pair().unwrap();
        let phone = Phone::new(&f);
        let mut asks = Asks::default();
        asks.open(1, "x@y.com", 0);
        assert_eq!(
            asks.heard(1, &phone.reveal(), &f, me(pc)),
            Step::Dropped(None)
        );
        assert_eq!(asks.heard(1, &phone.ask(), &f, me(pc)), Step::Nothing);
        // Still an ask channel: its frames are no phone's.
        assert!(asks.is_ask(1));

        asks.open(2, "x@y.com", 0);
        sent(asks.heard(2, &phone.ask(), &f, me(pc)));
        assert_eq!(
            asks.heard(2, &phone.ask(), &f, me(pc)),
            Step::Dropped(None)
        );

        let q = to_the_box(&mut asks, &f, 3);
        assert_eq!(
            asks.heard(3, br#"{"t":"reveal"}"#, &f, || None),
            Step::Dropped(Some(q.token))
        );
        assert_eq!(asks.answer(3, q.token, true), None);

        // The phone's human said no while the box was up.
        let q = to_the_box(&mut asks, &f, 4);
        assert_eq!(
            asks.heard(4, br#"{"t":"refused"}"#, &f, || None),
            Step::Dropped(Some(q.token))
        );
        asks.open(5, "x@y.com", 0);
        assert_eq!(asks.heard(5, b"not json", &f, me(pc)), Step::Dropped(None));
    }

    #[test]
    fn an_ask_of_another_version_or_without_a_commit_is_dropped() {
        let f = Fake::new();
        let pc = f.pair().unwrap();
        let mut asks = Asks::default();
        for (ch, bad) in [
            (1, json!({"t": "ask", "v": 2, "name": "P", "commit": b64url(&[0; 32])})),
            (2, json!({"t": "ask", "name": "P", "commit": b64url(&[0; 32])})),
            (3, json!({"t": "ask", "v": 1, "name": "P", "commit": b64url(&[0; 31])})),
        ] {
            asks.open(ch, "x@y.com", 0);
            let frame = bad.to_string().into_bytes();
            assert_eq!(asks.heard(ch, &frame, &f, me(pc)), Step::Dropped(None));
        }
    }

    #[test]
    fn a_pc_without_a_key_pair_refuses_when_it_cannot_make_one() {
        let f = Fake::new();
        let phone = Phone::new(&f);
        let mut asks = Asks::default();
        asks.open(1, "x@y.com", 0);
        assert_eq!(asks.heard(1, &phone.ask(), &f, || None), Step::Refuse);
    }

    #[test]
    fn at_most_three_asks_are_held_and_each_lives_five_minutes() {
        let f = Fake::new();
        let mut asks = Asks::default();
        assert!(asks.open(1, "a", 0).0);
        assert!(asks.open(2, "a", 10).0);
        let q = to_the_box(&mut asks, &f, 3);
        assert!(!asks.open(4, "a", 20).0);
        assert!(asks.is_ask(4));
        assert_eq!(asks.expire(EXPIRE_SECS - 1), vec![]);
        assert_eq!(asks.expire(EXPIRE_SECS), vec![(1, None)]);
        assert!(asks.open(5, "a", EXPIRE_SECS).0);
        let mut gone = asks.expire(EXPIRE_SECS + 100);
        gone.sort();
        assert_eq!(gone, vec![(2, None), (3, Some(q.token))]);
        assert_eq!(asks.answer(3, q.token, true), None);
    }

    #[test]
    fn a_closed_channel_takes_its_box_with_it() {
        let f = Fake::new();
        let mut asks = Asks::default();
        let q = to_the_box(&mut asks, &f, 9);
        assert_eq!(asks.close(9), Some(q.token));
        assert!(!asks.is_ask(9));
        assert_eq!(asks.answer(9, q.token, true), None);
        let q = to_the_box(&mut asks, &f, 10);
        assert!(asks.open(11, "a", 0).0);
        assert_eq!(asks.clear(), vec![q.token]);
        assert!(!asks.is_ask(11));
    }
}
