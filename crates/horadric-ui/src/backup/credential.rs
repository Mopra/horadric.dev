//! Secrets in Windows Credential Manager, so backups run unattended: the
//! passphrase here, and Google's refresh token beside it. Each name
//! carries the state folder's, so a dev instance keeps its own.

use windows::core::PCWSTR;
use windows::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
};

/// The passphrase's name.
pub const PASSPHRASE: &str = "backup passphrase";

/// What a secret is stored under: `Horadric-dev backup passphrase`. No
/// colon, which `cmdkey /delete` cannot take.
pub fn target(name: &str) -> String {
    format!("{} {name}", horadric_hooks::state_name())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// The secret stored under `name`, `None` when there is none.
pub fn read(name: &str) -> Option<String> {
    let target = wide(&target(name));
    let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
    unsafe {
        CredReadW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None, &mut cred).ok()?;
        let c = &*cred;
        let blob = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
        let text = String::from_utf8(blob.to_vec()).ok();
        CredFree(cred as *const _);
        text
    }
}

/// Stores `secret` under `name`, replacing what was there. Kept for this
/// user on this machine only, never roamed.
pub fn write(name: &str, secret: &str) -> Result<(), String> {
    crate::credential::write(&target(name), "Horadric", secret.as_bytes())
}

/// Forgets the secret under `name`; forgetting one that is not there is
/// not an error.
pub fn delete(name: &str) {
    let target = wide(&target(name));
    unsafe {
        let _ = CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_reads_and_deletes() {
        let name = format!("test secret {}", std::process::id());
        assert_eq!(read(&name), None);
        write(&name, "correct horse").unwrap();
        write(&name, "battery staple").unwrap();
        assert_eq!(read(&name).as_deref(), Some("battery staple"));
        delete(&name);
        assert_eq!(read(&name), None);
        delete(&name);
    }
}
