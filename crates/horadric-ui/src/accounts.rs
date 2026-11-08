//! Claude Code's login on disk, and the logins Horadric keeps aside for
//! switching, see [`horadric_core::accounts`].
//!
//! The kept logins are in `accounts.dat` beside the state file, sealed with
//! DPAPI so they read back only for this Windows user. Claude Code's own
//! files are plain text, so this is no weaker than what it copies, and a
//! copy of the folder elsewhere reads as nothing.

use std::fs;
use std::io;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use horadric_core::accounts::{self, Account, Accounts};
use horadric_core::trust;
use serde_json::Value;
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

use crate::{console, store};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// When the login files last changed, so the app reads them only then.
pub type Stamp = Vec<SystemTime>;

fn config_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// `.credentials.json`, in `CLAUDE_CONFIG_DIR` or `~/.claude`.
fn credentials_path() -> Option<PathBuf> {
    let dir = config_dir().or_else(|| Some(home()?.join(".claude")))?;
    Some(dir.join(".credentials.json"))
}

/// `.claude.json`, in `CLAUDE_CONFIG_DIR` or the home folder.
fn config_path() -> Option<PathBuf> {
    Some(config_dir().or_else(home)?.join(".claude.json"))
}

/// The `.claude.json` a session in `dir` reads: its town's own home's, or
/// the machine's.
fn config_path_for(dir: &Path) -> Option<PathBuf> {
    match horadric_hooks::towns::home_for(&dir.to_string_lossy()) {
        Some(h) => Some(h.join(".claude.json")),
        None => config_path(),
    }
}

/// Who a town's own Claude home is logged in as, by email, from its
/// `.claude.json`. None when nobody is.
pub fn email_in(home: &Path) -> Option<String> {
    let config = read_json(&home.join(".claude.json")).ok()?;
    let email = config.get("oauthAccount")?.get("emailAddress")?.as_str()?;
    Some(email.to_string())
}

/// Why this instance may not switch accounts, if it may not. A dev
/// instance must not log the real Claude Code into another account, so it
/// switches only one with a home of its own, its variable set.
pub fn refused() -> Option<String> {
    let var = "CLAUDE_CONFIG_DIR";
    let own = std::env::var_os(var).is_some_and(|v| !v.is_empty());
    (horadric_hooks::dev() && !own).then(|| format!("A dev instance switches only with {var} set."))
}

fn read_json(path: &Path) -> io::Result<Value> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Written beside and renamed over, so Claude Code never reads half a file.
fn write_json(path: &Path, value: &Value) -> io::Result<()> {
    let tmp = path.with_extension("json.horadric-tmp");
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

/// Tells Claude Code it may trust `dir`, so a session started where no one
/// sits at the PC does not wait on its trust prompt.
///
/// claude.exe writes the same file whenever it likes, so what was read is
/// read again just before the rename: if it changed meanwhile, the change
/// is taken in and the key set again, so nothing of claude.exe's is lost
/// and the window stays the few microseconds of a rename.
pub fn trust(dir: &Path) -> io::Result<()> {
    let path = config_path_for(dir)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home folder"))?;
    let key = trust::project_key(&dir.to_string_lossy());
    for _ in 0..5 {
        let before = match fs::read(&path) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let mut config = match &before {
            Some(b) => serde_json::from_slice(b)
                // A file being written is not ours to replace with less.
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
            None => Value::Null,
        };
        if !trust::trust(&mut config, &key) {
            return Ok(());
        }
        let tmp = path.with_extension("json.horadric-tmp");
        let mut text = serde_json::to_string_pretty(&config)?;
        text.push('\n');
        fs::write(&tmp, text)?;
        if fs::read(&path).ok() != before {
            let _ = fs::remove_file(&tmp);
            continue;
        }
        return fs::rename(&tmp, &path);
    }
    Err(io::Error::other("Claude Code kept writing its settings"))
}

/// Whether Claude Code would start in `dir` without asking whether to
/// trust it.
pub fn trusted(dir: &Path) -> bool {
    let key = trust::project_key(&dir.to_string_lossy());
    config_path_for(dir)
        .and_then(|p| read_json(&p).ok())
        .is_some_and(|c| trust::accepted(&c, &key))
}

/// Who is logged in now, by id, from `.claude.json` alone, which says it
/// even while the token is being written.
pub fn live_id() -> Option<String> {
    accounts::identify(&read_json(&config_path()?).ok()?)
}

/// The login in use now, whole.
pub fn live() -> Option<Account> {
    let credentials = read_json(&credentials_path()?).ok()?;
    let config = read_json(&config_path()?).ok()?;
    Account::capture(&credentials, &config)
}

/// When the login files last changed.
pub fn stamp() -> Option<Stamp> {
    [credentials_path(), config_path()]
        .into_iter()
        .map(|p| fs::metadata(p?).ok()?.modified().ok())
        .collect()
}

/// Logs Claude Code in as `account`, the rest of its files as they were.
pub fn put(account: &Account) -> io::Result<()> {
    let missing = || io::Error::new(io::ErrorKind::NotFound, "no home folder");
    let credentials_path = credentials_path().ok_or_else(missing)?;
    let config_path = config_path().ok_or_else(missing)?;
    let mut credentials = read_json(&credentials_path).unwrap_or(Value::Null);
    let mut config = read_json(&config_path)?;
    account.put_credentials(&mut credentials);
    account.put_profile(&mut config);
    write_json(&credentials_path, &credentials)?;
    write_json(&config_path, &config)
}

/// Opens Claude Code's own login in a console of its own. It is a login,
/// not a session, so it gets no pane. The app sees the new login in the
/// files.
pub fn log_in() -> io::Result<()> {
    log_in_at(None)
}

/// [`log_in`] into a town's own Claude home, `None` for the machine's.
pub fn log_in_at(home: Option<&Path>) -> io::Result<()> {
    let program = console::claude_program()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "claude not found on PATH"))?;
    // `start` gives it a console with working handles, which a program
    // without one cannot hand down.
    let mut command = std::process::Command::new("cmd.exe");
    command
        .raw_arg(format!(
            "/c start \"Log in to Claude\" \"{}\" auth login",
            program.display()
        ))
        .creation_flags(CREATE_NO_WINDOW);
    let tags = [horadric_hooks::SESSION_ENV, horadric_hooks::OWNER_ENV];
    for name in console::inherited_env(console::under_claude())
        .iter()
        .chain(&tags)
    {
        command.env_remove(name);
    }
    match home {
        Some(h) => command.env(horadric_core::towns::CONFIG_ENV, h),
        None => &mut command,
    };
    command.spawn().map(drop)
}

fn path() -> Option<PathBuf> {
    Some(store::dir()?.join("accounts.dat"))
}

pub fn load() -> Accounts {
    path()
        .and_then(|p| fs::read(p).ok())
        .and_then(|sealed| unseal(&sealed))
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save(all: &Accounts) -> io::Result<()> {
    let path = path().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no %APPDATA%"))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let sealed = seal(&serde_json::to_vec(all)?).ok_or_else(|| io::Error::other("DPAPI"))?;
    let tmp = path.with_extension("dat.tmp");
    fs::write(&tmp, sealed)?;
    fs::rename(&tmp, &path)
}

fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    }
}

/// Takes the bytes DPAPI handed out and frees its copy.
unsafe fn taken(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
    let _ = LocalFree(Some(HLOCAL(out.pbData as *mut _)));
    bytes
}

pub(crate) fn seal(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &blob(bytes),
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
        .ok()?;
        Some(taken(out))
    }
}

pub(crate) fn unseal(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &blob(bytes),
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
        .ok()?;
        Some(taken(out))
    }
}
