//! This machine's word on whose its sessions are, as every kind of
//! brother gives it: what a handoff, a landing, an ending and a takeover
//! sign and ask of the roster (`crate::mesh`, by the rules of
//! docs/plan/brotherhood-ownership.md), with the roster's folder, this
//! machine's relay id and the crypto it signs with bound once. A Windows
//! PC, a Linux node and the Mac differ only in that crypto.

use std::path::{Path, PathBuf};

use crate::brotherhood::Owner;
use crate::handoff::Record;
use crate::joining::Crypto;
use crate::keeping::Holding;
use crate::mesh::{self, Heard, Trust, Untakeable};
use crate::said::{Signer, Word};

/// What this machine judges the brotherhood's word by and signs its own
/// with.
pub struct Sworn<'a> {
    dir: PathBuf,
    me: String,
    crypto: &'a dyn Crypto,
}

impl<'a> Sworn<'a> {
    /// The word of the machine with relay id `me`, its roster in `dir`.
    /// None before it has a relay id.
    pub fn new(dir: PathBuf, me: String, crypto: &'a dyn Crypto) -> Option<Self> {
        (!me.is_empty()).then_some(Sworn { dir, me, crypto })
    }

    pub fn me(&self) -> &str {
        &self.me
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Runs `f` with the trust held now and the signer (none before this
    /// machine has a key pair).
    fn with<R>(&self, f: impl FnOnce(&Path, Option<&Signer>, &Trust) -> R) -> R {
        let t = Trust::load(&self.dir, &self.me, self.crypto);
        let signer = Signer::of(&self.dir, &self.me, self.crypto);
        f(&self.dir, signer.as_ref(), &t)
    }

    /// The roster number session `id` is handed off under, so the receiver
    /// owns it one higher. One never claimed is claimed now, signed, so
    /// the handoff has an owner to build on.
    pub fn number(&self, id: &str, now: u64) -> u64 {
        self.with(|d, s, t| mesh::own(d, s, t, id, now))
    }

    /// This machine's signed word that it hands session `id`, numbered
    /// `n`, to brother `to` (its relay id) under `n` + 1, with the words
    /// before it: what the bundle carries, so the receiver owns it on its
    /// sender's word.
    pub fn handing(&self, id: &str, to: &str, n: u64, now: u64) -> Vec<Word> {
        let n = n.saturating_add(1);
        self.with(|d, s, _| s.map(|s| mesh::handing_word(d, s, id, to, n, now)))
            .unwrap_or_default()
    }

    /// A handoff of session `id` failed: that very signed word is taken
    /// back, so a receiver that lands it late does not run it, and the
    /// next try counts.
    pub fn recall(&self, id: &str, now: u64) {
        self.with(|d, s, t| s.map(|s| mesh::recall(d, s, t, id, now)));
    }

    /// Whether this machine may run session `id`, handed here under `n`
    /// with the bundle's `words`: not when its sender recalled the
    /// handoff, or another brother already runs it under that number.
    pub fn may_arrive(&self, id: &str, words: &[Word], n: u64) -> Result<(), String> {
        self.with(|d, _, t| mesh::may_arrive(d, t, words, id, n))
    }

    /// Session `id`, handed here on its sender's `words`, runs here under
    /// `n`. False when the sender's word was not believed.
    pub fn arrived(&self, id: &str, n: u64, words: &[Word], now: u64) -> bool {
        self.with(|d, s, t| mesh::arrived(d, s, words, t, id, n, now))
    }

    /// The handoff of session `id` to brother `to` landed under `n`: the
    /// word the bundle carried is taken in, so the copy left behind is
    /// fenced and every brother hears it from either end.
    pub fn hand(&self, id: &str, to: &str, n: u64) -> bool {
        self.with(|d, _, t| mesh::hand(d, t, id, to, n))
    }

    /// Session `id` went for good here: where the roster has it as this
    /// machine's own, it is signed ended.
    pub fn end(&self, id: &str, now: u64) -> bool {
        self.with(|d, s, t| mesh::end(d, s, t, id, now))
    }

    /// Whether this machine may take over brother `from`'s session
    /// `session` as kept in `holding`, the words the snapshot came with
    /// taken in first, while no brother linked now (`listed`) lists it.
    pub fn takeable(
        &self,
        session: &str,
        from: &str,
        holding: &Holding,
        listed: &[String],
    ) -> Result<u64, Untakeable> {
        let r = &holding.record;
        self.with(|d, _, t| mesh::takeable_in(d, t, session, from, r.n, &r.said, listed))
    }

    /// Signs the takeover of brother `from`'s session `id` from the
    /// snapshot `record`, which started here: the words to tell every
    /// brother. None before this machine has a key pair.
    pub fn take(
        &self,
        id: &str,
        from: &str,
        record: &Record,
        listed: &[String],
        now: u64,
    ) -> Option<Result<(Owner, Vec<Word>), Untakeable>> {
        self.with(|d, s, t| {
            let s = s?;
            Some(mesh::take(
                d,
                s,
                t,
                id,
                from,
                record.n,
                &record.said,
                listed,
                now,
            ))
        })
    }

    /// Takes in the signed word a brother told with `takeover`.
    pub fn told(&self, words: &[Word]) -> Heard {
        self.with(|d, _, t| mesh::told(d, words, t))
    }

    /// Every word on the sessions this machine owns, to tell a brother.
    pub fn mine(&self) -> Vec<Word> {
        mesh::mine(&mesh::load_held(&self.dir), &self.me)
    }

    /// The words on session `id`, this machine's own, for a snapshot: its
    /// owner's word that it owns it, so a keeper can take over even before
    /// a roster came.
    pub fn chain(&self, id: &str) -> Vec<Word> {
        mesh::chain(&mesh::load_held(&self.dir), id, &self.me)
    }
}
