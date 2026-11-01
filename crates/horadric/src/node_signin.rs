//! Signing a node in to the brotherhood (see docs/plan/the-brotherhood.md,
//! "Linux nodes on the relay"). A server has no browser, so it takes
//! WorkOS's device code flow: it prints a short code and a link, the human
//! confirms it in any browser, and the node polls until they have. The
//! flow, the refresh token kept owner only in `workos.json` and the locks
//! around its rotation are `horadric_link::signin`, which the Mac signs in
//! with too; this prints what it shows.

use std::sync::{Arc, OnceLock};

use horadric_core::workos::User;
use horadric_link::signin::Workos;

static WORKOS: OnceLock<Arc<Workos>> = OnceLock::new();

/// This process's sign in, kept in the state folder. The running node and
/// a terminal's `horadric node signin` each have one, and the file lock in
/// `Workos` keeps them from rotating the refresh token at once.
pub(super) fn workos() -> Result<Arc<Workos>, String> {
    if let Some(w) = WORKOS.get() {
        return Ok(Arc::clone(w));
    }
    let dir = horadric_hooks::state_dir().ok_or("cannot find the state folder")?;
    Ok(Arc::clone(WORKOS.get_or_init(|| {
        Arc::new(Workos::new(dir, horadric_hooks::dev()))
    })))
}

/// Who is signed in here, without asking WorkOS.
pub(super) fn user() -> Option<User> {
    workos().ok()?.user()
}

/// Signs in with the device code flow, telling the human in the terminal
/// what to open and which code to see there. Blocks until they confirm,
/// refuse, or the code runs out.
pub(super) fn device() -> Result<User, String> {
    workos()?.device(|shown| {
        println!();
        println!("To sign this node in, open this link in any browser:");
        println!();
        println!("  {}", shown.link);
        println!();
        println!("and check that it shows the code  {}", shown.code);
        if horadric_hooks::dev() {
            println!("(a dev node: WorkOS's staging environment)");
        }
        println!();
        println!("Waiting for you to confirm it there...");
    })
}

/// Forgets the account here. The WorkOS session itself ends when its
/// refresh token is never used again.
pub(super) fn sign_out() -> Result<bool, String> {
    workos()?.sign_out()
}
