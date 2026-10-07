//! The Mac app icon, `horadric.icns`, made from the icon [`crate::icon`]
//! draws, so the Mac has no icon file to keep in step with it either.
//!
//! An `.icns` is a header and one PNG per size. The PNGs here are written
//! with stored deflate blocks, uncompressed: a few hundred kilobytes more
//! than a compressor would make, for no dependency and a dozen lines.

use crate::icon;

/// The sizes Finder and the Dock ask for, each with its `.icns` type.
const ENTRIES: [(&[u8; 4], u32); 7] = [
    (b"icp4", 16),
    (b"icp5", 32),
    (b"icp6", 64),
    (b"ic07", 128),
    (b"ic08", 256),
    (b"ic09", 512),
    (b"ic10", 1024),
];

/// `horadric.icns`, the dev instance's red cube with `dev`.
pub fn horadric(dev: bool) -> Vec<u8> {
    let entries: Vec<(&[u8; 4], Vec<u8>)> = ENTRIES
        .iter()
        .map(|(kind, size)| {
            let pixels = if dev {
                icon::dev_pixels(*size)
            } else {
                icon::pixels(*size)
            };
            (*kind, png(*size, *size, &pixels))
        })
        .collect();
    icns(&entries)
}

/// An `.icns` holding these entries.
pub fn icns(entries: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let total = 8 + entries.iter().map(|(_, d)| 8 + d.len()).sum::<usize>();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&(total as u32).to_be_bytes());
    for (kind, data) in entries {
        out.extend_from_slice(*kind);
        out.extend_from_slice(&((8 + data.len()) as u32).to_be_bytes());
        out.extend_from_slice(data);
    }
    out
}

/// A PNG of `pixels`, `0xAARRGGBB` row by row from the top, as RGBA.
pub fn png(width: u32, height: u32, pixels: &[u32]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((width as usize * 4 + 1) * height as usize);
    for row in pixels.chunks(width as usize).take(height as usize) {
        // Filter type none.
        raw.push(0);
        for p in row {
            raw.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8, (p >> 24) as u8]);
        }
    }
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    // Eight bits a channel, RGBA, deflate, standard filters, not interlaced.
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// zlib around deflate's stored blocks: the bytes as they are, 65535 at
/// a time.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = if data.is_empty() {
        vec![&[]]
    } else {
        data.chunks(0xFFFF).collect()
    };
    for (i, block) in blocks.iter().enumerate() {
        out.push(u8::from(i + 1 == blocks.len()));
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_match_their_known_values() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn a_png_has_its_header_size_and_end() {
        let p = png(2, 1, &[0xFF00_00FF, 0x8000_FF00]);
        assert_eq!(&p[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&p[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes([p[16], p[17], p[18], p[19]]), 2);
        assert_eq!(u32::from_be_bytes([p[20], p[21], p[22], p[23]]), 1);
        assert_eq!(&p[p.len() - 8..p.len() - 4], b"IEND");
    }

    #[test]
    fn stored_blocks_split_at_the_limit_and_keep_every_byte() {
        let data = vec![7u8; 0x1_0001];
        let z = zlib_stored(&data);
        // Header, two block headers, the bytes, the checksum.
        assert_eq!(z.len(), 2 + 2 * 5 + data.len() + 4);
        assert_eq!(z[2], 0);
        assert_eq!(z[2 + 5 + 0xFFFF], 1);
    }

    #[test]
    fn the_icns_names_its_length_and_every_size() {
        let i = icns(&[(b"ic07", vec![1, 2, 3])]);
        assert_eq!(&i[..4], b"icns");
        assert_eq!(
            u32::from_be_bytes([i[4], i[5], i[6], i[7]]) as usize,
            i.len()
        );
        assert_eq!(&i[8..12], b"ic07");
        assert_eq!(u32::from_be_bytes([i[12], i[13], i[14], i[15]]), 11);
        // Every size is a different type, and they grow, so Finder can pick.
        for pair in ENTRIES.windows(2) {
            assert_ne!(pair[0].0, pair[1].0);
            assert!(pair[0].1 < pair[1].1);
        }
    }
}
