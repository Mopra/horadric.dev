//! The release manifest, `latest.json`: which version a release is, what
//! changed, and a SHA-256 for each binary, under one signature.
//!
//! The signature covers [`Manifest::signed_bytes`], which are rebuilt from
//! the parsed fields rather than taken from the file, so whitespace or key
//! order in the JSON can not change what was signed. The version is inside
//! them, so an old manifest can not be replayed to offer a downgrade. The
//! crypto itself is CNG and lives with the Windows code; this is only the
//! bytes and the rules.

use serde_json::{json, Value};

/// Starts the signed bytes, so a signature over something else of ours
/// can never pass for a manifest signature.
const DOMAIN: &str = "horadric release 1\n";

/// An ECDSA P-256 signature is `r` and `s`, 32 bytes each.
pub const SIGNATURE_LEN: usize = 64;

/// A P-256 public key is the point's `x` and `y`, 32 bytes each.
pub const PUBLIC_KEY_LEN: usize = 64;

/// The public half of `%USERPROFILE%\.horadric\updater.key`, the only key
/// an install trusts a release from. Changing it strands every install
/// built before the change.
pub const PUBLIC_KEY: &str =
    "8wrKH/wZATizeAmuj0oEfEGhBJytxORaRtNzXw7XTA0gb3Z1zc5DRz4HiisPxgYk1snLIvQmAL87JH6LGL0DgA==";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub name: String,
    /// Lowercase hex, 64 characters.
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub version: String,
    pub notes: String,
    pub files: Vec<File>,
}

/// A manifest as read from `latest.json`, before its signature is checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    pub manifest: Manifest,
    pub signature: Vec<u8>,
}

impl Manifest {
    /// What the signature is over. Compact JSON in a fixed key order,
    /// behind [`DOMAIN`].
    pub fn signed_bytes(&self) -> Vec<u8> {
        let files: Vec<String> = self
            .files
            .iter()
            .map(|f| {
                format!(
                    "{{\"name\":{},\"sha256\":{}}}",
                    quote(&f.name),
                    quote(&f.sha256)
                )
            })
            .collect();
        format!(
            "{DOMAIN}{{\"version\":{},\"notes\":{},\"files\":[{}]}}",
            quote(&self.version),
            quote(&self.notes),
            files.join(",")
        )
        .into_bytes()
    }

    /// `latest.json` as published, the signature beside the fields.
    pub fn to_json(&self, signature: &[u8]) -> String {
        let files: Vec<Value> = self
            .files
            .iter()
            .map(|f| json!({ "name": f.name, "sha256": f.sha256 }))
            .collect();
        let value = json!({
            "version": self.version,
            "notes": self.notes,
            "files": files,
            "signature": base64_encode(signature),
        });
        serde_json::to_string_pretty(&value).expect("a manifest always serialises") + "\n"
    }

    pub fn file(&self, name: &str) -> Option<&File> {
        self.files.iter().find(|f| f.name == name)
    }
}

fn quote(s: &str) -> String {
    serde_json::to_string(s).expect("a string always serialises")
}

/// Reads `latest.json`. Rejects anything the updater could be led astray
/// by: a version it can not compare, a hash that is not SHA-256, a file
/// name that is a path, a signature of the wrong length.
pub fn parse(text: &str) -> Result<Signed, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let field = |name: &str| {
        value
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("no `{name}`"))
    };
    let version = field("version")?.to_string();
    parse_version(&version).ok_or_else(|| format!("version `{version}` is not x.y.z"))?;
    let notes = field("notes")?.to_string();
    let signature =
        base64_decode(field("signature")?).ok_or("the signature is not base64".to_string())?;
    if signature.len() != SIGNATURE_LEN {
        return Err(format!(
            "the signature is {} bytes, not {SIGNATURE_LEN}",
            signature.len()
        ));
    }
    let list = value
        .get("files")
        .and_then(Value::as_array)
        .ok_or("no `files`")?;
    let mut files = Vec::new();
    for entry in list {
        let get = |name: &str| {
            entry
                .get(name)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("a file with no `{name}`"))
        };
        let name = get("name")?.to_string();
        if !plain_file_name(&name) {
            return Err(format!("file name `{name}` is not a plain file name"));
        }
        let sha256 = get("sha256")?.to_string();
        if !is_sha256_hex(&sha256) {
            return Err(format!("the hash of `{name}` is not SHA-256 hex"));
        }
        if files.iter().any(|f: &File| f.name == name) {
            return Err(format!("`{name}` is listed twice"));
        }
        files.push(File { name, sha256 });
    }
    Ok(Signed {
        manifest: Manifest {
            version,
            notes,
            files,
        },
        signature,
    })
}

/// A download lands in a folder under this name, so it must not be able
/// to climb out of it.
fn plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && !name.chars().any(char::is_control)
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `major.minor.patch`, numbers only. A pre-release suffix is not
/// accepted: the workspace version never has one, and ordering them is a
/// rule we would only get wrong.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.split('.');
    let mut next = || -> Option<u64> {
        let p = parts.next()?;
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        p.parse().ok()
    };
    let v = (next()?, next()?, next()?);
    parts.next().is_none().then_some(v)
}

/// Whether `offered` should be offered to a build that is `current`. Only
/// a strictly newer version is; an unreadable one never is.
pub fn is_update(offered: &str, current: &str) -> bool {
    match (parse_version(offered), parse_version(current)) {
        (Some(o), Some(c)) => o > c,
        _ => false,
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding. Small enough to own rather than add a
/// dependency for a signature and a key.
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim().as_bytes();
    if !text.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    for (index, chunk) in text.chunks(4).enumerate() {
        let last = index == text.len() / 4 - 1;
        let pad = chunk.iter().rev().take_while(|&&b| b == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return None;
        }
        let mut n = 0u32;
        for &b in &chunk[..4 - pad] {
            let v = ALPHABET.iter().position(|&a| a == b)? as u32;
            n = n << 6 | v;
        }
        n <<= 6 * pad as u32;
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..3 - pad]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_public_key_is_a_whole_point() {
        let key = base64_decode(PUBLIC_KEY).unwrap();
        assert_eq!(key.len(), PUBLIC_KEY_LEN);
        assert_eq!(base64_encode(&key), PUBLIC_KEY);
    }

    fn manifest() -> Manifest {
        Manifest {
            version: "0.2.0".into(),
            notes: "Faster \"tiles\"\nand a fix".into(),
            files: vec![
                File {
                    name: "horadric.exe".into(),
                    sha256: "a".repeat(64),
                },
                File {
                    name: "horadricw.exe".into(),
                    sha256: "b".repeat(64),
                },
            ],
        }
    }

    #[test]
    fn signed_bytes_are_fixed() {
        let expected = format!(
            "horadric release 1\n{{\"version\":\"0.2.0\",\"notes\":\"Faster \\\"tiles\\\"\\nand a fix\",\"files\":[{{\"name\":\"horadric.exe\",\"sha256\":\"{}\"}},{{\"name\":\"horadricw.exe\",\"sha256\":\"{}\"}}]}}",
            "a".repeat(64),
            "b".repeat(64)
        );
        assert_eq!(
            String::from_utf8(manifest().signed_bytes()).unwrap(),
            expected
        );
    }

    #[test]
    fn json_round_trips() {
        let sig = [7u8; SIGNATURE_LEN];
        let parsed = parse(&manifest().to_json(&sig)).unwrap();
        assert_eq!(parsed.manifest, manifest());
        assert_eq!(parsed.signature, sig);
    }

    #[test]
    fn signed_bytes_ignore_layout_and_key_order() {
        let sig = base64_encode(&[1u8; SIGNATURE_LEN]);
        let text = format!(
            r#"{{"signature":"{sig}","files":[{{"sha256":"{a}","name":"horadric.exe"}},{{"name":"horadricw.exe","sha256":"{b}"}}],
                "notes":"Faster \"tiles\"\nand a fix",   "version":"0.2.0"}}"#,
            a = "a".repeat(64),
            b = "b".repeat(64)
        );
        let parsed = parse(&text).unwrap();
        assert_eq!(parsed.manifest.signed_bytes(), manifest().signed_bytes());
    }

    #[test]
    fn changing_any_field_changes_the_signed_bytes() {
        let base = manifest().signed_bytes();
        let mut m = manifest();
        m.version = "0.2.1".into();
        assert_ne!(m.signed_bytes(), base);
        let mut m = manifest();
        m.notes.push('!');
        assert_ne!(m.signed_bytes(), base);
        let mut m = manifest();
        m.files[1].sha256 = "c".repeat(64);
        assert_ne!(m.signed_bytes(), base);
        let mut m = manifest();
        m.files.swap(0, 1);
        assert_ne!(m.signed_bytes(), base);
    }

    #[test]
    fn parse_rejects_what_could_mislead_the_updater() {
        let good = manifest().to_json(&[0u8; SIGNATURE_LEN]);
        assert!(parse(&good).is_ok());
        let bad = [
            good.replace("0.2.0", "0.2"),
            good.replace("0.2.0", "0.2.0-beta"),
            good.replace("horadricw.exe", "..\\\\horadricw.exe"),
            good.replace("horadricw.exe", "C:x.exe"),
            good.replace(&"a".repeat(64), &"A".repeat(64)),
            good.replace(&"a".repeat(64), &"a".repeat(63)),
            good.replace("horadricw.exe", "horadric.exe"),
            good.replace(
                &base64_encode(&[0u8; SIGNATURE_LEN]),
                &base64_encode(&[0u8; 63]),
            ),
            good.replace(&base64_encode(&[0u8; SIGNATURE_LEN]), "not base64!"),
            "[]".to_string(),
        ];
        for text in bad {
            assert!(parse(&text).is_err(), "accepted {text}");
        }
    }

    #[test]
    fn only_a_newer_version_is_an_update() {
        assert!(is_update("0.2.0", "0.1.0"));
        assert!(is_update("0.1.10", "0.1.9"));
        assert!(is_update("1.0.0", "0.99.99"));
        assert!(!is_update("0.1.0", "0.1.0"));
        assert!(!is_update("0.1.0", "0.2.0"));
        assert!(!is_update("0.0.9", "0.1.0"));
        assert!(!is_update("junk", "0.1.0"));
        assert!(!is_update("0.2.0", "junk"));
    }

    #[test]
    fn versions_are_three_numbers() {
        assert_eq!(parse_version("0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_version("10.20.30"), Some((10, 20, 30)));
        for bad in [
            "", "1", "1.2", "1.2.3.4", "1.x.3", "1..3", "+1.2.3", " 1.2.3",
        ] {
            assert_eq!(parse_version(bad), None, "{bad}");
        }
    }

    #[test]
    fn base64_matches_the_standard() {
        let cases: [(&[u8], &str); 7] = [
            (b"", ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
        ];
        for (raw, text) in cases {
            assert_eq!(base64_encode(raw), text);
            assert_eq!(base64_decode(text).as_deref(), Some(raw));
        }
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(base64_decode(&base64_encode(&all)), Some(all));
        for bad in ["Zg=", "Z===", "Zg==Zg==", "Zm9*", "Zg=a"] {
            assert_eq!(base64_decode(bad), None, "{bad}");
        }
    }

    #[test]
    fn hex_is_lowercase() {
        assert_eq!(hex(&[0x00, 0xab, 0x0f]), "00ab0f");
    }
}
