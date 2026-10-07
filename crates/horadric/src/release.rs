//! `horadric release`: the signing half of the updater. `keygen` makes the
//! key once, `sign` turns a release build folder into a signed
//! `latest.json` beside its binaries, and `sign-mac` turns the Mac archive
//! CI built into a signed `latest-macos.json` beside it. See "The updater"
//! in docs/PLAN.md.
//!
//! The key never leaves this machine. Losing it strands every install, since
//! they only trust the public half built into them, so `keygen` refuses to
//! overwrite one.

use std::path::{Path, PathBuf};

use horadric_core::release::{self, File, Manifest};
use horadric_ui::update::{self, PrivateKey};

/// The binaries a release carries, the same pair `reload` copies.
const FILES: [&str; 2] = ["horadric.exe", "horadricw.exe"];

const MANIFEST: &str = "latest.json";

const USAGE: &str = "\
usage: horadric release keygen                           Make the signing key and print its public half
       horadric release sign DIR [--notes TEXT]          Write a signed DIR\\latest.json
       horadric release sign-mac ARCHIVE [--notes TEXT]  Write a signed latest-macos.json beside
                                                         ARCHIVE, a Horadric-macos.tar.gz";

pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("keygen") if args.len() == 1 => keygen(&key_path()?),
        Some("sign") => {
            let (dir, notes) = sign_args(&args[1..])?;
            sign(&key_path()?, &dir, &notes)
        }
        Some("sign-mac") => {
            let (archive, notes) = sign_args(&args[1..])?;
            sign_mac(&key_path()?, &archive, &notes)
        }
        _ => Err(USAGE.into()),
    }
}

fn key_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("USERPROFILE").ok_or("USERPROFILE is not set")?;
    Ok(Path::new(&home).join(".horadric").join("updater.key"))
}

fn keygen(path: &Path) -> Result<(), String> {
    if path.exists() {
        return Err(format!(
            "{} exists already. Every install trusts the key in it, so it is never replaced",
            path.display()
        ));
    }
    let key = update::generate()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, release::base64_encode(&key.0) + "\n")
        .map_err(|e| format!("{}: {e}", path.display()))?;
    println!("Wrote the private key to {}", path.display());
    println!("Back it up now. Without it no install can be updated again.");
    println!();
    println!("Public key, for the source:");
    println!("{}", release::base64_encode(&key.public()));
    Ok(())
}

fn sign_args(args: &[String]) -> Result<(PathBuf, String), String> {
    let mut dir = None;
    let mut notes = String::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--notes" => notes = rest.next().ok_or("--notes needs the text")?.clone(),
            _ if dir.is_none() && !arg.starts_with("--") => dir = Some(PathBuf::from(arg)),
            _ => return Err(format!("unexpected `{arg}`\n\n{USAGE}")),
        }
    }
    Ok((dir.ok_or(USAGE)?, notes))
}

fn load_key(key_path: &Path) -> Result<PrivateKey, String> {
    let text = std::fs::read_to_string(key_path).map_err(|e| {
        format!(
            "{}: {e} (make one with `horadric release keygen`)",
            key_path.display()
        )
    })?;
    let raw = release::base64_decode(&text)
        .ok_or_else(|| format!("{} is not a key", key_path.display()))?;
    PrivateKey::from_bytes(&raw)
}

/// Signs a manifest of `files` at this build's version and writes it to
/// `out`.
fn write_signed(key: &PrivateKey, files: Vec<File>, notes: &str, out: &Path) -> Result<(), String> {
    // The version is this build's, the signer being the release build it
    // signs, so the manifest can not claim a version the binaries are not.
    let manifest = Manifest {
        version: env!("CARGO_PKG_VERSION").to_string(),
        notes: notes.to_string(),
        files,
    };
    let json = update::sign_manifest(key, &manifest)?;
    // A key that does not match its own public half would sign releases no
    // install accepts; better to hear it here than after publishing.
    update::verify_manifest(&key.public(), &json)?;
    std::fs::write(out, json).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("Signed {} {}", out.display(), manifest.version);
    Ok(())
}

fn sign(key_path: &Path, dir: &Path, notes: &str) -> Result<(), String> {
    let key = load_key(key_path)?;
    let mut files = Vec::new();
    for name in FILES {
        let path = dir.join(name);
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        files.push(File {
            name: name.to_string(),
            sha256: release::hex(&update::sha256(&bytes)?),
        });
    }
    write_signed(&key, files, notes, &dir.join(MANIFEST))
}

/// Where `latest-macos.json` goes for `archive`: beside it. A Mac fetches
/// the archive by the name in the manifest, so one renamed on the way
/// would sign a release no Mac can download.
fn mac_manifest_path(archive: &Path) -> Result<PathBuf, String> {
    if archive.file_name().and_then(|n| n.to_str()) != Some(release::MAC_ARCHIVE) {
        return Err(format!(
            "{} is not {}, the name a Mac downloads",
            archive.display(),
            release::MAC_ARCHIVE
        ));
    }
    let dir = archive.parent().unwrap_or(Path::new(""));
    Ok(dir.join(release::MAC_MANIFEST))
}

fn sign_mac(key_path: &Path, archive: &Path, notes: &str) -> Result<(), String> {
    let out = mac_manifest_path(archive)?;
    let key = load_key(key_path)?;
    let bytes = std::fs::read(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let files = vec![File {
        name: release::MAC_ARCHIVE.to_string(),
        sha256: release::hex(&update::sha256(&bytes)?),
    }];
    write_signed(&key, files, notes, &out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn sign_takes_a_folder_and_notes() {
        assert_eq!(
            sign_args(&args("target/release")).unwrap(),
            (PathBuf::from("target/release"), String::new())
        );
        let mut with_notes = args("--notes");
        with_notes.push("Two words".into());
        with_notes.push("dir".into());
        assert_eq!(
            sign_args(&with_notes).unwrap(),
            (PathBuf::from("dir"), "Two words".into())
        );
        assert!(sign_args(&[]).is_err());
        assert!(sign_args(&args("a b")).is_err());
        assert!(sign_args(&args("a --notes")).is_err());
        assert!(sign_args(&args("a --force")).is_err());
    }

    #[test]
    fn sign_writes_a_manifest_the_public_key_accepts() {
        let dir = std::env::temp_dir().join(format!("horadric-release-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("horadric.exe"), b"one").unwrap();
        std::fs::write(dir.join("horadricw.exe"), b"two").unwrap();
        let key = update::generate().unwrap();
        let key_path = dir.join("test.key");
        std::fs::write(&key_path, release::base64_encode(&key.0)).unwrap();

        sign(&key_path, &dir, "Notes").unwrap();
        let text = std::fs::read_to_string(dir.join(MANIFEST)).unwrap();
        let manifest = update::verify_manifest(&key.public(), &text).unwrap();
        assert_eq!(manifest.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest.notes, "Notes");
        assert_eq!(
            manifest.file("horadricw.exe").unwrap().sha256,
            release::hex(&update::sha256(b"two").unwrap())
        );
        assert!(keygen(&key_path).is_err(), "keygen replaced a key");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_mac_manifest_sits_beside_an_archive_of_the_right_name() {
        assert_eq!(
            mac_manifest_path(Path::new("dl/Horadric-macos.tar.gz")).unwrap(),
            Path::new("dl").join("latest-macos.json")
        );
        assert_eq!(
            mac_manifest_path(Path::new("Horadric-macos.tar.gz")).unwrap(),
            PathBuf::from("latest-macos.json")
        );
        assert!(mac_manifest_path(Path::new("dl/horadric.tar.gz")).is_err());
        assert!(mac_manifest_path(Path::new("dl")).is_err());
    }

    #[test]
    fn sign_mac_writes_a_manifest_of_the_archive_alone() {
        let dir = std::env::temp_dir().join(format!("horadric-release-mac-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join(release::MAC_ARCHIVE);
        std::fs::write(&archive, b"tarball").unwrap();
        let key = update::generate().unwrap();
        let key_path = dir.join("test.key");
        std::fs::write(&key_path, release::base64_encode(&key.0)).unwrap();

        sign_mac(&key_path, &archive, "Mac notes").unwrap();
        let text = std::fs::read_to_string(dir.join(release::MAC_MANIFEST)).unwrap();
        let manifest = update::verify_manifest(&key.public(), &text).unwrap();
        assert_eq!(manifest.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest.notes, "Mac notes");
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(
            release::archive_hash(&manifest).unwrap(),
            release::hex(&update::sha256(b"tarball").unwrap())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
