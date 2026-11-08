//! Web logins: which origin a page is on, whether a kept login belongs
//! there, and the one time code from a TOTP seed. The origin is the guard
//! a fill rests on (docs/plan/vault.md, "Web logins: filled, never read"),
//! so it is parsed here by hand, strictly, rather than trusted to anything
//! that forgives a malformed URL.

pub mod page;

/// The origin of a URL, `https://host[:port]`, lower case, with the
/// scheme's default port left out. None for anything but http and https,
/// and for a URL with no host.
pub fn origin(url: &str) -> Option<String> {
    let url = url.trim();
    let colon = url.find(':')?;
    let scheme = url[..colon].to_ascii_lowercase();
    if scheme != "https" && scheme != "http" {
        return None;
    }
    let rest = url[colon + 1..].strip_prefix("//")?;
    let end = rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len());
    let authority = &rest[..end];
    // A user name before the host is never part of the origin, and
    // `https://app.example.com@evil.example` must read as evil.example.
    let host_port = match authority.rfind('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    };
    let (host, port) = if let Some(after) = host_port.strip_prefix('[') {
        let close = after.find(']')?;
        let host = &host_port[..close + 2];
        let tail = &after[close + 1..];
        let port = match tail {
            "" => None,
            t => Some(t.strip_prefix(':')?),
        };
        (host, port)
    } else {
        match host_port.rfind(':') {
            Some(c) => (&host_port[..c], Some(&host_port[c + 1..])),
            None => (host_port, None),
        }
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '[' | ']' | ':'))
    {
        return None;
    }
    let port = match port {
        None | Some("") => None,
        Some(p) => {
            let n: u16 = p.parse().ok()?;
            let default = if scheme == "https" { 443 } else { 80 };
            (n != default).then_some(n)
        }
    };
    Some(match port {
        Some(p) => format!("{scheme}://{host}:{p}"),
        None => format!("{scheme}://{host}"),
    })
}

fn loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

/// Whether a kept origin is one a login may be filled on: written exactly
/// as `origin` would give it, https, or plain http only for this machine.
pub fn valid_origin(o: &str) -> bool {
    if origin(o).as_deref() != Some(o) {
        return false;
    }
    if o.starts_with("https://") {
        return true;
    }
    let Some(rest) = o.strip_prefix("http://") else {
        return false;
    };
    let host = match rest.rfind(':') {
        Some(c) if !rest[c..].contains(']') => &rest[..c],
        _ => rest,
    };
    loopback(host)
}

/// Whether a page's origin is this machine. Any program here, the agent's
/// own dev server among them, can serve a page on a loopback port, so a
/// login is filled there only Once at a time.
pub fn is_loopback(o: &str) -> bool {
    let Some(o) = origin(o) else {
        return false;
    };
    let rest = o.split_once("://").map_or("", |(_, r)| r);
    let host = match rest.rfind(':') {
        Some(c) if !rest[c..].contains(']') => &rest[..c],
        _ => rest,
    };
    loopback(host)
}

/// Whether the page at `url` is on one of a login's origins, exactly.
pub fn matches(url: &str, origins: &[String]) -> bool {
    let Some(here) = origin(url) else {
        return false;
    };
    valid_origin(&here)
        && origins
            .iter()
            .any(|o| origin(o).is_some_and(|o| o == here && valid_origin(&o)))
}

/// The RFC 6238 code for a base32 seed at a time: SHA1, 30 second steps,
/// six digits, the defaults every authenticator app assumes.
pub fn totp(seed_base32: &str, unix: u64) -> Option<String> {
    let key = base32(seed_base32)?;
    if key.is_empty() {
        return None;
    }
    Some(hotp(&key, unix / 30))
}

fn hotp(key: &[u8], counter: u64) -> String {
    let mac = hmac_sha1(key, &counter.to_be_bytes());
    let at = (mac[19] & 0x0f) as usize;
    let bin = u32::from_be_bytes([mac[at] & 0x7f, mac[at + 1], mac[at + 2], mac[at + 3]]);
    format!("{:06}", bin % 1_000_000)
}

/// RFC 4648 base32, as seeds are shown: any case, spaces and dashes for
/// reading, padding optional.
fn base32(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut acc: u64 = 0;
    let mut bits = 0;
    for c in s.chars() {
        if c == '=' || c == ' ' || c == '-' {
            continue;
        }
        let v = match c.to_ascii_uppercase() {
            c @ 'A'..='Z' => c as u64 - 'A' as u64,
            c @ '2'..='7' => c as u64 - '2' as u64 + 26,
            _ => return None,
        };
        acc = (acc << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..20].copy_from_slice(&sha1(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Vec::with_capacity(64 + data.len());
    inner.extend(block.iter().map(|b| b ^ 0x36));
    inner.extend_from_slice(data);
    let mut outer = Vec::with_capacity(84);
    outer.extend(block.iter().map(|b| b ^ 0x5c));
    outer.extend_from_slice(&sha1(&inner));
    sha1(&outer)
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (hv, v) in h.iter_mut().zip([a, b, c, d, e]) {
            *hv = hv.wrapping_add(v);
        }
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn sha1_known_digests() {
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(hex(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            hex(&sha1(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn hotp_rfc_4226_vectors() {
        let want = [
            "755224", "287082", "359152", "969429", "338314", "254676", "287922", "162583",
            "399871", "520489",
        ];
        for (i, w) in want.iter().enumerate() {
            assert_eq!(hotp(b"12345678901234567890", i as u64), *w);
        }
    }

    #[test]
    fn totp_rfc_6238_sha1_vectors() {
        // The RFC's key "12345678901234567890" in base32, and its eight digit
        // codes cut to the six an app shows.
        let seed = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
        for (t, code) in [
            (59, "94287082"),
            (1111111109, "07081804"),
            (1111111111, "14050471"),
            (1234567890, "89005924"),
            (2000000000, "69279037"),
            (20000000000, "65353130"),
        ] {
            assert_eq!(totp(seed, t).unwrap(), &code[2..]);
        }
    }

    #[test]
    fn base32_reads_seeds_as_shown() {
        assert_eq!(
            totp("gezd gnbv gy3t qojq gezd gnbv gy3t qojq", 59).unwrap(),
            "287082"
        );
        assert_eq!(base32("MZXW6===").unwrap(), b"foo");
        assert!(totp("not base32!", 0).is_none());
        assert!(totp("", 0).is_none());
    }

    #[test]
    fn origin_is_scheme_and_host_exactly() {
        assert_eq!(
            origin("https://App.Example.com/login?x=1").as_deref(),
            Some("https://app.example.com")
        );
        assert_eq!(origin("https://a.dk:443/").as_deref(), Some("https://a.dk"));
        assert_eq!(origin("https://a.dk:8443").as_deref(), Some("https://a.dk:8443"));
        assert_eq!(
            origin("http://localhost:3000/x").as_deref(),
            Some("http://localhost:3000")
        );
        assert_eq!(origin("http://[::1]:8080/").as_deref(), Some("http://[::1]:8080"));
        assert_eq!(
            origin("https://app.example.com@evil.example/").as_deref(),
            Some("https://evil.example")
        );
        assert_eq!(origin("https://a.dk#@b.dk").as_deref(), Some("https://a.dk"));
        assert_eq!(origin("about:blank"), None);
        assert_eq!(origin("file:///c:/x"), None);
        assert_eq!(origin("javascript:alert(1)"), None);
        assert_eq!(origin("https:///nohost"), None);
        assert_eq!(origin("https://a.dk:99999"), None);
    }

    #[test]
    fn valid_origins() {
        assert!(valid_origin("https://app.example.com"));
        assert!(valid_origin("http://localhost:3000"));
        assert!(valid_origin("http://127.0.0.1"));
        assert!(valid_origin("http://[::1]:8080"));
        assert!(!valid_origin("http://app.example.com"));
        assert!(!valid_origin("https://app.example.com/"));
        assert!(!valid_origin("https://App.example.com"));
        assert!(!valid_origin("https://*.example.com"));
        assert!(!valid_origin("http://localhost.evil.example"));
    }

    #[test]
    fn loopback_origins_are_this_machine() {
        assert!(is_loopback("http://localhost:3000"));
        assert!(is_loopback("https://127.0.0.1/x"));
        assert!(is_loopback("http://[::1]:8080"));
        assert!(!is_loopback("https://localhost.evil.example"));
        assert!(!is_loopback("https://app.example.com"));
    }

    #[test]
    fn matches_only_the_kept_origins() {
        let kept = vec!["https://app.example.com".to_string(), "http://localhost:3000".to_string()];
        assert!(matches("https://app.example.com/login", &kept));
        assert!(matches("http://localhost:3000/", &kept));
        assert!(!matches("http://app.example.com/login", &kept));
        assert!(!matches("https://example-login.test/", &kept));
        assert!(!matches("https://evil.app.example.com/", &kept));
        assert!(!matches("http://localhost:3001/", &kept));
        assert!(!matches("https://app.example.com@evil.example/", &kept));
        assert!(!matches("http://plain.example/", &["http://plain.example".to_string()]));
    }
}
