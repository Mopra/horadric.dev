//! The updater on a Mac: the check for a newer release, its download and
//! unpacking, and the crypto behind both. `latest-macos.json` is signed
//! with the same key and in the same format as Windows' `latest.json`
//! (see "The updater" in docs/PLAN.md), so all it changes is the plumbing:
//! `/usr/bin/curl` for the network, CommonCrypto for SHA-256 and the
//! Security framework for ECDSA P-256, each already on every Mac, so no
//! crate for any of them.
//!
//! The app calls [`Updater`] on the main thread; everything that touches
//! the network or the disk runs on a thread of its own and leaves its
//! answer where the next [`Updater::tick`] picks it up.

use std::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use horadric_core::release::{self, hex, Manifest, Signed, PUBLIC_KEY_LEN, SIGNATURE_LEN};

/// A manifest is a few hundred bytes. Anything much larger is not one.
const MANIFEST_LIMIT: usize = 64 * 1024;

/// The app bundle packed is some tens of megabytes. This leaves room to
/// grow and still stops a server that never ends its answer.
const ARCHIVE_LIMIT: usize = 256 * 1024 * 1024;

/// Seconds curl may take, so a stalled connection ends in an error the
/// human hears rather than a check that never answers.
const MANIFEST_SECONDS: u32 = 30;
const ARCHIVE_SECONDS: u32 = 15 * 60;

/// What the threads hand back.
#[derive(Default)]
struct Found {
    looked: Option<Result<Option<Manifest>, String>>,
    ready: Option<Result<PathBuf, String>>,
}

pub struct Updater {
    told: Option<String>,
    started: Instant,
    last_check: Option<Instant>,
    checking: bool,
    /// Whether the human asked for the check under way, who then hears
    /// its answer.
    asked: bool,
    installing: bool,
    offer: Option<Manifest>,
    news: bool,
    checked: Option<Result<bool, String>>,
    ready: Option<Result<PathBuf, String>>,
    found: Arc<Mutex<Found>>,
}

impl Updater {
    pub fn new(told: Option<String>) -> Updater {
        Updater {
            told,
            started: Instant::now(),
            last_check: None,
            checking: false,
            asked: false,
            installing: false,
            offer: None,
            news: false,
            checked: None,
            ready: None,
            found: Arc::default(),
        }
    }

    /// Called every second by the app: takes in what the threads found
    /// and starts a check when one is due.
    pub fn tick(&mut self) {
        let (looked, ready) = match self.found.lock() {
            Ok(mut f) => (f.looked.take(), f.ready.take()),
            Err(_) => (None, None),
        };
        if let Some(looked) = looked {
            self.checking = false;
            self.landed(looked);
        }
        if let Some(ready) = ready {
            self.installing = false;
            self.ready = Some(ready);
        }
        if release::check_due(self.started.elapsed(), self.last_check.map(|t| t.elapsed())) {
            self.check(false);
        }
    }

    /// Checks now. `asked` when the human asked, who hears the answer
    /// through [`Updater::take_checked`].
    pub fn check(&mut self, asked: bool) {
        self.last_check = Some(Instant::now());
        self.asked |= asked;
        let Some(url) = update_url() else {
            if std::mem::take(&mut self.asked) {
                self.checked = Some(Err(
                    "A dev instance checks HORADRIC_UPDATE_URL only, and it is not set.".into(),
                ));
            }
            return;
        };
        // The check under way answers this one too, since `asked` stays.
        if self.checking {
            return;
        }
        self.checking = true;
        let found = Arc::clone(&self.found);
        std::thread::spawn(move || {
            let got = look(&url, env!("CARGO_PKG_VERSION"));
            if let Ok(mut f) = found.lock() {
                f.looked = Some(got);
            }
        });
    }

    /// What a check found. A version the human asked about is told of by
    /// the answer to their asking, so it is not news after that.
    fn landed(&mut self, looked: Result<Option<Manifest>, String>) {
        let asked = std::mem::take(&mut self.asked);
        match looked {
            Ok(Some(m)) => {
                eprintln!("horadric: Horadric {} is out", m.version);
                if asked {
                    self.told = Some(m.version.clone());
                    self.news = false;
                    self.checked = Some(Ok(true));
                } else if self.told.as_deref() != Some(m.version.as_str()) {
                    self.news = true;
                }
                self.offer = Some(m);
            }
            Ok(None) => {
                self.offer = None;
                self.news = false;
                if asked {
                    self.checked = Some(Ok(false));
                }
            }
            Err(e) => {
                eprintln!("horadric: update check failed: {e}");
                if asked {
                    self.checked = Some(Err(e));
                }
            }
        }
    }

    /// The version on offer, once a check found one.
    pub fn offer(&self) -> Option<String> {
        self.offer.as_ref().map(|m| m.version.clone())
    }

    /// True once for each version newly on offer that the human was not
    /// told of yet, which counts as telling them.
    pub fn take_news(&mut self) -> bool {
        if !std::mem::take(&mut self.news) {
            return false;
        }
        self.told = self.offer();
        self.told.is_some()
    }

    /// The answer to a check the human asked for: whether an update was
    /// found, or why the check failed. Once.
    pub fn take_checked(&mut self) -> Option<Result<bool, String>> {
        self.checked.take()
    }

    pub fn manifest(&self) -> Option<Manifest> {
        self.offer.clone()
    }

    /// The last version the human was told of, for state.json.
    pub fn told(&self) -> Option<String> {
        self.told.clone()
    }

    /// Downloads, checks and unpacks the release on offer, on a thread.
    pub fn start_install(&mut self) {
        if self.installing {
            return;
        }
        let Some(manifest) = self.offer.clone() else {
            return;
        };
        let Some(url) = update_url() else {
            self.ready = Some(Err("There is nowhere to download it from.".into()));
            return;
        };
        self.installing = true;
        let found = Arc::clone(&self.found);
        std::thread::spawn(move || {
            let got = download(&url, &manifest).and_then(|exe| {
                // A dev instance's `swap` restarts from the folder it was
                // started from, so it would run the download in place and
                // install nothing. It stops at a verified download.
                if horadric_hooks::dev() {
                    Err(format!(
                        "A dev instance does not install it. It is verified, in {}",
                        exe.display()
                    ))
                } else {
                    Ok(exe)
                }
            });
            if let Ok(mut f) = found.lock() {
                f.ready = Some(got);
            }
        });
    }

    /// The new build's `horadric`, once an install is ready to hand over
    /// to, or why it failed. Once.
    pub fn take_ready(&mut self) -> Option<Result<PathBuf, String>> {
        self.ready.take()
    }
}

/// Where to look for a release, or None for a dev instance that was not
/// pointed anywhere.
fn update_url() -> Option<String> {
    let env = std::env::var("HORADRIC_UPDATE_URL").ok();
    release::mac_manifest_url(
        horadric_hooks::dev(),
        cfg!(debug_assertions),
        env.as_deref(),
    )
}

/// Fetches the manifest at `url` and checks it was signed by the trusted
/// key. The manifest when it offers a version newer than `current`, None
/// when this build is up to date. Blocks, so it runs on a thread.
fn look(url: &str, current: &str) -> Result<Option<Manifest>, String> {
    let body = curl(url, MANIFEST_LIMIT, MANIFEST_SECONDS)?;
    let text = String::from_utf8(body).map_err(|_| "the manifest is not UTF-8".to_string())?;
    let env = std::env::var("HORADRIC_UPDATE_KEY").ok();
    let key = release::trusted_key(cfg!(debug_assertions), env.as_deref());
    let key = release::base64_decode(&key).ok_or("the trusted key is not base64")?;
    let manifest = verify_manifest(&key, &text)?;
    Ok(release::is_update(&manifest.version, current).then_some(manifest))
}

/// Reads a manifest and gives it only if `public` signed it.
fn verify_manifest(public: &[u8], text: &str) -> Result<Manifest, String> {
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

/// The body at `url`, at most `limit` bytes. Fails on any HTTP error.
fn curl(url: &str, limit: usize, seconds: u32) -> Result<Vec<u8>, String> {
    // Only a plain http or https URL, so it can never read as an option.
    release::split_url(url).ok_or_else(|| format!("`{url}` is not an http or https URL"))?;
    let out = Command::new("/usr/bin/curl")
        .args(["-fsSL", "--proto", "=http,https", "--proto-redir", "=https"])
        .args(["--max-time", &seconds.to_string()])
        .args(["--max-filesize", &limit.to_string()])
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        return Err(format!("{url}: {}", why.trim()));
    }
    // A server that gives no length up front gets past --max-filesize.
    if out.stdout.len() > limit {
        return Err(format!("{url} is larger than {limit} bytes"));
    }
    Ok(out.stdout)
}

/// Downloads the verified `manifest`'s archive into
/// `~/Library/Caches/Horadric/updates/<version>`, unpacks it there and
/// gives the new `horadric`, for `swap`. Blocks, so it runs on a thread.
fn download(manifest_url: &str, manifest: &Manifest) -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let updates = Path::new(&home)
        .join("Library/Caches")
        .join(horadric_hooks::state_name())
        .join("updates");
    fetch_into(&updates, manifest_url, manifest, |url| {
        curl(url, ARCHIVE_LIMIT, ARCHIVE_SECONDS)
    })
}

/// [`download`] with the fetching handed in. The archive is checked
/// against its signed hash in memory, before anything is written or
/// unpacked, so tar never reads a byte the manifest does not vouch for.
fn fetch_into(
    updates: &Path,
    manifest_url: &str,
    manifest: &Manifest,
    get: impl Fn(&str) -> Result<Vec<u8>, String>,
) -> Result<PathBuf, String> {
    // The version names a folder, so it must be one parse accepts.
    release::parse_version(&manifest.version)
        .ok_or_else(|| format!("version `{}` is not x.y.z", manifest.version))?;
    let want = release::archive_hash(manifest)?;
    let name = release::MAC_ARCHIVE;
    let url = release::download_url(manifest_url, &manifest.version, name)
        .ok_or_else(|| format!("no URL for {name} beside {manifest_url}"))?;
    let body = get(&url)?;
    let got = hex(&sha256(&body));
    if got != want {
        return Err(format!(
            "{name} does not match the signed release ({got}, not {want})"
        ));
    }
    let dir = updates.join(&manifest.version);
    let unpacked = unpack(&dir, &body);
    if unpacked.is_err() {
        let _ = fs::remove_dir_all(&dir);
    }
    unpacked
}

fn unpack(dir: &Path, archive: &[u8]) -> Result<PathBuf, String> {
    // A download broken off earlier may have left a part behind.
    if dir.exists() {
        fs::remove_dir_all(dir).map_err(|e| format!("cannot clear {}: {e}", dir.display()))?;
    }
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = dir.join(release::MAC_ARCHIVE);
    fs::write(&path, archive).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    run(Command::new("/usr/bin/tar")
        .arg("-xzf")
        .arg(&path)
        .arg("-C")
        .arg(dir))?;
    let _ = fs::remove_file(&path);
    let app = dir.join(release::MAC_APP);
    // An app that is not notarized is stopped by Gatekeeper while it
    // carries the quarantine flag, so whatever set it on the way, it goes.
    // A bundle without the flag makes xattr fail, which is no failure.
    let _ = Command::new("/usr/bin/xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(&app)
        .stdin(Stdio::null())
        .output();
    let exe = dir.join(release::MAC_EXE);
    if !exe.is_file() {
        return Err(format!(
            "{} holds no {}",
            release::MAC_ARCHIVE,
            release::MAC_EXE
        ));
    }
    Ok(exe)
}

fn run(command: &mut Command) -> Result<(), String> {
    let out = command
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {command:?}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let why = String::from_utf8_lossy(&out.stderr);
        Err(format!("{command:?} failed: {}", why.trim()))
    }
}

// CommonCrypto, in libSystem, which every program links.

/// `CC_SHA256_CTX` from `<CommonCrypto/CommonDigest.h>`.
#[repr(C)]
struct Sha256Context {
    count: [u32; 2],
    hash: [u32; 8],
    wbuf: [u32; 16],
}

extern "C" {
    fn CC_SHA256_Init(c: *mut Sha256Context) -> i32;
    fn CC_SHA256_Update(c: *mut Sha256Context, data: *const c_void, len: u32) -> i32;
    fn CC_SHA256_Final(md: *mut u8, c: *mut Sha256Context) -> i32;
}

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut c = Sha256Context {
        count: [0; 2],
        hash: [0; 8],
        wbuf: [0; 16],
    };
    let mut out = [0u8; 32];
    unsafe {
        CC_SHA256_Init(&mut c);
        // CC_LONG is 32 bits, so a larger buffer goes in pieces.
        for chunk in data.chunks(u32::MAX as usize) {
            CC_SHA256_Update(&mut c, chunk.as_ptr().cast(), chunk.len() as u32);
        }
        CC_SHA256_Final(out.as_mut_ptr(), &mut c);
    }
    out
}

// The Security framework and the Core Foundation types it takes.

type CFTypeRef = *const c_void;

/// The callback tables Core Foundation exports, only ever passed by
/// address.
#[repr(C)]
struct CallBacks {
    _opaque: [u8; 0],
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFTypeDictionaryKeyCallBacks: CallBacks;
    static kCFTypeDictionaryValueCallBacks: CallBacks;
    fn CFDataCreate(allocator: CFTypeRef, bytes: *const u8, length: isize) -> CFTypeRef;
    fn CFDictionaryCreate(
        allocator: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        count: isize,
        key_callbacks: *const CallBacks,
        value_callbacks: *const CallBacks,
    ) -> CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
}

#[link(name = "Security", kind = "framework")]
extern "C" {
    static kSecAttrKeyType: CFTypeRef;
    static kSecAttrKeyTypeECSECPrimeRandom: CFTypeRef;
    static kSecAttrKeyClass: CFTypeRef;
    static kSecAttrKeyClassPublic: CFTypeRef;
    // The RFC 4754 algorithms take CNG's `r || s` as it is, but need
    // macOS 14, and the app runs on 11; X9.62 takes it as DER.
    static kSecKeyAlgorithmECDSASignatureMessageX962SHA256: CFTypeRef;
    fn SecKeyCreateWithData(
        data: CFTypeRef,
        attributes: CFTypeRef,
        error: *mut CFTypeRef,
    ) -> CFTypeRef;
    fn SecKeyVerifySignature(
        key: CFTypeRef,
        algorithm: CFTypeRef,
        signed_data: CFTypeRef,
        signature: CFTypeRef,
        error: *mut CFTypeRef,
    ) -> u8;
}

/// A Core Foundation object we made, released on drop.
struct Owned(CFTypeRef);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

fn data(bytes: &[u8]) -> Owned {
    Owned(unsafe { CFDataCreate(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize) })
}

/// Whether `signature`, CNG's `r || s`, is `public`'s signature over
/// `data`. Anything that goes wrong on the way, a malformed key included,
/// is a no.
fn verify(public: &[u8], signed: &[u8], signature: &[u8]) -> bool {
    let (Ok(signature), true) = (
        <&[u8; SIGNATURE_LEN]>::try_from(signature),
        public.len() == PUBLIC_KEY_LEN,
    ) else {
        return false;
    };
    // An uncompressed point, as ANSI X9.63 has it: 0x04, x, y.
    let mut point = Vec::with_capacity(1 + PUBLIC_KEY_LEN);
    point.push(0x04);
    point.extend_from_slice(public);
    unsafe {
        let key_data = data(&point);
        let keys = [kSecAttrKeyType, kSecAttrKeyClass];
        let values = [kSecAttrKeyTypeECSECPrimeRandom, kSecAttrKeyClassPublic];
        let attributes = Owned(CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            keys.len() as isize,
            std::ptr::addr_of!(kCFTypeDictionaryKeyCallBacks),
            std::ptr::addr_of!(kCFTypeDictionaryValueCallBacks),
        ));
        if key_data.0.is_null() || attributes.0.is_null() {
            return false;
        }
        let mut error: CFTypeRef = std::ptr::null();
        let key = Owned(SecKeyCreateWithData(key_data.0, attributes.0, &mut error));
        drop(Owned(error));
        if key.0.is_null() {
            return false;
        }
        let message = data(signed);
        let der = data(&release::signature_der(signature));
        if message.0.is_null() || der.0.is_null() {
            return false;
        }
        let mut error: CFTypeRef = std::ptr::null();
        let ok = SecKeyVerifySignature(
            key.0,
            kSecKeyAlgorithmECDSASignatureMessageX962SHA256,
            message.0,
            der.0,
            &mut error,
        ) != 0;
        drop(Owned(error));
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horadric_core::release::File;

    /// Signed once on Windows through CNG, `horadric_ui::update::generate`
    /// and `sign_manifest`, with a key made for it and thrown away. Only
    /// its public half is kept.
    const VECTOR_KEY: &str =
        "ozkAx2+boEjaVVlAI5Eah80onTnTQ4cbSGYm1KjA8YqHVOMUEwBS8B9hMMfR2HCcWHAfsDZfcSbUkhgXP0mMfA==";
    const VECTOR: &str = r#"{
  "files": [
    {
      "name": "Horadric-macos.tar.gz",
      "sha256": "0eb3e36bfb24dcd9bb1d1bece1531216b59539a8fde17ee80224af0653c92aa3"
    }
  ],
  "notes": "A test vector, signed once with a throwaway key.",
  "signature": "NSX528aRktHVvfdimSZr20hZZ+HUnsOWmoQVlXHJCdvHr46CHABw/j6qWhp/kWX0pqyNfIC+jlJN0LfG7suSCw==",
  "version": "0.9.0"
}
"#;

    fn vector_key() -> Vec<u8> {
        release::base64_decode(VECTOR_KEY).unwrap()
    }

    #[test]
    fn sha256_matches_the_standard() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // The vector's archive hash was made by CNG over these bytes.
        assert_eq!(
            hex(&sha256(b"archive")),
            "0eb3e36bfb24dcd9bb1d1bece1531216b59539a8fde17ee80224af0653c92aa3"
        );
    }

    #[test]
    fn a_manifest_cng_signed_passes() {
        let m = verify_manifest(&vector_key(), VECTOR).unwrap();
        assert_eq!(m.version, "0.9.0");
        assert_eq!(release::archive_hash(&m).unwrap(), hex(&sha256(b"archive")));
    }

    #[test]
    fn a_flipped_bit_fails() {
        let Signed {
            manifest,
            signature,
        } = release::parse(VECTOR).unwrap();
        let key = vector_key();
        let data = manifest.signed_bytes();
        assert!(verify(&key, &data, &signature));
        for i in [0, data.len() / 2, data.len() - 1] {
            let mut changed = data.clone();
            changed[i] ^= 1;
            assert!(!verify(&key, &changed, &signature), "byte {i}");
        }
        for i in [0, 31, 32, 63] {
            let mut changed = signature.clone();
            changed[i] ^= 1;
            assert!(!verify(&key, &data, &changed), "signature byte {i}");
        }
        let mut other = key.clone();
        other[10] ^= 1;
        assert!(!verify(&other, &data, &signature));
        let changed = VECTOR.replace("0.9.0", "9.9.9");
        assert!(verify_manifest(&key, &changed).is_err());
    }

    #[test]
    fn a_malformed_key_or_signature_fails_without_a_panic() {
        let Signed {
            manifest,
            signature,
        } = release::parse(VECTOR).unwrap();
        let data = manifest.signed_bytes();
        assert!(!verify(&[0u8; PUBLIC_KEY_LEN], &data, &signature));
        assert!(!verify(&vector_key()[..32], &data, &signature));
        assert!(!verify(&vector_key(), &data, &signature[..63]));
        assert!(!verify(&vector_key(), &data, &[0u8; SIGNATURE_LEN]));
    }

    fn offered(version: &str) -> Manifest {
        Manifest {
            version: version.into(),
            notes: "Notes".into(),
            files: Vec::new(),
        }
    }

    #[test]
    fn news_comes_once_per_version() {
        let mut u = Updater::new(None);
        u.landed(Ok(Some(offered("0.9.0"))));
        assert_eq!(u.offer().as_deref(), Some("0.9.0"));
        assert!(u.take_news());
        assert!(!u.take_news());
        assert_eq!(u.told().as_deref(), Some("0.9.0"));
        u.landed(Ok(Some(offered("0.9.0"))));
        assert!(!u.take_news());
        u.landed(Ok(Some(offered("0.9.1"))));
        assert!(u.take_news());
        assert_eq!(u.told().as_deref(), Some("0.9.1"));
        assert_eq!(u.take_checked(), None);
    }

    #[test]
    fn a_version_told_before_a_restart_is_not_news() {
        let mut u = Updater::new(Some("0.9.0".into()));
        u.landed(Ok(Some(offered("0.9.0"))));
        assert!(!u.take_news());
        assert_eq!(u.offer().as_deref(), Some("0.9.0"));
        assert_eq!(u.manifest(), Some(offered("0.9.0")));
    }

    #[test]
    fn an_asked_check_answers_once() {
        let mut u = Updater::new(None);
        u.asked = true;
        u.landed(Ok(Some(offered("0.9.0"))));
        assert_eq!(u.take_checked(), Some(Ok(true)));
        assert_eq!(u.take_checked(), None);
        // The answer told them, so it is not news as well.
        assert!(!u.take_news());
        assert_eq!(u.told().as_deref(), Some("0.9.0"));

        u.asked = true;
        u.landed(Ok(None));
        assert_eq!(u.take_checked(), Some(Ok(false)));
        assert_eq!(u.offer(), None);

        u.asked = true;
        u.landed(Err("offline".into()));
        assert_eq!(u.take_checked(), Some(Err("offline".into())));
        u.landed(Err("offline".into()));
        assert_eq!(u.take_checked(), None);
    }

    /// A folder of its own under the temp folder, gone when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("horadric-mac-update-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A `Horadric-macos.tar.gz` made the way CI packs one, holding a
    /// bundle whose `horadric` is `exe`, or no `horadric` at all.
    fn archive(scratch: &Scratch, exe: Option<&[u8]>) -> Vec<u8> {
        let src = scratch.0.join("src");
        let macos = src.join("Horadric.app/Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        if let Some(exe) = exe {
            fs::write(macos.join("horadric"), exe).unwrap();
        }
        let out = scratch.0.join("packed.tar.gz");
        run(Command::new("/usr/bin/tar")
            .arg("-czf")
            .arg(&out)
            .arg("-C")
            .arg(&src)
            .arg("Horadric.app"))
        .unwrap();
        fs::read(out).unwrap()
    }

    fn manifest_of(body: &[u8]) -> Manifest {
        Manifest {
            version: "0.9.0".into(),
            notes: String::new(),
            files: vec![File {
                name: release::MAC_ARCHIVE.into(),
                sha256: hex(&sha256(body)),
            }],
        }
    }

    const URL: &str = "http://127.0.0.1:8123/latest-macos.json";

    #[test]
    fn a_matching_archive_unpacks_into_its_version_folder() {
        let scratch = Scratch::new("good");
        let body = archive(&scratch, Some(b"new build"));
        let updates = scratch.0.join("updates");
        let served = body.clone();
        let get = |url: &str| {
            assert_eq!(url, "http://127.0.0.1:8123/Horadric-macos.tar.gz");
            Ok(served.clone())
        };
        let exe = fetch_into(&updates, URL, &manifest_of(&body), get).unwrap();
        assert_eq!(
            exe,
            updates.join("0.9.0/Horadric.app/Contents/MacOS/horadric")
        );
        assert_eq!(fs::read(&exe).unwrap(), b"new build");
        assert!(!updates.join("0.9.0").join(release::MAC_ARCHIVE).exists());
    }

    #[test]
    fn a_wrong_archive_writes_nothing() {
        let scratch = Scratch::new("bad");
        let body = archive(&scratch, Some(b"new build"));
        let updates = scratch.0.join("updates");
        let evil = archive(&Scratch::new("evil"), Some(b"evil build"));
        let e = fetch_into(&updates, URL, &manifest_of(&body), |_| Ok(evil.clone())).unwrap_err();
        assert!(e.contains("does not match the signed release"), "{e}");
        assert!(!updates.exists());
    }

    #[test]
    fn an_archive_without_the_app_leaves_nothing() {
        let scratch = Scratch::new("empty");
        let body = archive(&scratch, None);
        let updates = scratch.0.join("updates");
        let e = fetch_into(&updates, URL, &manifest_of(&body), |_| Ok(body.clone())).unwrap_err();
        assert!(e.contains("holds no"), "{e}");
        assert!(!updates.join("0.9.0").exists());
    }

    #[test]
    fn a_release_without_the_archive_fetches_nothing() {
        let scratch = Scratch::new("none");
        let mut m = manifest_of(b"x");
        m.files[0].name = "horadric.exe".into();
        let e = fetch_into(&scratch.0, URL, &m, |_| panic!("fetched")).unwrap_err();
        assert!(e.contains("has no Horadric-macos.tar.gz"), "{e}");
        m = manifest_of(b"x");
        m.version = "..".into();
        assert!(fetch_into(&scratch.0, URL, &m, |_| panic!("fetched")).is_err());
    }
}
