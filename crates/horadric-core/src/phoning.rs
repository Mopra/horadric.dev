//! A phone served by every brother (docs/plan/one-portal.md): a phone
//! pairs with one brother, which vouches for its public key in its signed
//! word, and every brother that takes the word serves it with a key of its
//! own, derived from the phone's public key and its own key pair
//! ([`crate::portal::phone_key`]). What a Windows PC and a Linux node
//! share of it lives here: which phones the word serves and with what
//! keys, the `brothers` list a phone learns the others from, and whether
//! a phone paired here was forgotten by word.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde_json::{json, Value};

use crate::brotherhood::{Brother, Roster};
use crate::joining::{public_of, Crypto};
use crate::mesh::Held;
use crate::portal::{b64url, fingerprint, phone_key, unb64url};
use crate::said::What;

/// A phone the word serves, with the key this machine shares with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keyed {
    pub id: String,
    pub name: String,
    /// Its public key, b64url, as the vouch names it.
    pub public: String,
    pub key: [u8; 32],
}

/// The keys derived so far, by the phone's id and public key, so an ECDH
/// is done once a phone rather than at every look at the word.
#[derive(Default)]
pub struct Keys(HashMap<(String, String), Option<[u8; 32]>>);

impl Keys {
    /// The phones `h` serves, each with the key this machine (relay id
    /// `me`, key pair `private`) shares with it. A phone whose key is no
    /// point on the curve is left out.
    pub fn served(
        &mut self,
        h: &Held,
        me: &str,
        private: &[u8; 96],
        crypto: &dyn Crypto,
    ) -> Vec<Keyed> {
        let phones = crate::mesh::phones(h);
        self.0
            .retain(|(id, key), _| phones.iter().any(|p| &p.phone == id && &p.key == key));
        phones
            .into_iter()
            .filter_map(|p| {
                let key = *self
                    .0
                    .entry((p.phone.clone(), p.key.clone()))
                    .or_insert_with(|| derive(private, &p.key, &p.phone, me, crypto));
                Some(Keyed {
                    id: p.phone,
                    name: p.name,
                    public: p.key,
                    key: key?,
                })
            })
            .collect()
    }
}

/// The key this machine shares with the phone `phone` whose public key
/// is `public`, b64url. None when that is not a P-256 point.
pub fn derive(
    private: &[u8; 96],
    public: &str,
    phone: &str,
    me: &str,
    crypto: &dyn Crypto,
) -> Option<[u8; 32]> {
    let theirs = unb64url(public)?;
    let shared = crypto.ecdh(private, &theirs)?;
    Some(phone_key(&shared, &theirs, &public_of(private), phone, me))
}

/// The public keys of phones some signed word forgot, whoever signed it.
/// A phone paired here whose key is among them and that the word no
/// longer serves was forgotten elsewhere, and is dropped here too.
pub fn forgotten(h: &Held) -> BTreeSet<String> {
    h.roster
        .said
        .iter()
        .filter_map(|w| match &w.what {
            What::Forgets { key, .. } => Some(key.clone()),
            _ => None,
        })
        .collect()
}

/// Whether a brother counts as in, for a phone: not cast out, and its key
/// neither out nor contested.
fn is_in(r: &Roster, b: &Brother) -> bool {
    !b.cast_out && !r.outcast.contains(&b.key) && !r.contested.contains(&b.key)
}

/// The relay ids of the members in, in roster order and once each: the
/// machines a v3 phone learns from [`brothers_message`] and dials.
pub fn members(r: &Roster) -> Vec<String> {
    let mut seen = BTreeSet::new();
    r.brothers
        .iter()
        .filter(|b| is_in(r, b) && seen.insert(b.id.clone()))
        .map(|b| b.id.clone())
        .collect()
}

/// The `brothers` message a phone learns every brother from: each
/// member in, in roster order, with its relay id, name, public key,
/// `mark` (its colour as 0xRRGGBB, where this machine draws one) and
/// `state`, how this machine finds it now: "live", "asleep" or "away".
/// This machine itself is "live".
pub fn brothers_message(
    r: &Roster,
    me: &str,
    state: impl Fn(&str) -> &'static str,
    mark: impl Fn(&Brother) -> Option<u32>,
) -> Value {
    let mut seen = BTreeSet::new();
    let list: Vec<Value> = r
        .brothers
        .iter()
        .filter(|b| is_in(r, b) && seen.insert(b.id.clone()))
        .map(|b| {
            let mut v = json!({
                "id": b.id,
                "name": b.name,
                "key": b.key,
                "state": if b.id == me { "live" } else { state(&b.id) },
            });
            if let Some(m) = mark(b) {
                v["mark"] = json!(m);
            }
            v
        })
        .collect();
    json!({"t": "brothers", "list": list})
}

/// The answer to a pairing v3 or an upgrade: the phone's id, this
/// machine's relay id and public key, from which the phone derives the
/// key itself.
pub fn paired_message(t: &str, phone: &str, me: &str, private: &[u8; 96]) -> Value {
    json!({
        "t": t,
        "phone": phone,
        "brother": me,
        "bkey": b64url(&public_of(private)),
    })
}

/// A phone as the desktop's menu and `horadric portal phones` list it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Known {
    pub id: String,
    pub name: String,
    /// Its public key, empty for a phone paired the old way.
    pub public: String,
    /// The brothers that vouched for it, by relay id.
    pub by: Vec<String>,
    /// Paired with this machine, so in its `portal.json`.
    pub here: bool,
    /// When it last said hello here, for one paired here.
    pub seen: Option<u64>,
}

/// Every phone this machine knows: those `paired` with it first, as they
/// paired, then those the word `served` that another brother vouched for.
pub fn known(paired: &[crate::portal::Phone], served: &[crate::said::Phone]) -> Vec<Known> {
    let by = |public: &str| -> Vec<String> {
        served
            .iter()
            .filter(|s| !public.is_empty() && s.key == public)
            .flat_map(|s| s.by.values().cloned())
            .collect()
    };
    let mut out: Vec<Known> = paired
        .iter()
        .map(|p| Known {
            id: p.id.clone(),
            name: p.name.clone(),
            public: p.public.clone(),
            by: by(&p.public),
            here: true,
            seen: Some(p.seen),
        })
        .collect();
    for s in served {
        if out.iter().any(|k| k.id == s.phone && k.public == s.key) {
            continue;
        }
        out.push(Known {
            id: s.phone.clone(),
            name: s.name.clone(),
            public: s.key.clone(),
            by: s.by.values().cloned().collect(),
            here: false,
            seen: None,
        });
    }
    out
}

/// The phone channels of a machine that pairs no phones but serves those
/// the word vouches for: a Linux node. A PC keeps its own, with pairing.
#[derive(Default)]
pub struct Chans {
    /// Each phone channel the relay opened, and once a frame opened with a
    /// served phone's key, which phone and key.
    chans: HashMap<u32, Option<(String, [u8; 32])>>,
    keys: Keys,
    served: Vec<Keyed>,
    /// The channels whose phone said hello, so it hears the brothers.
    greeted: HashSet<u32>,
    /// The `brothers` message each was sent last, by fingerprint.
    told: HashMap<u32, u64>,
}

/// What a phone's frame comes to.
#[derive(Debug, PartialEq)]
pub enum Heard {
    /// Nothing: a channel the relay never opened, or no served key opens
    /// it.
    Nothing,
    /// A message for the server, from phone `id`.
    Asked { id: String, msg: Value },
    /// The phone asked to be forgotten, by the public key it was vouched
    /// for under: the caller signs it, then [`Chans::look`] cuts it off.
    Forget { id: String, public: String },
}

impl Chans {
    pub fn open(&mut self, ch: u32) {
        self.chans.insert(ch, None);
    }

    /// True when `ch` was a phone's.
    pub fn close(&mut self, ch: u32) -> bool {
        self.greeted.remove(&ch);
        self.told.remove(&ch);
        self.chans.remove(&ch).is_some()
    }

    /// Opens a frame on `ch`: with the key of the phone it is known as,
    /// or on a channel not known yet with each served phone's key.
    pub fn frame(&mut self, ch: u32, sealed: &[u8], crypto: &dyn Crypto) -> Heard {
        let Some(who) = self.chans.get_mut(&ch) else {
            return Heard::Nothing;
        };
        let read = |key: &[u8; 32]| -> Option<Value> {
            serde_json::from_slice(&crypto.open_bound(key, sealed, &[])?).ok()
        };
        let (id, msg) = match who {
            Some((id, key)) => match read(key) {
                Some(msg) => (id.clone(), msg),
                None => return Heard::Nothing,
            },
            None => {
                let found = self
                    .served
                    .iter()
                    .find_map(|p| Some((p.id.clone(), p.key, read(&p.key)?)));
                let Some((id, key, msg)) = found else {
                    return Heard::Nothing;
                };
                *who = Some((id.clone(), key));
                (id, msg)
            }
        };
        match msg["t"].as_str() {
            Some("forget") => {
                let public = self
                    .served
                    .iter()
                    .find(|p| p.id == id)
                    .map(|p| p.public.clone())
                    .unwrap_or_default();
                Heard::Forget { id, public }
            }
            // Pairing and upgrading are for the brother a phone paired
            // with; this machine pairs none.
            Some("pair") | Some("upgrade") => Heard::Nothing,
            t => {
                if t == Some("hello") {
                    self.greeted.insert(ch);
                    self.told.remove(&ch);
                }
                Heard::Asked { id, msg }
            }
        }
    }

    /// `msg` sealed for the phone on `ch`, None when no key opened it yet.
    pub fn seal(&self, ch: u32, msg: &Value, crypto: &dyn Crypto) -> Option<Vec<u8>> {
        let (_, key) = self.chans.get(&ch)?.as_ref()?;
        Some(crypto.seal_bound(key, msg.to_string().as_bytes(), &[]))
    }

    /// Takes the phones `h` serves now. Each channel of a phone it no
    /// longer serves is told `unpaired`, sealed, and is nobody from then
    /// on: those are answered, with the channel, for the caller to send
    /// and to tell the server of.
    pub fn look(
        &mut self,
        h: &Held,
        me: &str,
        private: Option<&[u8; 96]>,
        crypto: &dyn Crypto,
    ) -> Vec<(u32, Vec<u8>)> {
        self.served = match private {
            Some(private) => self.keys.served(h, me, private, crypto),
            None => Vec::new(),
        };
        let unpaired = json!({"t": "unpaired"}).to_string();
        let mut cut = Vec::new();
        for (&ch, who) in self.chans.iter_mut() {
            let Some((id, key)) = who else {
                continue;
            };
            if self.served.iter().any(|p| &p.id == id && p.key == *key) {
                continue;
            }
            cut.push((ch, crypto.seal_bound(key, unpaired.as_bytes(), &[])));
            *who = None;
            self.greeted.remove(&ch);
            self.told.remove(&ch);
        }
        cut
    }

    /// `brothers` sealed for each greeted phone that has not heard this
    /// list yet.
    pub fn tell(&mut self, brothers: &Value, crypto: &dyn Crypto) -> Vec<(u32, Vec<u8>)> {
        let print = fingerprint(brothers);
        let plain = brothers.to_string();
        let mut out = Vec::new();
        for &ch in &self.greeted {
            let Some(Some((_, key))) = self.chans.get(&ch) else {
                continue;
            };
            if self.told.insert(ch, print) != Some(print) {
                out.push((ch, crypto.seal_bound(key, plain.as_bytes(), &[])));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brother(id: &str, key: &str) -> Brother {
        serde_json::from_value(json!({
            "id": id, "name": id.to_uppercase(), "key": key, "link": "relay",
        }))
        .unwrap()
    }

    #[test]
    fn the_brothers_list_names_every_member_in_once_and_this_machine_live() {
        let mut r = Roster::default();
        let mut gone = brother("c", "kc");
        gone.cast_out = true;
        r.brothers = vec![
            brother("a", "ka"),
            brother("b", "kb"),
            brother("b", "kb2"),
            gone,
            brother("d", "kd"),
            brother("e", "ke"),
        ];
        r.outcast.insert("kd".into());
        r.contested.insert("ke".into());
        let m = brothers_message(&r, "a", |_| "asleep", |b| (b.id == "b").then_some(0x123456));
        assert_eq!(m["t"], "brothers");
        let list = m["list"].as_array().unwrap();
        let ids: Vec<&str> = list.iter().map(|b| b["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(list[0]["state"], "live");
        assert_eq!(list[1]["state"], "asleep");
        assert_eq!(list[1]["key"], "kb");
        assert_eq!(list[1]["mark"], 0x123456);
        assert!(list[0].get("mark").is_none());
    }

    use crate::joining::fake::Fake;

    #[test]
    fn known_phones_are_those_paired_here_then_those_vouched_elsewhere() {
        let phone = |id: &str, public: &str| crate::portal::Phone {
            id: id.into(),
            name: id.to_uppercase(),
            key: "k".into(),
            public: public.into(),
            was: String::new(),
            paired: 1,
            seen: 2,
        };
        let served = |id: &str, key: &str, by: &[&str]| crate::said::Phone {
            phone: id.into(),
            key: key.into(),
            name: id.to_uppercase(),
            by: by
                .iter()
                .map(|b| (format!("key of {b}"), b.to_string()))
                .collect(),
        };
        let k = known(
            &[phone("old", ""), phone("mine", "P1")],
            &[
                served("mine", "P1", &["me"]),
                served("far", "P2", &["laptop", "vps"]),
            ],
        );
        let ids: Vec<(&str, bool)> = k.iter().map(|k| (k.id.as_str(), k.here)).collect();
        assert_eq!(ids, [("old", true), ("mine", true), ("far", false)]);
        assert!(k[0].by.is_empty());
        assert_eq!(k[1].by, ["me"]);
        assert_eq!(k[2].by, ["laptop", "vps"]);
        assert_eq!((k[1].seen, k[2].seen), (Some(2), None));
    }

    fn chans_serving(id: &str, key: [u8; 32]) -> Chans {
        let mut c = Chans::default();
        c.served.push(Keyed {
            id: id.into(),
            name: "Phone".into(),
            public: "PUB".into(),
            key,
        });
        c
    }

    #[test]
    fn a_phone_channel_is_known_by_the_served_key_that_opens_it() {
        let f = Fake::new();
        let mut c = chans_serving("p", [5; 32]);
        let hello = f.seal_bound(&[5; 32], br#"{"t":"hello","v":3}"#, &[]);
        // A channel the relay never opened is nobody's.
        assert_eq!(c.frame(1, &hello, &f), Heard::Nothing);
        c.open(1);
        let other = f.seal_bound(&[6; 32], br#"{"t":"hello"}"#, &[]);
        assert_eq!(c.frame(1, &other, &f), Heard::Nothing);
        let Heard::Asked { id, msg } = c.frame(1, &hello, &f) else {
            panic!("the served key opens it");
        };
        assert_eq!((id.as_str(), msg["v"].as_u64()), ("p", Some(3)));
        let back = c.seal(1, &json!({"t": "x"}), &f).unwrap();
        assert!(f.open_bound(&[5; 32], &back, &[]).is_some());
        // Greeted, it hears the brothers once a list.
        let list = json!({"t": "brothers", "list": []});
        assert_eq!(c.tell(&list, &f).len(), 1);
        assert!(c.tell(&list, &f).is_empty());
        let forget = f.seal_bound(&[5; 32], br#"{"t":"forget"}"#, &[]);
        assert_eq!(
            c.frame(1, &forget, &f),
            Heard::Forget {
                id: "p".into(),
                public: "PUB".into()
            }
        );
        assert!(c.close(1));
        assert!(!c.close(1));
    }

    #[test]
    fn a_phone_the_word_stops_serving_is_told_unpaired_and_cut_off() {
        let f = Fake::new();
        let mut c = chans_serving("p", [5; 32]);
        c.open(1);
        c.frame(1, &f.seal_bound(&[5; 32], br#"{"t":"hello"}"#, &[]), &f);
        let cut = c.look(&Held::default(), "me", None, &f);
        assert_eq!(cut.len(), 1);
        let said = f.open_bound(&[5; 32], &cut[0].1, &[]).unwrap();
        assert_eq!(said, br#"{"t":"unpaired"}"#);
        assert_eq!(c.seal(1, &json!({}), &f), None);
        assert!(c.tell(&json!({"t": "brothers"}), &f).is_empty());
    }
}
