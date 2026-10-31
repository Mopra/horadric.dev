//! Pairing a phone with Town Portal, the one way both `horadric portal`
//! and the app's own button do it: turn the portal on, make a one-time
//! code and show it as a QR code in the browser. The protocol is in
//! `docs/PORTAL.md`.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use horadric_core::portal::Config;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::portal_crypto::random;

/// Turns the portal on in `dir`, making its config the first time, opens
/// a new pairing code and shows its QR page. Gives the config as saved,
/// whose `link` is the code as a link.
pub fn pair_a_phone(dir: &Path) -> Result<Config, String> {
    let mut c = Config::load(dir).unwrap_or_else(|| Config::fresh(&random32()));
    c.enabled = true;
    c.open_ticket(&random32(), now());
    c.save(dir)
        .map_err(|e| format!("cannot write {}: {e}", dir.join("portal.json").display()))?;
    if let Some(url) = c.pair_url(&pc_name()) {
        open(&url);
    }
    Ok(c)
}

pub fn random32() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    random(&mut bytes);
    bytes
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The name a phone or a brother shows for this PC. `HORADRIC_PC_NAME`
/// overrides it, so two dev instances on one machine can be told apart.
pub fn pc_name() -> String {
    std::env::var("HORADRIC_PC_NAME")
        .ok()
        .filter(|n| !n.trim().is_empty())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| "PC".into())
}

fn open(url: &str) {
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(url),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// Every phone this PC knows, paired here or vouched for by a brother,
/// as `horadric portal phones` and the Town Portal menu list them.
pub fn known_phones(dir: &Path) -> Vec<horadric_core::phoning::Known> {
    let paired = Config::load(dir).map(|c| c.phones).unwrap_or_default();
    let served = horadric_core::mesh::phones(&horadric_core::mesh::load_held(dir));
    horadric_core::phoning::known(&paired, &served)
}

/// Who vouched for a phone, by name, "this PC" for this one.
pub fn vouched_by(dir: &Path, k: &horadric_core::phoning::Known) -> String {
    let me = Config::load(dir).map(|c| horadric_core::portal::id(&c.secret));
    let roster = horadric_core::mesh::load(dir);
    let names: Vec<String> = k
        .by
        .iter()
        .map(|id| match me.as_deref() == Some(id.as_str()) {
            true => "this PC".to_string(),
            false => roster.name(id).to_string(),
        })
        .collect();
    names.join(", ")
}

/// Forgets phone `id` on every brother: one with a key pair by a signed
/// forget, which each brother that takes it obeys; one paired here the
/// old way, here alone, which is everywhere it was served. The running
/// app cuts it off within seconds. Answers the phone's name.
pub fn forget_everywhere(dir: &Path, id: &str) -> Result<String, String> {
    let known = known_phones(dir);
    let k = known
        .iter()
        .find(|k| k.id == id)
        .ok_or("That phone is not known here.")?;
    if !k.public.is_empty() {
        let me = Config::load(dir)
            .map(|c| horadric_core::portal::id(&c.secret))
            .unwrap_or_default();
        let signer = horadric_core::said::Signer::of(dir, &me, &crate::brothers::Cng);
        let signed = horadric_core::mesh::forget(dir, signer.as_ref(), &k.id, &k.public, now());
        if !signed && !k.here {
            return Err("This PC has no key pair to sign the forget with.".into());
        }
    }
    if let Some(mut c) = Config::load(dir) {
        let before = c.phones.len();
        c.phones.retain(|p| p.id != id);
        if c.phones.len() != before {
            c.save(dir)
                .map_err(|e| format!("cannot write {}: {e}", dir.join("portal.json").display()))?;
        }
    }
    Ok(k.name.clone())
}

/// "Phones can type here": whether a phone may change anything on this
/// PC, or only watch it.
pub fn phones_type(dir: &Path) -> bool {
    !horadric_core::standing::Standing::load(dir).phones_watch_only
}

/// Switches "Phones can type here". It takes hold at a phone's next hello.
pub fn set_phones_type(dir: &Path, on: bool) -> Result<(), String> {
    let mut s = horadric_core::standing::Standing::load(dir);
    s.phones_watch_only = !on;
    s.save(dir).map_err(|e| format!("cannot keep the switch: {e}"))
}
