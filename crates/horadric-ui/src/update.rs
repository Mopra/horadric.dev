//! The updater: the check for a newer release, its download, and the
//! crypto behind both through Windows CNG: SHA-256, and ECDSA P-256 to sign a release manifest
//! and check it. No crate for it, since `bcrypt.dll` already does both (see
//! "The updater" in docs/PLAN.md).
//!
//! A private key is kept as `x`, `y` and `d`, 32 bytes each; a public key
//! as `x` and `y`. CNG wants them behind a `BCRYPT_ECCKEY_BLOB` header,
//! which is added here and never stored.

use std::fs;
use std::path::{Path, PathBuf};

use horadric_core::release::{self, hex, Manifest, Signed, PUBLIC_KEY_LEN, SIGNATURE_LEN};
use windows::core::PCWSTR;
use windows::Win32::Foundation::NTSTATUS;
use windows::Win32::Security::Cryptography::{
    BCryptCloseAlgorithmProvider, BCryptDestroyKey, BCryptExportKey, BCryptFinalizeKeyPair,
    BCryptGenerateKeyPair, BCryptHash, BCryptImportKeyPair, BCryptOpenAlgorithmProvider,
    BCryptSignHash, BCryptVerifySignature, BCRYPT_ALG_HANDLE, BCRYPT_ECCPRIVATE_BLOB,
    BCRYPT_ECCPUBLIC_BLOB, BCRYPT_ECDSA_P256_ALGORITHM, BCRYPT_ECDSA_PRIVATE_P256_MAGIC,
    BCRYPT_ECDSA_PUBLIC_P256_MAGIC, BCRYPT_FLAGS, BCRYPT_KEY_HANDLE,
    BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS, BCRYPT_SHA256_ALGORITHM,
};

const COORD: usize = 32;
pub const PRIVATE_KEY_LEN: usize = 3 * COORD;

/// A key pair made by [`generate`]: `x || y || d`.
pub struct PrivateKey(pub [u8; PRIVATE_KEY_LEN]);

impl PrivateKey {
    pub fn public(&self) -> [u8; PUBLIC_KEY_LEN] {
        let mut out = [0; PUBLIC_KEY_LEN];
        out.copy_from_slice(&self.0[..PUBLIC_KEY_LEN]);
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let raw: [u8; PRIVATE_KEY_LEN] = bytes
            .try_into()
            .map_err(|_| format!("a key is {PRIVATE_KEY_LEN} bytes, not {}", bytes.len()))?;
        Ok(Self(raw))
    }
}

fn check(status: NTSTATUS, what: &str) -> Result<(), String> {
    if status.is_ok() {
        Ok(())
    } else {
        Err(format!("{what} failed: NTSTATUS {:#010x}", status.0))
    }
}

/// An open CNG algorithm, closed on drop.
struct Algorithm(BCRYPT_ALG_HANDLE);

impl Algorithm {
    fn open(id: PCWSTR) -> Result<Self, String> {
        let mut handle = BCRYPT_ALG_HANDLE::default();
        let status = unsafe {
            BCryptOpenAlgorithmProvider(
                &mut handle,
                id,
                PCWSTR::null(),
                BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0),
            )
        };
        check(status, "BCryptOpenAlgorithmProvider")?;
        Ok(Self(handle))
    }
}

impl Drop for Algorithm {
    fn drop(&mut self) {
        unsafe {
            let _ = BCryptCloseAlgorithmProvider(self.0, 0);
        }
    }
}

/// A CNG key, destroyed on drop.
struct Key(BCRYPT_KEY_HANDLE);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = BCryptDestroyKey(self.0);
        }
    }
}

pub fn sha256(data: &[u8]) -> Result<[u8; 32], String> {
    let alg = Algorithm::open(BCRYPT_SHA256_ALGORITHM)?;
    let mut out = [0u8; 32];
    check(
        unsafe { BCryptHash(alg.0, None, data, &mut out) },
        "BCryptHash",
    )?;
    Ok(out)
}

/// The header CNG puts in front of a P-256 key: a magic number, then the
/// length of one coordinate.
fn blob(magic: u32, key: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + key.len());
    out.extend_from_slice(&magic.to_le_bytes());
    out.extend_from_slice(&(COORD as u32).to_le_bytes());
    out.extend_from_slice(key);
    out
}

fn import(alg: &Algorithm, kind: PCWSTR, blob: &[u8]) -> Result<Key, String> {
    let mut handle = BCRYPT_KEY_HANDLE::default();
    let status = unsafe { BCryptImportKeyPair(alg.0, None, kind, &mut handle, blob, 0) };
    check(status, "BCryptImportKeyPair")?;
    Ok(Key(handle))
}

pub fn generate() -> Result<PrivateKey, String> {
    let alg = Algorithm::open(BCRYPT_ECDSA_P256_ALGORITHM)?;
    let mut handle = BCRYPT_KEY_HANDLE::default();
    check(
        unsafe { BCryptGenerateKeyPair(alg.0, &mut handle, 256, 0) },
        "BCryptGenerateKeyPair",
    )?;
    let key = Key(handle);
    check(
        unsafe { BCryptFinalizeKeyPair(key.0, 0) },
        "BCryptFinalizeKeyPair",
    )?;
    let mut exported = [0u8; 8 + PRIVATE_KEY_LEN];
    let mut len = 0u32;
    check(
        unsafe {
            BCryptExportKey(
                key.0,
                None,
                BCRYPT_ECCPRIVATE_BLOB,
                Some(&mut exported),
                &mut len,
                0,
            )
        },
        "BCryptExportKey",
    )?;
    if len as usize != exported.len() {
        return Err(format!("BCryptExportKey gave {len} bytes"));
    }
    PrivateKey::from_bytes(&exported[8..])
}

/// Signs the SHA-256 of `data`, giving `r || s`.
pub fn sign(key: &PrivateKey, data: &[u8]) -> Result<[u8; SIGNATURE_LEN], String> {
    let alg = Algorithm::open(BCRYPT_ECDSA_P256_ALGORITHM)?;
    let key = import(
        &alg,
        BCRYPT_ECCPRIVATE_BLOB,
        &blob(BCRYPT_ECDSA_PRIVATE_P256_MAGIC, &key.0),
    )?;
    let hash = sha256(data)?;
    let mut out = [0u8; SIGNATURE_LEN];
    let mut len = 0u32;
    check(
        unsafe {
            BCryptSignHash(
                key.0,
                None,
                &hash,
                Some(&mut out),
                &mut len,
                BCRYPT_FLAGS(0),
            )
        },
        "BCryptSignHash",
    )?;
    if len as usize != SIGNATURE_LEN {
        return Err(format!("BCryptSignHash gave {len} bytes"));
    }
    Ok(out)
}

/// Whether `signature` is `public`'s signature over `data`. Anything that
/// goes wrong on the way, a malformed key included, is a no.
pub fn verify(public: &[u8], data: &[u8], signature: &[u8]) -> bool {
    if public.len() != PUBLIC_KEY_LEN || signature.len() != SIGNATURE_LEN {
        return false;
    }
    let Ok(alg) = Algorithm::open(BCRYPT_ECDSA_P256_ALGORITHM) else {
        return false;
    };
    let Ok(key) = import(
        &alg,
        BCRYPT_ECCPUBLIC_BLOB,
        &blob(BCRYPT_ECDSA_PUBLIC_P256_MAGIC, public),
    ) else {
        return false;
    };
    let Ok(hash) = sha256(data) else {
        return false;
    };
    unsafe { BCryptVerifySignature(key.0, None, &hash, signature, BCRYPT_FLAGS(0)) }.is_ok()
}

/// A manifest is a few hundred bytes. Anything much larger is not one.
const MANIFEST_LIMIT: usize = 64 * 1024;

/// Fetches the manifest at `url` and checks it was signed by the trusted
/// key. The manifest when it offers a version newer than `current`, None
/// when this build is up to date. Blocks, so it runs on a thread.
pub fn look(url: &str, current: &str) -> Result<Option<Manifest>, String> {
    let body = crate::net::get(url, MANIFEST_LIMIT)?;
    let text = String::from_utf8(body).map_err(|_| "the manifest is not UTF-8".to_string())?;
    let env = std::env::var("HORADRIC_UPDATE_KEY").ok();
    let key = release::trusted_key(cfg!(debug_assertions), env.as_deref());
    let key = release::base64_decode(&key).ok_or("the trusted key is not base64")?;
    let manifest = verify_manifest(&key, &text)?;
    Ok(release::is_update(&manifest.version, current).then_some(manifest))
}

/// A release binary is a few megabytes. This leaves room to grow and
/// still stops a server that never ends its answer.
const BINARY_LIMIT: usize = 256 * 1024 * 1024;

/// Downloads the verified `manifest`'s binaries into
/// `%LOCALAPPDATA%\Horadric\updates\<version>` and gives the `horadric.exe`
/// there, for `reload`. Blocks, so it runs on a thread.
pub fn download(manifest_url: &str, manifest: &Manifest) -> Result<PathBuf, String> {
    let dir = crate::store::local_dir()
        .ok_or("cannot find %LOCALAPPDATA%")?
        .join("updates");
    fetch_into(&dir, manifest_url, manifest, |url| {
        crate::net::get(url, BINARY_LIMIT)
    })
}

/// [`download`] with the fetching handed in. Both binaries are fetched
/// and checked against their hashes in memory, and only when both match
/// is anything written, so nothing under `updates` was ever not what the
/// signed manifest names.
fn fetch_into(
    updates: &Path,
    manifest_url: &str,
    manifest: &Manifest,
    get: impl Fn(&str) -> Result<Vec<u8>, String>,
) -> Result<PathBuf, String> {
    // The version names a folder, so it must be one parse accepts.
    release::parse_version(&manifest.version)
        .ok_or_else(|| format!("version `{}` is not x.y.z", manifest.version))?;
    let mut bodies = Vec::new();
    for (name, want) in release::binary_hashes(manifest)? {
        let url = release::download_url(manifest_url, &manifest.version, name)
            .ok_or_else(|| format!("no URL for {name} beside {manifest_url}"))?;
        let body = get(&url)?;
        let got = hex(&sha256(&body)?);
        if got != want {
            return Err(format!(
                "{name} does not match the signed release ({got}, not {want})"
            ));
        }
        bodies.push((name, body));
    }
    let dir = updates.join(&manifest.version);
    // A download broken off earlier may have left a part behind.
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| format!("cannot clear {}: {e}", dir.display()))?;
    }
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    for (name, body) in &bodies {
        let path = dir.join(name);
        fs::write(&path, body).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    Ok(dir.join(release::BINARIES[0]))
}

/// Signs a manifest and gives `latest.json`.
pub fn sign_manifest(key: &PrivateKey, manifest: &Manifest) -> Result<String, String> {
    let signature = sign(key, &manifest.signed_bytes())?;
    Ok(manifest.to_json(&signature))
}

/// Reads `latest.json` and gives its manifest only if `public` signed it.
pub fn verify_manifest(public: &[u8], text: &str) -> Result<Manifest, String> {
    let Signed {
        manifest,
        signature,
    } = release::parse(text)?;
    if verify(public, &manifest.signed_bytes(), &signature) {
        Ok(manifest)
    } else {
        Err("the signature does not match".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::release::{hex, is_update, File};

    fn manifest() -> Manifest {
        Manifest {
            version: "0.2.0".into(),
            notes: "Notes".into(),
            files: vec![
                File {
                    name: "horadric.exe".into(),
                    sha256: hex(&sha256(b"one").unwrap()),
                },
                File {
                    name: "horadricw.exe".into(),
                    sha256: hex(&sha256(b"two").unwrap()),
                },
            ],
        }
    }

    #[test]
    fn sha256_matches_the_standard() {
        assert_eq!(
            hex(&sha256(b"").unwrap()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc").unwrap()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_good_signature_passes() {
        let key = generate().unwrap();
        let text = sign_manifest(&key, &manifest()).unwrap();
        assert_eq!(verify_manifest(&key.public(), &text), Ok(manifest()));
    }

    #[test]
    fn another_key_fails() {
        let key = generate().unwrap();
        let other = generate().unwrap();
        let text = sign_manifest(&key, &manifest()).unwrap();
        assert!(verify_manifest(&other.public(), &text).is_err());
    }

    #[test]
    fn a_flipped_byte_fails() {
        let key = generate().unwrap();
        let data = manifest().signed_bytes();
        let signature = sign(&key, &data).unwrap();
        assert!(verify(&key.public(), &data, &signature));
        for i in [0, data.len() / 2, data.len() - 1] {
            let mut changed = data.clone();
            changed[i] ^= 1;
            assert!(!verify(&key.public(), &changed, &signature), "byte {i}");
        }
        for i in [0, 31, 32, 63] {
            let mut changed = signature;
            changed[i] ^= 1;
            assert!(
                !verify(&key.public(), &data, &changed),
                "signature byte {i}"
            );
        }
    }

    #[test]
    fn a_changed_version_fails() {
        let key = generate().unwrap();
        let text = sign_manifest(&key, &manifest()).unwrap();
        let changed = text.replace("\"0.2.0\"", "\"9.0.0\"");
        assert_ne!(changed, text);
        assert!(verify_manifest(&key.public(), &changed).is_err());
    }

    #[test]
    fn a_changed_hash_fails() {
        let key = generate().unwrap();
        let text = sign_manifest(&key, &manifest()).unwrap();
        let old = hex(&sha256(b"two").unwrap());
        let changed = text.replace(&old, &hex(&sha256(b"evil").unwrap()));
        assert!(verify_manifest(&key.public(), &changed).is_err());
    }

    #[test]
    fn an_older_signed_version_is_not_an_update() {
        let key = generate().unwrap();
        let mut old = manifest();
        old.version = "0.1.0".into();
        let text = sign_manifest(&key, &old).unwrap();
        let verified = verify_manifest(&key.public(), &text).unwrap();
        assert!(!is_update(&verified.version, "0.1.0"));
        assert!(!is_update(&verified.version, "0.2.0"));
        assert!(is_update(&verified.version, "0.0.9"));
    }

    #[test]
    fn a_malformed_public_key_fails_without_a_panic() {
        let key = generate().unwrap();
        let data = b"data";
        let signature = sign(&key, data).unwrap();
        assert!(!verify(&[0u8; PUBLIC_KEY_LEN], data, &signature));
        assert!(!verify(&key.public()[..32], data, &signature));
    }

    /// A folder of its own under the temp folder, gone when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("horadric-update-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const URL: &str = "http://127.0.0.1:8123/latest.json";

    fn serve(url: &str) -> Result<Vec<u8>, String> {
        match url {
            "http://127.0.0.1:8123/horadric.exe" => Ok(b"one".to_vec()),
            "http://127.0.0.1:8123/horadricw.exe" => Ok(b"two".to_vec()),
            other => Err(format!("404 {other}")),
        }
    }

    #[test]
    fn a_matching_download_lands_in_its_version_folder() {
        let scratch = Scratch::new("good");
        let exe = fetch_into(&scratch.0, URL, &manifest(), serve).unwrap();
        assert_eq!(exe, scratch.0.join("0.2.0").join("horadric.exe"));
        assert_eq!(fs::read(&exe).unwrap(), b"one");
        assert_eq!(
            fs::read(scratch.0.join("0.2.0").join("horadricw.exe")).unwrap(),
            b"two"
        );
    }

    #[test]
    fn a_wrong_binary_writes_nothing() {
        let scratch = Scratch::new("bad");
        let evil = |url: &str| {
            if url.ends_with("/horadricw.exe") {
                Ok(b"evil".to_vec())
            } else {
                serve(url)
            }
        };
        let e = fetch_into(&scratch.0, URL, &manifest(), evil).unwrap_err();
        assert!(e.contains("horadricw.exe does not match"), "{e}");
        assert!(!scratch.0.join("0.2.0").exists());
    }

    #[test]
    fn a_failed_fetch_writes_nothing() {
        let scratch = Scratch::new("gone");
        let other = "http://127.0.0.1:8123/x/latest.json";
        let e = fetch_into(&scratch.0, other, &manifest(), serve).unwrap_err();
        assert!(e.starts_with("404"), "{e}");
        assert!(!scratch.0.exists());
    }

    #[test]
    fn an_old_part_is_replaced() {
        let scratch = Scratch::new("part");
        let dir = scratch.0.join("0.2.0");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("horadric.exe"), b"half").unwrap();
        fs::write(dir.join("stray.exe"), b"x").unwrap();
        fetch_into(&scratch.0, URL, &manifest(), serve).unwrap();
        assert_eq!(fs::read(dir.join("horadric.exe")).unwrap(), b"one");
        assert!(!dir.join("stray.exe").exists());
    }

    #[test]
    fn a_version_that_is_a_path_is_refused() {
        let scratch = Scratch::new("path");
        let mut m = manifest();
        m.version = "..".into();
        assert!(fetch_into(&scratch.0, URL, &m, serve).is_err());
        assert!(!scratch.0.exists());
    }

    #[test]
    fn a_key_survives_its_bytes() {
        let key = generate().unwrap();
        let again = PrivateKey::from_bytes(&key.0).unwrap();
        let signature = sign(&again, b"x").unwrap();
        assert!(verify(&key.public(), b"x", &signature));
        assert!(PrivateKey::from_bytes(&key.0[1..]).is_err());
    }
}
