//! The Vault's sealed file: secrets and web logins in `secrets.dat` beside
//! `accounts.dat`, sealed with DPAPI for this Windows user, and the log of
//! every use beside it. A dev instance has its own state folder, so its
//! own file.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use horadric_core::vault::secret::{winner, Owner};
use serde::{Deserialize, Serialize};

use crate::accounts::{seal, unseal};

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
    /// Full program paths allowed Always, lower case.
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

/// The file as last read or written. Unsealing costs a DPAPI call and a
/// question can come every second, so it is read once.
static CACHE: Mutex<Option<Vault>> = Mutex::new(None);

fn path() -> Option<PathBuf> {
    Some(horadric_hooks::state_dir()?.join("secrets.dat"))
}

fn load() -> Vault {
    path()
        .and_then(|p| fs::read(p).ok())
        .and_then(|sealed| unseal(&sealed))
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save(vault: &Vault) -> Result<(), String> {
    let path = path().ok_or("no state folder")?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec(vault).map_err(|e| e.to_string())?;
    let sealed = seal(&bytes).ok_or("DPAPI would not seal the secrets")?;
    let tmp = path.with_extension("dat.tmp");
    fs::write(&tmp, sealed).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// Everything kept.
pub fn read() -> Vault {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.get_or_insert_with(load).clone()
}

/// Changes the kept file and writes it sealed. Nothing changes when the
/// write fails, so what is in memory is what is on disk.
pub fn change(f: impl FnOnce(&mut Vault)) -> Result<(), String> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let mut vault = cache.get_or_insert_with(load).clone();
    f(&mut vault);
    save(&vault)?;
    *cache = Some(vault);
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
