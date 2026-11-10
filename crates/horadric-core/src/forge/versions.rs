//! Every write to a file shown in the Forge keeps the bytes it replaced.
//!
//! Under the root the caller passes (`%APPDATA%\Horadric\forge\<project>\`)
//! each file has a folder named for its path in the project, holding one
//! file per kept version named for when it was kept, in Unix milliseconds.
//! A restore is the caller writing a version's bytes, which keeps the bytes
//! it replaces like any other write.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// One kept version of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// Where its bytes are.
    pub file: PathBuf,
    /// When it was kept, which is when the write that replaced it came.
    pub at: SystemTime,
    pub len: u64,
}

const EXT: &str = "v";

/// Folder names longer than this end in a hash instead, so a deep path
/// stays inside Windows' 260 character limit with the root before it.
const NAME_MAX: usize = 120;

/// Keeps `bytes` as the newest version of `rel_path`, unless the newest
/// kept already holds the same bytes, in which case that one is returned.
pub fn keep(root: &Path, rel_path: &Path, bytes: &[u8]) -> io::Result<Version> {
    keep_at(root, rel_path, bytes, SystemTime::now())
}

/// [`keep`] at a given time, for tests and for a caller that knows when
/// the write happened.
pub fn keep_at(root: &Path, rel_path: &Path, bytes: &[u8], now: SystemTime) -> io::Result<Version> {
    let dir = root.join(folder(rel_path));
    if let Some(newest) = list_in(&dir).into_iter().next() {
        // Pruned away between the listing and the read: keep it anew.
        if newest.len == bytes.len() as u64 && fs::read(&newest.file).ok().as_deref() == Some(bytes)
        {
            return Ok(newest);
        }
    }
    fs::create_dir_all(&dir)?;
    let mut ms = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    // Two writes in one millisecond, or a clock stepped back: the later
    // write still sorts after.
    if let Some(newest) = list_in(&dir).first() {
        ms = ms.max(millis(newest.at) + 1);
    }
    // The tmp file, made with create_new, is what claims a name: a second
    // keeper at the same millisecond cannot make it too, and once the first
    // has renamed it the name is taken by the version instead. Rename
    // replaces on Windows, so checking the version only before would lose
    // one of the two.
    let mut denied = 0;
    let (file, tmp) = loop {
        let file = dir.join(format!("{ms:013}.{EXT}"));
        let tmp = file.with_extension("tmp");
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(mut f) if !file.exists() => {
                if let Err(e) = io::Write::write_all(&mut f, bytes) {
                    drop(f);
                    let _ = fs::remove_file(&tmp);
                    return Err(e);
                }
                break (file, tmp);
            }
            Ok(f) => {
                drop(f);
                let _ = fs::remove_file(&tmp);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            // Windows refuses a name whose file another keeper is still
            // deleting. That name is as taken as one that exists, but a
            // folder that refuses every name must still end in an error.
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && denied < 64 => denied += 1,
            Err(e) => return Err(e),
        }
        ms += 1;
    };
    if let Err(e) = fs::rename(&tmp, &file) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(Version {
        file,
        at: UNIX_EPOCH + Duration::from_millis(ms),
        len: bytes.len() as u64,
    })
}

/// The kept versions of `rel_path`, newest first.
pub fn list(root: &Path, rel_path: &Path) -> Vec<Version> {
    list_in(&root.join(folder(rel_path)))
}

pub fn read(version: &Version) -> io::Result<Vec<u8>> {
    fs::read(&version.file)
}

/// Deletes the oldest versions, of any file, until what is kept under
/// `root` is at most `max_bytes`. Returns the bytes freed.
pub fn prune(root: &Path, max_bytes: u64) -> io::Result<u64> {
    let mut all = Vec::new();
    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            all.extend(list_in(&entry.path()));
        }
    }
    let mut total: u64 = all.iter().map(|v| v.len).sum();
    all.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.file.cmp(&b.file)));
    let mut freed = 0;
    for v in all {
        if total <= max_bytes {
            break;
        }
        // Gone already, to another prune or by hand: it no longer counts.
        match fs::remove_file(&v.file) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                total -= v.len;
                continue;
            }
            Err(e) => return Err(e),
        }
        total -= v.len;
        freed += v.len;
        if let Some(dir) = v.file.parent() {
            // Only succeeds once the folder is empty, which is the point.
            let _ = fs::remove_dir(dir);
        }
    }
    Ok(freed)
}

fn millis(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn list_in(dir: &Path) -> Vec<Version> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Version> = entries
        .flatten()
        .filter_map(|e| {
            let file = e.path();
            if file.extension()? != EXT {
                return None;
            }
            let ms: u64 = file.file_stem()?.to_str()?.parse().ok()?;
            Some(Version {
                at: UNIX_EPOCH + Duration::from_millis(ms),
                len: e.metadata().ok()?.len(),
                file,
            })
        })
        .collect();
    out.sort_by_key(|v| std::cmp::Reverse(v.at));
    out
}

/// The folder name for a path in the project: one name, whatever the
/// path's depth, that cannot climb out of the root or be a name Windows
/// refuses. Separators and anything outside a small safe set are escaped
/// as `%XX`, and the `f-` in front keeps `CON` and `..` from being what
/// they are. Both separators mean the same, so `a\b` and `a/b` share one.
pub(super) fn folder(rel_path: &Path) -> String {
    let parts: Vec<String> = rel_path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            Component::ParentDir => Some("..".to_string()),
            _ => None,
        })
        .flat_map(|s| s.split(['/', '\\']).map(str::to_string).collect::<Vec<_>>())
        .filter(|s| !s.is_empty() && s != ".")
        .collect();
    let joined = parts.join("/");
    let mut name = String::from("f-");
    let bytes = joined.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        let keep = b.is_ascii_alphanumeric()
            || b == b'-'
            || b == b'_'
            || (b == b'.' && i + 1 < bytes.len());
        if keep {
            name.push(b as char);
        } else {
            name.push_str(&format!("%{b:02X}"));
        }
    }
    if name.len() > NAME_MAX {
        let mut cut = NAME_MAX - 17;
        // Never split an escape, so two paths that differ there differ here.
        while name[..cut].rfind('%').is_some_and(|p| p + 3 > cut) {
            cut -= 1;
        }
        name = format!("{}~{:016x}", &name[..cut], fnv(joined.as_bytes()));
    }
    name
}

/// FNV-1a: stable across runs and builds, which `DefaultHasher` is not
/// promised to be, and a folder name must be found again next week.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("horadric-forge-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn at(ms: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(ms)
    }

    #[test]
    fn forge_versions_folder_names() {
        assert_eq!(folder(Path::new("decks/q3.html")), "f-decks%2Fq3.html");
        assert_eq!(folder(Path::new("decks\\q3.html")), "f-decks%2Fq3.html");
        assert_eq!(folder(Path::new("./a.md")), "f-a.md");
        assert_eq!(folder(Path::new("../../x")), "f-..%2F..%2Fx");
        assert_eq!(folder(Path::new("..")), "f-.%2E");
        assert_eq!(folder(Path::new("CON")), "f-CON");
        assert_eq!(folder(Path::new("a b:c*?.")), "f-a%20b%3Ac%2A%3F%2E");
        assert_eq!(folder(Path::new("æ.md")), "f-%C3%A6.md");
        let long = "d/".repeat(100) + "x.md";
        let name = folder(Path::new(&long));
        assert!(name.len() <= NAME_MAX, "{}", name.len());
        assert_ne!(name, folder(Path::new(&("d/".repeat(100) + "y.md"))));
        for name in [folder(Path::new("../../x")), name] {
            assert!(!name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']));
            assert!(!name.ends_with('.'));
        }
    }

    #[test]
    fn forge_versions_keep_list_skip_same() {
        let root = tmp("keep");
        let rel = Path::new("docs/a.md");
        let v1 = keep_at(&root, rel, b"one", at(1_000)).unwrap();
        let v2 = keep_at(&root, rel, b"two", at(2_000)).unwrap();
        let again = keep_at(&root, rel, b"two", at(3_000)).unwrap();
        assert_eq!(again, v2);
        let v3 = keep_at(&root, rel, b"three", at(2_000)).unwrap();
        assert_eq!(v3.at, at(2_001));
        let listed = list(&root, rel);
        assert_eq!(listed, vec![v3.clone(), v2, v1]);
        assert_eq!(read(&listed[0]).unwrap(), b"three");
        assert!(list(&root, Path::new("other.md")).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn forge_versions_prune_oldest_first() {
        let root = tmp("prune");
        keep_at(&root, Path::new("a"), b"aaaa", at(1)).unwrap();
        keep_at(&root, Path::new("b"), b"bbbb", at(2)).unwrap();
        keep_at(&root, Path::new("a"), b"AAAA", at(3)).unwrap();
        keep_at(&root, Path::new("b"), b"BBBB", at(4)).unwrap();
        assert_eq!(prune(&root, 16).unwrap(), 0);
        assert_eq!(prune(&root, 9).unwrap(), 8);
        assert_eq!(list(&root, Path::new("a")).len(), 1);
        assert_eq!(read(&list(&root, Path::new("a"))[0]).unwrap(), b"AAAA");
        assert_eq!(prune(&root, 0).unwrap(), 8);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        assert_eq!(prune(&root.join("missing"), 0).unwrap(), 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn forge_versions_same_millisecond_keeps_all() {
        let root = tmp("same-ms");
        let rel = Path::new("a.md");
        let a = keep_at(&root, rel, b"one", at(5_000)).unwrap();
        let b = keep_at(&root, rel, b"two", at(5_000)).unwrap();
        assert_ne!(a.file, b.file);
        // Keepers racing at one millisecond each get a name of their own.
        let threads: Vec<_> = (0..8u8)
            .map(|i| {
                let root = root.clone();
                std::thread::spawn(move || {
                    keep_at(&root, Path::new("a.md"), &[b'x', i], at(5_000)).unwrap()
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let listed = list(&root, rel);
        assert_eq!(listed.len(), 10);
        let mut kept: Vec<Vec<u8>> = listed.iter().map(|v| read(v).unwrap()).collect();
        kept.sort();
        kept.dedup();
        assert_eq!(kept.len(), 10);
        assert!(fs::read_dir(root.join(folder(rel)))
            .unwrap()
            .flatten()
            .all(|e| e.path().extension().is_some_and(|x| x == EXT)));
        let _ = fs::remove_dir_all(&root);
    }
}
