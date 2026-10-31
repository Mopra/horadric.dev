//! Signing a machine in to the brotherhood with WorkOS's device code flow,
//! for a machine with no sign in of its own: the Mac. What
//! `crates/horadric/src/node_signin.rs` does for a Linux node, with the
//! state folder handed in and the link and code given to the caller to
//! show, not printed. The refresh token goes in `workos.json` in the state
//! folder, readable by its owner alone. The pure half is
//! `horadric_core::workos`.
//!
//! Refresh tokens rotate on every use, so a refresh holds a lock in this
//! process and a file lock across processes: two refreshing with one token
//! would sign the second out. The token in hand has a lock of its own,
//! never held through a refresh, so whoever only reads it or drops it
//! never waits on WorkOS.

use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::sync::{Mutex, TryLockError};
use std::time::{Duration, Instant};

use horadric_core::portal::write_private;
use horadric_core::workos::{self, Polled, User};

use crate::http;
use crate::relay::{now, SignIn};

/// The file the refresh token is kept in, in the state folder.
const FILE: &str = "workos.json";

/// Taken around every read and write of [`FILE`] that may rotate it.
const LOCK: &str = "workos.lock";

/// What the human is shown to confirm a sign in elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub link: String,
    pub code: String,
}

/// One machine's WorkOS sign in, kept in `dir`.
pub struct Workos {
    dir: PathBuf,
    /// A dev instance signs in with WorkOS's staging environment, so it
    /// groups with a Windows dev instance signed in there.
    dev: bool,
    /// The access token in hand and when it runs out.
    access: Mutex<Option<(String, u64)>>,
    /// Held through a refresh, so this process refreshes once at a time.
    refreshing: Mutex<()>,
}

/// An exclusive lock on [`LOCK`], let go when dropped.
struct Locked(File);

impl Drop for Locked {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl Workos {
    pub fn new(dir: PathBuf, dev: bool) -> Workos {
        Workos {
            dir,
            dev,
            access: Mutex::new(None),
            refreshing: Mutex::new(()),
        }
    }

    fn client(&self) -> &'static str {
        workos::client_id(self.dev)
    }

    fn lock(&self) -> Result<Locked, String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(LOCK))
            .map_err(|e| format!("cannot lock the sign in: {e}"))?;
        f.lock()
            .map_err(|e| format!("cannot lock the sign in: {e}"))?;
        Ok(Locked(f))
    }

    /// The refresh token and who it is, as kept.
    fn read(&self) -> Option<(String, User)> {
        let text = std::fs::read_to_string(self.dir.join(FILE)).ok()?;
        workos::read_kept_file(&text, self.client())
    }

    fn keep(&self, refresh: &str, user: &User) -> Result<(), String> {
        let path = self.dir.join(FILE);
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        write_private(
            &path,
            workos::kept_file(refresh, user, self.client()).as_bytes(),
        )
        .map_err(|e| format!("cannot keep the sign in in {}: {e}", path.display()))
    }

    /// Who is signed in here, without asking WorkOS.
    pub fn user(&self) -> Option<User> {
        self.read().map(|(_, u)| u)
    }

    /// Signs in with the device code flow. `show` gets the link to open
    /// and the code the human should see there, once; then this blocks
    /// until they confirm, refuse, or the code runs out. Run it on a
    /// thread of its own.
    pub fn device(&self, show: impl FnOnce(Shown)) -> Result<User, String> {
        let answer = http::post_form(
            workos::DEVICE_URL,
            &workos::device_body(self.client()),
            1 << 16,
        )
        .map_err(|e| format!("cannot reach WorkOS: {e}"))?;
        let device = workos::parse_device(&answer.body)
            .map_err(|why| format!("WorkOS gave no code ({}): {why}", answer.status))?;
        show(Shown {
            link: device.link().to_string(),
            code: device.user_code.clone(),
        });
        let started = Instant::now();
        let mut interval = device.interval;
        loop {
            std::thread::sleep(Duration::from_secs(interval));
            let left = device
                .expires_in
                .saturating_sub(started.elapsed().as_secs());
            let body = workos::poll_body(self.client(), &device.device_code);
            let poll = match http::post_form(workos::AUTHENTICATE_URL, &body, 1 << 20) {
                Ok(a) => workos::parse_poll(&a.body),
                // The network came and went: the code is still good, so
                // poll on until it runs out.
                Err(_) if left > interval => continue,
                Err(e) => return Err(format!("cannot reach WorkOS: {e}")),
            };
            match workos::after_poll(poll, interval, left) {
                Polled::Wait(next) => interval = next,
                Polled::Stop(why) => return Err(why),
                Polled::Signed(auth) => {
                    let user = auth.user.ok_or("WorkOS did not say who signed in")?;
                    {
                        let _locked = self.lock()?;
                        self.keep(&auth.refresh, &user)?;
                    }
                    self.hold(auth.access);
                    return Ok(user);
                }
            }
        }
    }

    /// Forgets the account here. The WorkOS session itself ends when its
    /// refresh token is never used again.
    pub fn sign_out(&self) -> Result<bool, String> {
        let _locked = self.lock()?;
        self.forget();
        let path = self.dir.join(FILE);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// The access token in hand, if it may be handed out now: see
    /// [`workos::usable`].
    fn in_hand(&self, refreshing: bool) -> Option<String> {
        let held = self.access.lock().unwrap_or_else(|e| e.into_inner());
        held.as_ref()
            .filter(|(_, exp)| workos::usable(*exp, now(), refreshing))
            .map(|(token, _)| token.clone())
    }

    /// A good access token, refreshed and the rotated refresh token kept
    /// when the one in hand is about to run out. Blocks while it
    /// refreshes, unless another caller is refreshing already and the one
    /// in hand has not run out yet.
    pub fn token(&self) -> Result<String, String> {
        if let Some(token) = self.in_hand(false) {
            return Ok(token);
        }
        let _one = match self.refreshing.try_lock() {
            Ok(one) => one,
            Err(TryLockError::Poisoned(e)) => e.into_inner(),
            Err(TryLockError::WouldBlock) => {
                if let Some(token) = self.in_hand(true) {
                    return Ok(token);
                }
                self.refreshing.lock().unwrap_or_else(|e| e.into_inner())
            }
        };
        // Whoever held it before may have just refreshed.
        if let Some(token) = self.in_hand(false) {
            return Ok(token);
        }
        let _locked = self.lock()?;
        // Read under the file lock: another process may have rotated it.
        let (refresh, user) = self.read().ok_or("not signed in")?;
        let body = workos::refresh_body(self.client(), &refresh);
        let answer = http::post_form(workos::AUTHENTICATE_URL, &body, 1 << 20)
            .map_err(|e| format!("cannot reach WorkOS: {e}"))?;
        let auth = workos::parse_auth(&answer.body).map_err(|why| {
            format!(
                "WorkOS refused to renew the sign in ({}): {why}; sign in again",
                answer.status
            )
        })?;
        self.keep(&auth.refresh, auth.user.as_ref().unwrap_or(&user))?;
        self.hold(auth.access.clone());
        Ok(auth.access)
    }

    fn forget(&self) {
        *self.access.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn hold(&self, access: String) {
        let exp = workos::expiry(&access).unwrap_or(0);
        *self.access.lock().unwrap_or_else(|e| e.into_inner()) = Some((access, exp));
    }
}

impl SignIn for Workos {
    fn signed_in(&self) -> bool {
        self.user().is_some()
    }

    fn access_token(&self) -> Result<String, String> {
        self.token()
    }

    fn forget_access(&self) {
        self.forget();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("horadric-link-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_kept_sign_in_is_read_back_and_signing_out_forgets_it() {
        let d = dir("signin");
        let w = Workos::new(d.clone(), true);
        assert!(!w.signed_in());
        let user = User {
            id: "user_01".into(),
            email: "a@b.c".into(),
        };
        w.keep("refresh", &user).unwrap();
        assert_eq!(w.user(), Some(user));
        assert!(w.signed_in());
        assert_eq!(w.sign_out(), Ok(true));
        assert!(!w.signed_in());
        assert_eq!(w.sign_out(), Ok(false));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_sign_in_kept_for_the_other_environment_is_not_this_ones() {
        let d = dir("signin-env");
        let staging = Workos::new(d.clone(), true);
        let user = User {
            id: "user_01".into(),
            email: String::new(),
        };
        staging.keep("refresh", &user).unwrap();
        let live = Workos::new(d.clone(), false);
        assert_eq!(live.user(), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_lock_can_be_taken_again_once_let_go() {
        let d = dir("signin-lock");
        let w = Workos::new(d.clone(), true);
        drop(w.lock().unwrap());
        drop(w.lock().unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }
}
