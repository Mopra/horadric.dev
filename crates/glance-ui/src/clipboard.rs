//! Text in and out of the Windows clipboard, and images and files out of it.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows::core::{w, Error, HSTRING};
use windows::Win32::Foundation::{GlobalFree, E_FAIL, GENERIC_WRITE, HANDLE, HGLOBAL};
use windows::Win32::Graphics::Gdi::{HBITMAP, HPALETTE};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, IWICImagingFactory, WICBitmapEncoderNoCache,
    WICBitmapIgnoreAlpha,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

const CF_BITMAP: u32 = 2;
const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const CF_DIBV5: u32 = 17;

/// Pasted images older than this are deleted when the next one is saved.
/// The agent reads the file when it is pasted, so a day is generous.
const KEEP_IMAGES: Duration = Duration::from_secs(24 * 60 * 60);

/// Another app can hold the clipboard open for a moment. Retrying briefly
/// beats failing a paste the user can see.
fn open() -> bool {
    for _ in 0..10 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

fn available(format: u32) -> bool {
    format != 0 && unsafe { IsClipboardFormatAvailable(format) }.is_ok()
}

pub fn has_text() -> bool {
    available(CF_UNICODETEXT)
}

/// Browsers and the Snipping Tool offer the PNG itself under one of these
/// names, with transparency intact. Everything else offers a bitmap.
fn png_formats() -> [u32; 2] {
    unsafe {
        [
            RegisterClipboardFormatW(w!("PNG")),
            RegisterClipboardFormatW(w!("image/png")),
        ]
    }
}

pub fn has_image() -> bool {
    png_formats().into_iter().any(available)
        || [CF_DIBV5, CF_DIB, CF_BITMAP].into_iter().any(available)
}

pub fn has_files() -> bool {
    available(CF_HDROP)
}

/// Files copied in Explorer.
pub fn get_files() -> Vec<String> {
    if !has_files() || !open() {
        return Vec::new();
    }
    let files = unsafe {
        GetClipboardData(CF_HDROP)
            .map(|h| drop_paths(HDROP(h.0)))
            .unwrap_or_default()
    };
    unsafe {
        let _ = CloseClipboard();
    }
    files
}

/// The paths in a drop, from Explorer or the clipboard.
pub fn drop_paths(hdrop: HDROP) -> Vec<String> {
    unsafe {
        let count = DragQueryFileW(hdrop, u32::MAX, None);
        (0..count)
            .filter_map(|i| {
                let len = DragQueryFileW(hdrop, i, None) as usize;
                let mut buf = vec![0u16; len + 1];
                let got = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
                (got > 0).then(|| String::from_utf16_lossy(&buf[..got]))
            })
            .collect()
    }
}

/// Saves the clipboard image as a PNG in the temp folder and returns its
/// path.
pub fn save_image() -> Option<PathBuf> {
    let dir = std::env::temp_dir().join("Glance");
    std::fs::create_dir_all(&dir).ok()?;
    forget_old_images(&dir);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let path = dir.join(format!("paste-{stamp}.png"));
    if !open() {
        return None;
    }
    let saved = unsafe {
        match png_formats().into_iter().find(|&f| available(f)) {
            Some(format) => write_global(format, &path),
            None => GetClipboardData(CF_BITMAP)
                .and_then(|h| encode_png(HBITMAP(h.0), &path))
                .is_ok(),
        }
    };
    unsafe {
        let _ = CloseClipboard();
    }
    if saved {
        Some(path)
    } else {
        let _ = std::fs::remove_file(&path);
        None
    }
}

fn forget_old_images(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > KEEP_IMAGES);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Writes a clipboard format's bytes as they are. The clipboard must be
/// open.
unsafe fn write_global(format: u32, path: &Path) -> bool {
    let Ok(h) = GetClipboardData(format) else {
        return false;
    };
    let mem = HGLOBAL(h.0);
    let ptr = GlobalLock(mem) as *const u8;
    if ptr.is_null() {
        return false;
    }
    let bytes = std::slice::from_raw_parts(ptr, GlobalSize(mem));
    let written = std::fs::write(path, bytes).is_ok();
    let _ = GlobalUnlock(mem);
    written
}

/// Encodes a bitmap as PNG with the Windows Imaging Component, which ships
/// with Windows and so costs no dependency. Alpha is ignored: a bitmap from
/// the clipboard rarely has a real one, and a wrong one makes it invisible.
unsafe fn encode_png(bitmap: HBITMAP, path: &Path) -> windows::core::Result<()> {
    let factory: IWICImagingFactory =
        CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
    let source =
        factory.CreateBitmapFromHBITMAP(bitmap, HPALETTE::default(), WICBitmapIgnoreAlpha)?;
    let stream = factory.CreateStream()?;
    stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_WRITE.0)?;
    let encoder = factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
    encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
    let mut frame = None;
    encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
    let frame = frame.ok_or_else(|| Error::from(E_FAIL))?;
    frame.Initialize(None)?;
    frame.WriteSource(&source, std::ptr::null())?;
    frame.Commit()?;
    encoder.Commit()
}

pub fn get_text() -> Option<String> {
    if !has_text() || !open() {
        return None;
    }
    let text = unsafe {
        GetClipboardData(CF_UNICODETEXT).ok().and_then(|h| {
            let mem = HGLOBAL(h.0);
            let ptr = GlobalLock(mem) as *const u16;
            if ptr.is_null() {
                return None;
            }
            let mut len = 0;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            let _ = GlobalUnlock(mem);
            Some(s)
        })
    };
    unsafe {
        let _ = CloseClipboard();
    }
    text
}

pub fn set_text(text: &str) {
    if !open() {
        return;
    }
    unsafe {
        let _ = EmptyClipboard();
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        if let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) {
            let ptr = GlobalLock(mem) as *mut u16;
            if !ptr.is_null() {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
                let _ = GlobalUnlock(mem);
                // On success the clipboard owns the memory. On failure we do.
                if SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0))).is_err() {
                    let _ = GlobalFree(Some(mem));
                }
            }
        }
        let _ = CloseClipboard();
    }
}
