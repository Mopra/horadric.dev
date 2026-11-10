//! Secrets in Windows Credential Manager, as generic credentials: a
//! name, a user name shown beside it, and a few hundred bytes that only
//! this Windows account can read. The Google refresh token lives here,
//! and every other secret Horadric keeps goes through [`write`].

use windows::core::{HRESULT, HSTRING, PWSTR};
use windows::Win32::Foundation::{ERROR_NOT_ENOUGH_MEMORY, FILETIME};
use windows::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_FLAGS,
    CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
};

/// The user name and secret stored under `target`, if any.
pub fn read(target: &str) -> Option<(String, Vec<u8>)> {
    unsafe {
        let mut found: *mut CREDENTIALW = std::ptr::null_mut();
        CredReadW(&HSTRING::from(target), CRED_TYPE_GENERIC, None, &mut found).ok()?;
        let c = &*found;
        let user = if c.UserName.is_null() {
            String::new()
        } else {
            c.UserName.to_string().unwrap_or_default()
        };
        let secret = if c.CredentialBlob.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec()
        };
        CredFree(found as *const _);
        Some((user, secret))
    }
}

/// Writes or replaces `target`. Kept on this machine only, never roamed
/// with a domain profile.
pub fn write(target: &str, user: &str, secret: &[u8]) -> Result<(), String> {
    // Checked here because Windows reports an oversized secret as bad RPC
    // stub data, which tells the human nothing.
    if secret.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
        return Err(too_large(secret.len()));
    }
    let mut target_w: Vec<u16> = target.encode_utf16().chain([0]).collect();
    let mut user_w: Vec<u16> = user.encode_utf16().chain([0]).collect();
    let mut blob = secret.to_vec();
    let credential = CREDENTIALW {
        Flags: CRED_FLAGS(0),
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target_w.as_mut_ptr()),
        Comment: PWSTR::null(),
        LastWritten: FILETIME::default(),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        AttributeCount: 0,
        Attributes: std::ptr::null_mut(),
        TargetAlias: PWSTR::null(),
        UserName: PWSTR(user_w.as_mut_ptr()),
    };
    unsafe { CredWriteW(&credential, 0) }
        .map_err(|e| write_failure(e.code(), &e.message()))
}

fn too_large(len: usize) -> String {
    format!(
        "the secret is {len} bytes, more than the {CRED_MAX_CREDENTIAL_BLOB_SIZE}          Windows Credential Manager holds"
    )
}

/// Why a write failed, in words that say what to do. A full Credential
/// Manager comes back as out of memory, which reads like Horadric's fault.
/// It can still be a real shortage, hence "most likely".
fn write_failure(code: HRESULT, windows_says: &str) -> String {
    if code == ERROR_NOT_ENOUGH_MEMORY.to_hresult() {
        return "Windows Credential Manager is most likely full, so it refuses new entries.                 This Windows account has too many saved sign ins, often Xbox tokens whose                 names start with XblGrts. Remove old ones in Control Panel, Credential                 Manager, Windows Credentials, then try again."
            .into();
    }
    format!(
        "cannot write to Windows Credential Manager: {windows_says} (0x{:08X})",
        code.0 as u32
    )
}

/// Forgets `target`. Already gone is fine.
pub fn delete(target: &str) {
    unsafe {
        let _ = CredDeleteW(&HSTRING::from(target), CRED_TYPE_GENERIC, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::ERROR_ACCESS_DENIED;

    #[test]
    fn out_of_memory_is_named_as_a_full_credential_manager() {
        let why = write_failure(ERROR_NOT_ENOUGH_MEMORY.to_hresult(), "Not enough memory");
        assert!(why.starts_with("Windows Credential Manager is most likely full"));
    }

    #[test]
    fn anything_else_keeps_what_windows_said() {
        assert_eq!(
            write_failure(ERROR_ACCESS_DENIED.to_hresult(), "Access is denied."),
            "cannot write to Windows Credential Manager: Access is denied. (0x80070005)"
        );
    }

    #[test]
    fn a_secret_over_the_cap_is_refused_before_windows_sees_it() {
        let big = vec![0u8; CRED_MAX_CREDENTIAL_BLOB_SIZE as usize + 1];
        let why = write("Horadric test oversized secret", "", &big).unwrap_err();
        assert!(why.contains("2561 bytes"));
    }
}
