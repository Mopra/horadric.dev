//! The Vault's sealed file: secrets and web logins in `secrets.dat` beside
//! `accounts.dat`, sealed with DPAPI for this Windows user, and the log of
//! every use beside it. A dev instance has its own state folder, so its
//! own file.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use horadric_core::vault::secret::{winner, Owner};
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Vault {
    #[serde(default)]
    pub secrets: Vec<Secret>,
    #[serde(default)]
    pub logins: Vec<WebLogin>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Secret {
    pub name: String,
    pub owner: Owner,
    pub value: String,
    /// The Always answers, each for one project and one program by its
    /// canonical path and content, as `secret::approval` spells them.
    #[serde(default)]
    pub allowed: Vec<String>,
    #[serde(default)]
    pub last_used: Option<Used>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebLogin {
    pub name: String,
    pub owner: Owner,
    pub origins: Vec<String>,
    pub user: String,
    pub password: String,
    #[serde(default)]
    pub totp: Option<String>,
    /// Project keys allowed Always.
    #[serde(default)]
    pub allowed: Vec<String>,
    #[serde(default)]
    pub last_used: Option<Used>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Used {
    /// Unix seconds.
    pub at: u64,
    pub session: String,
    /// The program, or the site, it went to.
    pub what: String,
}

/// The file as last read or written, or why it could not be read.
/// Unsealing costs a DPAPI call and a question can come every second, so
/// it is read once. A file that is there but will not open is never taken
/// for an empty one: the next change would write over every secret.
static CACHE: Mutex<Option<Result<Vault, String>>> = Mutex::new(None);

/// What the human is told when the file is there but will not open.
pub const UNREADABLE: &str = "The secrets file could not be read, so Horadric changes nothing in it \
     until it can. Nothing in it was lost.";

fn path() -> Option<PathBuf> {
    Some(horadric_hooks::state_dir()?.join("secrets.dat"))
}

/// Mixed into every DPAPI seal of the Vault's files, so the one line of
/// PowerShell that unseals any of this user's DPAPI blobs does not open
/// them. Only a speed bump: anything running as this user can read this
/// constant out of horadric.exe and unseal them all the same.
const ENTROPY: [u8; 32] = [
    0x48, 0x6f, 0x72, 0x61, 0x64, 0x72, 0x69, 0x63, 0x20, 0x56, 0x61, 0x75, 0x6c, 0x74, 0x9e, 0x31,
    0x5c, 0xd2, 0x07, 0xa4, 0x6b, 0xe0, 0x13, 0x88, 0xf5, 0x2a, 0x71, 0xbd, 0x04, 0xc9, 0x66, 0x3f,
];

fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    }
}

/// Takes the bytes DPAPI handed out and frees its copy.
unsafe fn taken(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
    let _ = unsafe { LocalFree(Some(HLOCAL(out.pbData as *mut _))) };
    bytes
}

/// Seals `bytes` for this Windows user, with [`ENTROPY`].
pub(crate) fn seal(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = CRYPT_INTEGER_BLOB::default();
    let entropy = blob(&ENTROPY);
    unsafe {
        CryptProtectData(
            &blob(bytes),
            None,
            Some(&entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
        .ok()?;
        Some(taken(out))
    }
}

/// Unseals what [`seal`] sealed, or a file from before the entropy, which
/// the next write seals again with it.
pub(crate) fn unseal(bytes: &[u8]) -> Option<Vec<u8>> {
    let entropy = blob(&ENTROPY);
    [Some(&entropy as *const CRYPT_INTEGER_BLOB), None]
        .into_iter()
        .find_map(|e| {
            let mut out = CRYPT_INTEGER_BLOB::default();
            unsafe {
                CryptUnprotectData(
                    &blob(bytes),
                    None,
                    e,
                    None,
                    None,
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out,
                )
                .ok()?;
                Some(taken(out))
            }
        })
}

/// A sealed file in the state folder, None when it is not there. Any other
/// failure is an error, never taken for an empty file.
pub(crate) fn read_sealed(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let sealed = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    unseal(&sealed)
        .map(Some)
        .ok_or_else(|| "DPAPI would not unseal it".to_string())
}

/// Writes `bytes` sealed to `path`, through a temporary file so a failed
/// write never leaves half a file.
pub(crate) fn write_sealed(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let sealed = seal(bytes).ok_or("DPAPI would not seal it")?;
    let tmp = path.with_extension("dat.tmp");
    fs::write(&tmp, sealed).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn load() -> Result<Vault, String> {
    let Some(path) = path() else {
        return Ok(Vault::default());
    };
    match read_sealed(&path) {
        Ok(None) => Ok(Vault::default()),
        Ok(Some(bytes)) => serde_json::from_slice(&bytes).map_err(|e| format!("{UNREADABLE} ({e})")),
        Err(e) => Err(format!("{UNREADABLE} ({e})")),
    }
}

fn save(vault: &Vault) -> Result<(), String> {
    let path = path().ok_or("no state folder")?;
    let bytes = serde_json::to_vec(vault).map_err(|e| e.to_string())?;
    write_sealed(&path, &bytes)
}

/// The cache, read now if it never was or could not be: a file that would
/// not open may open now, after a sign in or a restore.
fn loaded(cache: &mut Option<Result<Vault, String>>) -> &Result<Vault, String> {
    if !matches!(cache, Some(Ok(_))) {
        *cache = Some(load());
    }
    cache.get_or_insert_with(load)
}

/// Everything kept, or nothing while the file will not open; [`problem`]
/// says why.
pub fn read() -> Vault {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    match loaded(&mut cache) {
        Ok(v) => v.clone(),
        Err(_) => Vault::default(),
    }
}

/// Why the secrets file will not open, None when it does or is not there
/// yet. For Settings and `secret list` to show.
pub fn problem() -> Option<String> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    loaded(&mut cache).as_ref().err().cloned()
}

/// Changes the kept file and writes it sealed. Nothing changes when the
/// write fails, so what is in memory is what is on disk, and nothing is
/// written while the file on disk will not open.
pub fn change(f: impl FnOnce(&mut Vault)) -> Result<(), String> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let mut vault = loaded(&mut cache).clone()?;
    f(&mut vault);
    save(&vault)?;
    *cache = Some(Ok(vault));
    Ok(())
}

/// The secret a session of `project_key` with `identity` gets by `name`.
pub fn secret(project_key: &str, identity: Option<&str>, name: &str) -> Option<Secret> {
    let v = read();
    winner(
        &v.secrets,
        |s| &s.owner,
        |s| &s.name,
        project_key,
        identity,
        name,
    )
    .cloned()
}

/// The web login a session of `project_key` with `identity` gets by `name`.
pub fn login(project_key: &str, identity: Option<&str>, name: &str) -> Option<WebLogin> {
    let v = read();
    winner(
        &v.logins,
        |l| &l.owner,
        |l| &l.name,
        project_key,
        identity,
        name,
    )
    .cloned()
}

/// Whether a secret or a web login is kept by this name, for a quest
/// blocked `--on-secret`.
pub fn kept(project_key: &str, identity: Option<&str>, name: &str) -> bool {
    secret(project_key, identity, name).is_some() || login(project_key, identity, name).is_some()
}

/// Every value kept, for the memory to cut from transcripts.
pub fn values() -> Vec<String> {
    let v = read();
    v.secrets
        .iter()
        .map(|s| s.value.clone())
        .chain(v.logins.iter().map(|l| l.password.clone()))
        .chain(v.logins.iter().filter_map(|l| l.totp.clone()))
        .filter(|s| !s.is_empty())
        .collect()
}

/// What Settings shows of one kept item: never a value, a password or a
/// seed.
#[derive(Clone, Debug)]
pub struct Listed {
    pub name: String,
    pub owner: Owner,
    pub kind: Kind,
    /// The Always answers: approvals for a secret, project keys for a web
    /// login.
    pub allowed: Vec<String>,
    pub last_used: Option<Used>,
    /// A web login's sites, empty for a secret.
    pub origins: Vec<String>,
    /// A web login's user name, empty for a secret.
    pub user: String,
    pub has_totp: bool,
    /// Another item of this name wins for the project, so this one is not
    /// what its sessions get.
    pub beaten: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Secret,
    WebLogin,
}

/// Every item kept, as a project's sessions see them: secrets first, then
/// web logins.
pub fn listing(project_key: &str, identity: Option<&str>) -> Vec<Listed> {
    let v = read();
    let mut out = Vec::new();
    for s in &v.secrets {
        let wins = winner(&v.secrets, |s| &s.owner, |s| &s.name, project_key, identity, &s.name);
        out.push(Listed {
            name: s.name.clone(),
            owner: s.owner.clone(),
            kind: Kind::Secret,
            allowed: s.allowed.clone(),
            last_used: s.last_used.clone(),
            origins: Vec::new(),
            user: String::new(),
            has_totp: false,
            beaten: wins.is_some_and(|w| w.owner != s.owner),
        });
    }
    for l in &v.logins {
        let wins = winner(&v.logins, |l| &l.owner, |l| &l.name, project_key, identity, &l.name);
        out.push(Listed {
            name: l.name.clone(),
            owner: l.owner.clone(),
            kind: Kind::WebLogin,
            allowed: l.allowed.clone(),
            last_used: l.last_used.clone(),
            origins: l.origins.clone(),
            user: l.user.clone(),
            has_totp: l.totp.as_deref().is_some_and(|t| !t.is_empty()),
            beaten: wins.is_some_and(|w| w.owner != l.owner),
        });
    }
    out
}

/// Adds a line to `secrets.log`. A log that cannot be written stops
/// nothing: the use itself was decided already.
pub fn log(line: &str) {
    let Some(path) = horadric_hooks::state_dir().map(|d| d.join("secrets.log")) else {
        return;
    };
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// The names kept for a project's sessions, each with the owner that wins
/// it: secrets first, then web logins.
pub fn names(project_key: &str, identity: Option<&str>) -> (Vec<(String, Owner)>, Vec<(String, Owner)>) {
    let v = read();
    let mut secrets: Vec<(String, Owner)> = Vec::new();
    for s in &v.secrets {
        if secrets.iter().any(|(n, _)| *n == s.name) {
            continue;
        }
        if let Some(w) = winner(&v.secrets, |s| &s.owner, |s| &s.name, project_key, identity, &s.name) {
            secrets.push((w.name.clone(), w.owner.clone()));
        }
    }
    let mut logins: Vec<(String, Owner)> = Vec::new();
    for l in &v.logins {
        if logins.iter().any(|(n, _)| *n == l.name) {
            continue;
        }
        if let Some(w) = winner(&v.logins, |l| &l.owner, |l| &l.name, project_key, identity, &l.name) {
            logins.push((w.name.clone(), w.owner.clone()));
        }
    }
    (secrets, logins)
}
