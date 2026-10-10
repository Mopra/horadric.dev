//! The small picture a site shows on its tabs, and beside its pages in
//! the address field's suggestions.
//!
//! WebView2 hands it over as a PNG. It is decoded once into pixels
//! Direct2D can draw, and the PNG is kept on disk by host, so a suggestion
//! has it before its site is opened again this run.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1RenderTarget, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

use horadric_core::history;

/// Its side in pixels once decoded: twice what it is drawn at, so it
/// stays sharp on a screen at 200%.
pub const SIZE: u32 = 32;

/// A site's picture as premultiplied BGRA, [`SIZE`] square.
pub struct Favicon {
    pub pixels: Vec<u8>,
}

thread_local! {
    /// Every host asked for this run, None where it has no picture, so
    /// the disk is read once a host.
    static CACHE: RefCell<HashMap<String, Option<Rc<Favicon>>>> = RefCell::new(HashMap::new());
}

/// The picture of the site at `url`, from this run or from disk.
pub fn of(url: &str) -> Option<Rc<Favicon>> {
    let host = history::host(url).to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    if let Some(known) = CACHE.with(|c| c.borrow().get(&host).cloned()) {
        return known;
    }
    let found = path(&host)
        .and_then(|p| fs::read(p).ok())
        .and_then(|png| decode(&png))
        .map(Rc::new);
    CACHE.with(|c| c.borrow_mut().insert(host, found.clone()));
    found
}

/// The site at `url` showed this PNG: kept on disk and for this run.
pub fn keep(url: &str, png: &[u8]) -> Option<Rc<Favicon>> {
    let host = history::host(url).to_ascii_lowercase();
    let icon = decode(png).map(Rc::new)?;
    if let Some(p) = path(&host) {
        if let Some(dir) = p.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(p, png);
    }
    CACHE.with(|c| c.borrow_mut().insert(host, Some(Rc::clone(&icon))));
    Some(icon)
}

fn path(host: &str) -> Option<PathBuf> {
    Some(crate::store::dir()?.join("favicons").join(file_name(host)?))
}

/// The file a host's picture is kept in: its name with what a file name
/// cannot hold, a port's colon, made safe.
pub fn file_name(host: &str) -> Option<String> {
    let safe: String = host
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' => c.to_ascii_lowercase(),
            _ => '_',
        })
        .collect();
    let named = safe.chars().any(|c| c.is_ascii_alphanumeric());
    named.then(|| format!("{safe}.png"))
}

/// Decodes an image WebView2 handed over, scaled to [`SIZE`].
fn decode(png: &[u8]) -> Option<Favicon> {
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let stream = factory.CreateStream().ok()?;
        stream.InitializeFromMemory(png).ok()?;
        let decoder = factory
            .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
            .ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let scaler = factory.CreateBitmapScaler().ok()?;
        scaler
            .Initialize(&frame, SIZE, SIZE, WICBitmapInterpolationModeFant)
            .ok()?;
        let converter = factory.CreateFormatConverter().ok()?;
        converter
            .Initialize(
                &scaler,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
            .ok()?;
        let mut pixels = vec![0u8; (SIZE * SIZE * 4) as usize];
        converter
            .CopyPixels(std::ptr::null(), SIZE * 4, &mut pixels)
            .ok()?;
        Some(Favicon { pixels })
    }
}

/// Draws `icon` into `r`.
pub fn draw(rt: &ID2D1RenderTarget, icon: &Favicon, r: D2D_RECT_F) {
    let props = D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
    };
    unsafe {
        let size = D2D_SIZE_U {
            width: SIZE,
            height: SIZE,
        };
        let pixels = icon.pixels.as_ptr() as *const std::ffi::c_void;
        if let Ok(bitmap) = rt.CreateBitmap(size, Some(pixels), SIZE * 4, &props) {
            rt.DrawBitmap(
                &bitmap,
                Some(&r),
                1.0,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_names_a_safe_file() {
        assert_eq!(file_name("GitHub.com").as_deref(), Some("github.com.png"));
        assert_eq!(
            file_name("localhost:3000").as_deref(),
            Some("localhost_3000.png")
        );
        assert_eq!(file_name("[::1]:80").as_deref(), Some("___1__80.png"));
        assert_eq!(file_name("..").as_deref(), None);
        assert_eq!(file_name("").as_deref(), None);
    }
}
