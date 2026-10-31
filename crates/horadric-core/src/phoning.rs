//! A phone served by every brother (docs/plan/one-portal.md): a phone
//! pairs with one brother, which vouches for its public key in its signed
//! word, and every brother that takes the word serves it with a key of its
//! own, derived from the phone's public key and its own key pair
//! ([`crate::portal::phone_key`]). What a Windows PC and a Linux node
//! share of it lives here: which phones the word serves and with what
//! keys, the `brothers` list a phone learns the others from, and whether
//! a phone paired here was forgotten by word.

use std::collections::{BTreeSet, HashMap};

use serde_json::{json, Value};

use crate::brotherhood::{Brother, Roster};
use crate::joining::{public_of, Crypto};
use crate::mesh::Held;
use crate::portal::{b64url, phone_key, unb64url};
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
}
