//! Bakes the Horadric icon and version information into both executables, so
//! Explorer, the Start menu, the taskbar and Task Manager show Horadric rather
//! than a generic program called `horadric.exe`.
//!
//! Writes a compiled resource file (`.res`) directly and hands it to the
//! linker, which takes `.res` files as input. No resource compiler and no
//! build dependency: the format is a few headers around the icon images and
//! a version block. The icon is drawn by the same code as the tray icon.

use std::env;
use std::fs;
use std::path::PathBuf;

#[allow(dead_code)]
#[path = "../horadric-ui/src/icon.rs"]
mod icon;

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;
const LANG_EN_US: u16 = 0x0409;
const SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../horadric-ui/src/icon.rs");
    let windows = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if !windows || !msvc {
        return;
    }

    let mut res = Vec::new();
    // A resource file starts with an empty entry, by convention.
    resource(&mut res, 0, 0, 0, &[]);

    let mut group = Vec::new();
    group.extend(0u16.to_le_bytes());
    group.extend(1u16.to_le_bytes()); // icons, not cursors
    group.extend((SIZES.len() as u16).to_le_bytes());
    for (i, &size) in SIZES.iter().enumerate() {
        let image = dib(size);
        let id = i as u16 + 1;
        // Width and height are a byte each, where 0 means 256.
        group.push(size as u8);
        group.push(size as u8);
        group.push(0); // colour count
        group.push(0); // reserved
        group.extend(1u16.to_le_bytes()); // planes
        group.extend(32u16.to_le_bytes()); // bits per pixel
        group.extend((image.len() as u32).to_le_bytes());
        group.extend(id.to_le_bytes());
        resource(&mut res, RT_ICON, id, 0, &image);
    }
    // Group 1 is the application icon: the first group in the file.
    resource(&mut res, RT_GROUP_ICON, 1, 0, &group);
    resource(&mut res, RT_VERSION, 1, LANG_EN_US, &version_info());

    let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("horadric.res");
    fs::write(&out, res).expect("write horadric.res");
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

/// One entry: a header naming its type and id by number, then the data,
/// padded to four bytes.
fn resource(out: &mut Vec<u8>, kind: u16, id: u16, lang: u16, data: &[u8]) {
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(32u32.to_le_bytes()); // header size
    out.extend(0xFFFFu16.to_le_bytes());
    out.extend(kind.to_le_bytes());
    out.extend(0xFFFFu16.to_le_bytes());
    out.extend(id.to_le_bytes());
    out.extend(0u32.to_le_bytes()); // data version
    let flags: u16 = if kind == 0 { 0 } else { 0x1030 }; // moveable, pure, discardable
    out.extend(flags.to_le_bytes());
    out.extend(lang.to_le_bytes());
    out.extend(0u32.to_le_bytes()); // version
    out.extend(0u32.to_le_bytes()); // characteristics
    out.extend(data);
    pad(out);
}

fn pad(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

/// An icon image as a bitmap: a header claiming double height, the colour
/// rows bottom up, then a one bit mask the alpha channel makes redundant.
fn dib(size: u32) -> Vec<u8> {
    let pixels = icon::pixels(size);
    let mut out = Vec::new();
    out.extend(40u32.to_le_bytes());
    out.extend((size as i32).to_le_bytes());
    out.extend((size as i32 * 2).to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(32u16.to_le_bytes());
    out.extend([0u8; 24]); // no compression, sizes and palette unused
    for row in (0..size).rev() {
        for col in 0..size {
            out.extend(pixels[(row * size + col) as usize].to_le_bytes());
        }
    }
    let mask_row = (size as usize).div_ceil(32) * 4;
    out.extend(vec![0u8; mask_row * size as usize]);
    out
}

/// The `VS_VERSIONINFO` block: numbers, then the strings Windows shows.
fn version_info() -> Vec<u8> {
    let version: Vec<u16> = env::var("CARGO_PKG_VERSION")
        .unwrap_or_default()
        .split('.')
        .map(|p| p.parse().unwrap_or(0))
        .chain(std::iter::repeat(0))
        .take(4)
        .collect();
    let ms = (version[0] as u32) << 16 | version[1] as u32;
    let ls = (version[2] as u32) << 16 | version[3] as u32;
    let mut fixed = Vec::new();
    for v in [
        0xFEEF_04BDu32, // signature
        0x0001_0000,    // structure version
        ms,
        ls,
        ms,
        ls,
        0x3F,        // flags mask
        0,           // flags
        0x0004_0004, // Windows NT
        1,           // application
        0,
        0,
        0,
    ] {
        fixed.extend(v.to_le_bytes());
    }

    let text = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let strings: Vec<Vec<u8>> = [
        ("CompanyName", "Horadric"),
        ("FileDescription", "Horadric"),
        ("FileVersion", text.as_str()),
        ("InternalName", "horadric"),
        ("LegalCopyright", "MIT licence"),
        ("ProductName", "Horadric"),
        ("ProductVersion", text.as_str()),
    ]
    .iter()
    .map(|(k, v)| block(k, Value::Text(v), Vec::new()))
    .collect();
    let table = block("040904B0", Value::None, strings);
    let string_info = block("StringFileInfo", Value::None, vec![table]);
    let translation = block(
        "Translation",
        Value::Binary(&[0x09, 0x04, 0xB0, 0x04]),
        Vec::new(),
    );
    let var_info = block("VarFileInfo", Value::None, vec![translation]);
    block(
        "VS_VERSION_INFO",
        Value::Binary(&fixed),
        vec![string_info, var_info],
    )
}

enum Value<'a> {
    None,
    Text(&'a str),
    Binary(&'a [u8]),
}

/// A version block: length, value length, type, key, value, children, with
/// the four byte alignment the format wants between each part.
fn block(key: &str, value: Value, children: Vec<Vec<u8>>) -> Vec<u8> {
    let mut out = vec![0u8; 6];
    out.extend(key.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
    pad(&mut out);
    let (value_len, kind) = match value {
        Value::None => (0u16, 1u16),
        Value::Text(t) => {
            let wide: Vec<u16> = t.encode_utf16().chain([0]).collect();
            out.extend(wide.iter().flat_map(|u| u.to_le_bytes()));
            // Text lengths count characters, binary lengths count bytes.
            (wide.len() as u16, 1)
        }
        Value::Binary(b) => {
            out.extend(b);
            (b.len() as u16, 0)
        }
    };
    for child in children {
        pad(&mut out);
        out.extend(child);
    }
    let len = out.len() as u16;
    out[0..2].copy_from_slice(&len.to_le_bytes());
    out[2..4].copy_from_slice(&value_len.to_le_bytes());
    out[4..6].copy_from_slice(&kind.to_le_bytes());
    out
}
