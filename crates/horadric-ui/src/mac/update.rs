//! The updater on a Mac. Built out in its own step; until then it offers
//! nothing.

use std::path::PathBuf;

use horadric_core::release::Manifest;

pub struct Updater {
    told: Option<String>,
}

impl Updater {
    pub fn new(told: Option<String>) -> Updater {
        Updater { told }
    }

    pub fn offer(&self) -> Option<String> {
        None
    }

    pub fn check(&mut self, _asked: bool) {}

    pub fn take_news(&mut self) -> bool {
        false
    }

    pub fn manifest(&self) -> Option<Manifest> {
        None
    }

    pub fn told(&self) -> Option<String> {
        self.told.clone()
    }
}

pub fn check_soon() {}

pub fn install(_m: &Manifest) -> Result<PathBuf, String> {
    Err("not yet".into())
}
