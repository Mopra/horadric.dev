//! The updater on a Mac. Built out in its own step; until then it offers
//! nothing. The interface is the one the app calls.

use std::path::PathBuf;

use horadric_core::release::Manifest;

pub struct Updater {
    told: Option<String>,
}

impl Updater {
    pub fn new(told: Option<String>) -> Updater {
        Updater { told }
    }

    /// Called every second by the app: starts a check when one is due.
    pub fn tick(&mut self) {}

    /// Checks now. `asked` when the human asked, who hears the answer
    /// through [`Updater::take_checked`].
    pub fn check(&mut self, _asked: bool) {}

    /// The version on offer, once a check found one.
    pub fn offer(&self) -> Option<String> {
        None
    }

    /// True once for each version newly on offer that the human was not
    /// told of yet, which counts as telling them.
    pub fn take_news(&mut self) -> bool {
        false
    }

    /// The answer to a check the human asked for: whether an update was
    /// found, or why the check failed. Once.
    pub fn take_checked(&mut self) -> Option<Result<bool, String>> {
        None
    }

    pub fn manifest(&self) -> Option<Manifest> {
        None
    }

    /// The last version the human was told of, for state.json.
    pub fn told(&self) -> Option<String> {
        self.told.clone()
    }

    /// Downloads, checks and unpacks the release on offer, on a thread.
    pub fn start_install(&mut self) {}

    /// The new build's `horadric`, once an install is ready to hand over
    /// to, or why it failed. Once.
    pub fn take_ready(&mut self) -> Option<Result<PathBuf, String>> {
        None
    }
}
