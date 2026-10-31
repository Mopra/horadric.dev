//! Catching the shared lore up from the backups of other machines, for a
//! brother that is awake while every other one is silent: their backups
//! hold their shared lore's logs and the store months those name
//! passages in, so the one at the backup's target never sleeps.
//!
//! Each machine's backup is copied, only those files, into
//! `catch-up\<machine>\memory` in the state folder, and a file is fetched
//! again only when its hash moved. That copy is then read as a brother
//! would send it ([`horadric_memory::sharing::Project::catch_up_from`]).
//! It must open with this machine's own passphrase: a backup sealed with
//! another is passed over, since asking for it hourly is no way to live.

use std::path::Path;

use super::{blob, crypto, local, manifest, Vault};
use crate::portal_crypto::open_bound;

/// The folder in the state folder that holds the copies.
pub const FOLDER: &str = "catch-up";
/// Beside a machine's copy: the projects it was all taken in for, so a
/// copy nothing new came to is not read again, while a project opened
/// here since still gets its catch up.
const CAUGHT: &str = "caught";

/// Whether a backed up file is one a catch up reads: a shared lore's log,
/// or a month of the store, which holds the passages a log names.
pub fn wanted(rel: &str) -> bool {
    let Some(name) = rel.strip_prefix("memory/") else {
        return false;
    };
    let name = name.strip_prefix("brethren/").unwrap_or(name);
    !name.contains('/') && name.ends_with(".jsonl")
}

/// Brings the copy in `into` up to the backup in `vault`: each wanted
/// file whose hash differs from the copy's is fetched, opened and checked.
/// Gives how many were fetched. Nothing is written before the passphrase
/// is found to open it.
pub fn fetch(vault: &dyn Vault, passphrase: &str, into: &Path) -> Result<usize, String> {
    let Some(m) = manifest(vault)? else {
        return Ok(0);
    };
    let key = m.unlock(passphrase)?;
    let mut fetched = 0;
    for entry in m.files.iter().filter(|e| wanted(&e.path)) {
        let path = local(into, &entry.path);
        let held = std::fs::read(&path)
            .ok()
            .map(|b| crypto::hex(&crypto::sha256(&b)));
        if held.as_deref() == Some(entry.sha256.as_str()) {
            continue;
        }
        let sealed = vault.get(&blob(entry))?;
        let plain = open_bound(&key, &sealed, entry.path.as_bytes())
            .ok_or_else(|| format!("{} in the backup is damaged", entry.path))?;
        if crypto::hex(&crypto::sha256(&plain)) != entry.sha256 {
            return Err(format!("{} in the backup is not what was backed up", entry.path));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&path, &plain).map_err(|e| format!("{}: {e}", path.display()))?;
        fetched += 1;
    }
    if fetched > 0 {
        let _ = std::fs::remove_file(into.join(CAUGHT));
    }
    Ok(fetched)
}

/// Whether the copy in `into` was all taken in for `projects` since it
/// last changed.
pub fn caught(into: &Path, projects: &[&str]) -> bool {
    std::fs::read_to_string(into.join(CAUGHT)).is_ok_and(|t| t == projects.join("\n"))
}

/// Marks the copy in `into` as taken in for `projects`, until a fetch
/// changes it.
pub fn mark_caught(into: &Path, projects: &[&str]) {
    let _ = std::fs::write(into.join(CAUGHT), projects.join("\n"));
}

#[cfg(test)]
mod tests {
    use super::super::{back_up, MemoryVault};
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("horadric-catch-up-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn write(root: &Path, rel: &str, text: &str) {
        let path = local(root, rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn only_the_logs_and_the_months_are_wanted() {
        assert!(wanted("memory/2026-10.jsonl"));
        assert!(wanted("memory/brethren/horadric.dev-990a2c84.jsonl"));
        assert!(!wanted("memory/day/2026-10-16.md"));
        assert!(!wanted("memory/horadrim.json"));
        assert!(!wanted("lore/horadric.dev-990a2c84.json"));
        assert!(!wanted("memory/brethren/deeper/x.jsonl"));
    }

    #[test]
    fn a_copy_is_fetched_once_and_again_only_where_it_changed() {
        let state = scratch("state");
        let into = scratch("into");
        write(&state, "memory/2026-10.jsonl", "a month");
        write(&state, "memory/brethren/p-1.jsonl", "a log");
        write(&state, "memory/day/2026-10-16.md", "a day");
        let vault = MemoryVault::default();
        back_up(&state, &vault, "passphrase", 100).unwrap();

        assert!(fetch(&vault, "wrong one", &into).is_err());
        assert!(!into.exists(), "a wrong passphrase writes nothing");
        assert_eq!(fetch(&vault, "passphrase", &into).unwrap(), 2);
        assert!(!local(&into, "memory/day/2026-10-16.md").exists());
        mark_caught(&into, &["p"]);
        assert_eq!(fetch(&vault, "passphrase", &into).unwrap(), 0);
        assert!(caught(&into, &["p"]));
        assert!(!caught(&into, &["p", "q"]), "a project opened since is read for");

        write(&state, "memory/brethren/p-1.jsonl", "a longer log");
        back_up(&state, &vault, "passphrase", 200).unwrap();
        assert_eq!(fetch(&vault, "passphrase", &into).unwrap(), 1);
        assert!(!caught(&into, &["p"]), "a fetch that changed the copy asks for a read");
        let log = std::fs::read_to_string(local(&into, "memory/brethren/p-1.jsonl")).unwrap();
        assert_eq!(log, "a longer log");
        let _ = std::fs::remove_dir_all(&state);
        let _ = std::fs::remove_dir_all(&into);
    }

    #[test]
    fn no_backup_there_is_nothing_to_fetch() {
        let into = scratch("none");
        assert_eq!(fetch(&MemoryVault::default(), "passphrase", &into).unwrap(), 0);
    }
}
