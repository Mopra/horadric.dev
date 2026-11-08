//! Getting a file out of the Forge (docs/plan/the-forge.md, Getting it
//! out): a PDF through WebView2's `PrintToPdf`, pictures through DevTools
//! at twice the page's pixels or a deck's own size, the shown slide on the
//! clipboard, a real `.pptx` asked of the session, and the file handed to
//! Windows.
//!
//! Each runs from the bar's export menu on the UI thread; what WebView2
//! answers comes back on it too.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::{json, Value};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2Environment6, ICoreWebView2_2, ICoreWebView2_7,
    COREWEBVIEW2_PRINT_ORIENTATION_PORTRAIT,
};
use webview2_com::PrintToPdfCompletedHandler;
use windows::core::{w, Interface, HSTRING};
use windows::Win32::Foundation::{GlobalFree, HANDLE};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICImagingFactory,
    WICConvertBitmapSource, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FileSaveDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName, SHCreateMemStream,
    FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use crate::menu::{self, Item};
use crate::web;

const PDF: usize = 1;
const SLIDES: usize = 2;
const VISIBLE: usize = 3;
const COPY: usize = 4;
const PPTX: usize = 5;
const OPEN_WITH: usize = 6;
const EXPLORER: usize = 7;
const EDITOR: usize = 8;
const CHANGES: usize = 9;

/// The export menu for the file `file` shown in the tab `tab`.
pub fn menu(key: &str, tab: u64, file: &Path) {
    let deck = super::bar::is_deck(file);
    let page = !matches!(super::kind(file), Some(horadric_core::forge::Kind::Pdf));
    let mut items = Vec::new();
    if page {
        items.push(Item::action(
            PDF,
            if deck { "PDF, a page per slide" } else { "PDF" },
        ));
        if deck {
            items.push(Item::action(SLIDES, "Pictures of every slide"));
        }
        items.push(Item::action(VISIBLE, "Picture of what is shown"));
        items.push(Item::action(
            COPY,
            if deck {
                "Copy the slide as a picture"
            } else {
                "Copy what is shown as a picture"
            },
        ));
    }
    if deck {
        items.push(Item::action(PPTX, "To .pptx, made by the session"));
    }
    if !items.is_empty() {
        items.push(Item::Separator);
    }
    let root = super::TABS.with(|t| {
        t.borrow()
            .iter()
            .find(|f| f.key == key && f.id == tab)
            .map(|f| f.root.clone())
    });
    let changes = root.and_then(|r| super::home::diff_url(&r, file));
    if changes.is_some() {
        items.push(Item::action(CHANGES, "Changes since the last commit"));
    }
    items.push(Item::action(EDITOR, "Open in editor"));
    items.push(Item::action(
        OPEN_WITH,
        "Open with the program Windows has for it",
    ));
    items.push(Item::action(EXPLORER, "Show in Explorer"));
    let file = file.to_path_buf();
    match menu::popup(&items) {
        Some(PDF) => pdf(key, tab, &file, deck),
        Some(SLIDES) => slides(key, tab, &file),
        Some(VISIBLE) => visible(key, tab, &file),
        Some(COPY) => copy(key, tab, deck),
        Some(PPTX) => to_pptx(key, &file),
        Some(OPEN_WITH) => super::bar::open_with(&file),
        Some(EXPLORER) => super::bar::show_in_explorer(&file),
        Some(CHANGES) => {
            if let Some(url) = &changes {
                crate::web::navigate(key, tab, url);
            }
        }
        Some(EDITOR) => {
            super::source_view(key);
        }
        _ => {}
    }
}

fn stem(file: &Path) -> String {
    file.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "export".into())
}

/// Asks where to save, starting beside `file` with `name`. None when
/// cancelled. Runs a modal loop.
fn save_as(file: &Path, name: &str, what: &str, pattern: &str) -> Option<PathBuf> {
    unsafe {
        let dialog: IFileSaveDialog =
            CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        let _ = dialog.SetOptions(options | FOS_FORCEFILESYSTEM | FOS_OVERWRITEPROMPT);
        let what = HSTRING::from(what);
        let pattern_h = HSTRING::from(pattern);
        let types = [COMDLG_FILTERSPEC {
            pszName: windows::core::PCWSTR(what.as_ptr()),
            pszSpec: windows::core::PCWSTR(pattern_h.as_ptr()),
        }];
        let _ = dialog.SetFileTypes(&types);
        let ext = pattern.trim_start_matches("*.");
        let _ = dialog.SetDefaultExtension(&HSTRING::from(ext));
        let _ = dialog.SetFileName(&HSTRING::from(name));
        if let Some(dir) = file.parent().filter(|d| d.is_dir()) {
            let item: windows::core::Result<IShellItem> =
                SHCreateItemFromParsingName(&HSTRING::from(dir.as_os_str()), None);
            if let Ok(item) = item {
                let _ = dialog.SetFolder(&item);
            }
        }
        dialog.Show(Some(GetForegroundWindow())).ok()?;
        let item = dialog.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(name.0 as *const _));
        path
    }
}

/// The page as a PDF. A deck prints a slide a page at PowerPoint's
/// widescreen size with nothing round it, as deck.css lays it out for
/// print; anything else at the default paper with its backgrounds.
fn pdf(key: &str, tab: u64, file: &Path, deck: bool) {
    let Some(out) = save_as(file, &format!("{}.pdf", stem(file)), "PDF", "*.pdf") else {
        return;
    };
    web::with_view(key, tab, move |view| {
        let Some(view) = view else { return };
        let printed = (|| unsafe {
            let v7 = view.cast::<ICoreWebView2_7>()?;
            let env = view
                .cast::<ICoreWebView2_2>()?
                .Environment()?
                .cast::<ICoreWebView2Environment6>()?;
            let settings = env.CreatePrintSettings()?;
            if deck {
                // Width over height is landscape already; asking for
                // landscape as well would turn it back.
                settings.SetOrientation(COREWEBVIEW2_PRINT_ORIENTATION_PORTRAIT)?;
                settings.SetPageWidth(13.333)?;
                settings.SetPageHeight(7.5)?;
                settings.SetMarginTop(0.0)?;
                settings.SetMarginBottom(0.0)?;
                settings.SetMarginLeft(0.0)?;
                settings.SetMarginRight(0.0)?;
            }
            settings.SetShouldPrintBackgrounds(true)?;
            settings.SetShouldPrintHeaderAndFooter(false)?;
            let saved = out.clone();
            let done = PrintToPdfCompletedHandler::create(Box::new(move |result, ok| {
                if result.is_ok() && ok {
                    super::bar::show_in_explorer(&saved);
                } else {
                    eprintln!("horadric: the PDF of {} was not made", saved.display());
                }
                Ok(())
            }));
            v7.PrintToPdf(&HSTRING::from(out.as_os_str()), &settings, &done)
        })();
        if let Err(e) = printed {
            eprintln!("horadric: cannot print to PDF: {e}");
        }
    });
}

/// A rectangle of the page to take a picture of, in CSS pixels from the
/// document's top left, and how many picture pixels to a CSS pixel.
#[derive(Clone, Copy)]
struct Clip {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    scale: f64,
}

impl Clip {
    fn from_json(v: &Value) -> Option<Clip> {
        let f = |k: &str| v.get(k).and_then(Value::as_f64).filter(|n| n.is_finite());
        let clip = Clip {
            x: f("x")?,
            y: f("y")?,
            w: f("w")?,
            h: f("h")?,
            scale: f("scale").unwrap_or(2.0).clamp(1.0, 8.0),
        };
        (clip.w >= 1.0 && clip.h >= 1.0).then_some(clip)
    }
}

/// Runs `expr` in the page and gives `done` what it came to, a promise
/// awaited.
fn evaluate(key: &str, tab: u64, expr: &str, done: impl FnOnce(Option<Value>) + 'static) {
    let params = json!({"expression": expr, "returnByValue": true, "awaitPromise": true});
    web::devtools(
        key,
        tab,
        "Runtime.evaluate",
        &params.to_string(),
        move |r| {
            let value = r
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .and_then(|v| v.get("result")?.get("value").cloned());
            done(value);
        },
    );
}

/// A PNG of `clip`.
fn capture(key: &str, tab: u64, clip: Clip, done: impl FnOnce(Option<Vec<u8>>) + 'static) {
    let params = json!({
        "format": "png",
        "fromSurface": true,
        "clip": {"x": clip.x, "y": clip.y, "width": clip.w, "height": clip.h, "scale": clip.scale},
    });
    web::devtools(
        key,
        tab,
        "Page.captureScreenshot",
        &params.to_string(),
        move |r| {
            let png = r
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .and_then(|v| horadric_core::release::base64_decode(v.get("data")?.as_str()?));
            done(png);
        },
    );
}

/// What of the page is in view, at twice its pixels.
const VISIBLE_JS: &str = "({x: scrollX, y: scrollY, w: innerWidth, h: innerHeight, scale: 2})";

/// The slide `n` (the shown one for 0) shown, two frames drawn, and where
/// it is: at twice its pixels, or at the deck's own size when that is
/// more, so a slide fitted small in the pane still comes out sharp.
fn slide_js(n: usize) -> String {
    format!(
        "(async (n) => {{\
           const d = window.__forgeDeck; if (!d) return null;\
           if (n) d.goto(n);\
           await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)));\
           await new Promise(r => setTimeout(r, 80));\
           const s = document.querySelectorAll('section.slide')[d.current - 1];\
           if (!s) return null;\
           const b = s.getBoundingClientRect();\
           return {{x: b.left + scrollX, y: b.top + scrollY, w: b.width, h: b.height,\
                    scale: Math.max(2, (d.size.w || 0) / b.width), current: d.current, count: d.count}};\
         }})({n})"
    )
}

/// A picture of what the pane shows, saved where asked.
fn visible(key: &str, tab: u64, file: &Path) {
    let Some(out) = save_as(file, &format!("{}.png", stem(file)), "PNG picture", "*.png") else {
        return;
    };
    let owned = key.to_string();
    evaluate(key, tab, VISIBLE_JS, move |v| {
        let Some(clip) = v.as_ref().and_then(Clip::from_json) else {
            return;
        };
        capture(&owned, tab, clip, move |png| save(&out, png, true));
    });
}

fn save(out: &Path, png: Option<Vec<u8>>, reveal: bool) {
    let Some(png) = png else {
        eprintln!("horadric: no picture for {}", out.display());
        return;
    };
    match std::fs::write(out, png) {
        Ok(()) if reveal => super::bar::show_in_explorer(out),
        Ok(()) => {}
        Err(e) => eprintln!("horadric: cannot save {}: {e}", out.display()),
    }
}

/// A picture of each slide of the deck, `<name>-01.png` and on, in a
/// folder asked for, then the deck back on the slide it showed.
fn slides(key: &str, tab: u64, file: &Path) {
    let start = file.parent().map(Path::to_path_buf);
    let Some(dir) = crate::picker::pick_folder_for(
        unsafe { GetForegroundWindow() },
        start.as_deref(),
        "Save the slides as pictures in",
        "Save here",
    ) else {
        return;
    };
    let name = stem(file);
    let owned = key.to_string();
    evaluate(key, tab, &slide_js(0), move |v| {
        let Some(v) = v else { return };
        let count = v.get("count").and_then(Value::as_u64).unwrap_or(0) as usize;
        let was = v.get("current").and_then(Value::as_u64).unwrap_or(1) as usize;
        if count == 0 {
            return;
        }
        let job = Rc::new(Slides {
            key: owned,
            tab,
            dir,
            name,
            count,
            was,
        });
        job.shoot(1);
    });
}

/// Pictures of a deck's slides being taken, one after the other: each
/// needs its slide shown first.
struct Slides {
    key: String,
    tab: u64,
    dir: PathBuf,
    name: String,
    count: usize,
    /// The slide shown before, shown again at the end.
    was: usize,
}

impl Slides {
    fn shoot(self: Rc<Self>, n: usize) {
        if n > self.count {
            let back = format!(
                "window.__forgeDeck && window.__forgeDeck.goto({})",
                self.was
            );
            evaluate(&self.key, self.tab, &back, |_| {});
            super::bar::show_in_explorer(&self.out(1));
            return;
        }
        let job = Rc::clone(&self);
        evaluate(&self.key, self.tab, &slide_js(n), move |v| {
            let Some(clip) = v.as_ref().and_then(Clip::from_json) else {
                return;
            };
            let next = Rc::clone(&job);
            capture(&job.key, job.tab, clip, move |png| {
                save(&next.out(n), png, false);
                next.shoot(n + 1);
            });
        });
    }

    fn out(&self, n: usize) -> PathBuf {
        let width = self.count.to_string().len().max(2);
        self.dir.join(format!("{}-{n:0width$}.png", self.name))
    }
}

/// The shown slide, or what the pane shows of a page that is not a deck,
/// on the clipboard as a picture.
fn copy(key: &str, tab: u64, deck: bool) {
    let owned = key.to_string();
    let js = if deck {
        slide_js(0)
    } else {
        VISIBLE_JS.to_string()
    };
    evaluate(key, tab, &js, move |v| {
        let Some(clip) = v.as_ref().and_then(Clip::from_json) else {
            return;
        };
        capture(&owned, tab, clip, |png| {
            if let Some(png) = png {
                if !set_clipboard_image(&png) {
                    eprintln!("horadric: cannot put the picture on the clipboard");
                }
            }
        });
    });
}

/// Puts a PNG on the clipboard twice over: as PNG, which Teams, Office and
/// browsers take with its edges kept, and as a DIB for everything older.
fn set_clipboard_image(png: &[u8]) -> bool {
    let dib = dib_of(png);
    unsafe {
        if OpenClipboard(None).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        let png_format = RegisterClipboardFormatW(w!("PNG"));
        let mut any = put(png_format, png);
        if let Some(dib) = dib {
            // CF_DIB.
            any |= put(8, &dib);
        }
        let _ = CloseClipboard();
        any
    }
}

/// Hands `bytes` to the clipboard as `format`. The clipboard must be open.
unsafe fn put(format: u32, bytes: &[u8]) -> bool {
    if format == 0 {
        return false;
    }
    let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, bytes.len()) else {
        return false;
    };
    let ptr = GlobalLock(mem) as *mut u8;
    if ptr.is_null() {
        let _ = GlobalFree(Some(mem));
        return false;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
    let _ = GlobalUnlock(mem);
    // On success the clipboard owns the memory.
    if SetClipboardData(format, Some(HANDLE(mem.0))).is_err() {
        let _ = GlobalFree(Some(mem));
        return false;
    }
    true
}

/// A PNG as a bottom up 32 bit DIB, header and pixels, through WIC.
fn dib_of(png: &[u8]) -> Option<Vec<u8>> {
    unsafe {
        let wic: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let stream = SHCreateMemStream(Some(png))?;
        let decoder = wic
            .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
            .ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let bgra = WICConvertBitmapSource(&GUID_WICPixelFormat32bppBGRA, &frame).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        bgra.GetSize(&mut w, &mut h).ok()?;
        let stride = w as usize * 4;
        let mut pixels = vec![0u8; stride * h as usize];
        bgra.CopyPixels(std::ptr::null(), stride as u32, &mut pixels)
            .ok()?;
        let mut out = Vec::with_capacity(40 + pixels.len());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&(w as i32).to_le_bytes());
        out.extend_from_slice(&(h as i32).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        // BI_RGB, then the size of the pixels, then resolution and palette
        // left to the reader.
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(pixels.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 16]);
        for row in pixels.chunks_exact(stride).rev() {
            out.extend_from_slice(row);
        }
        Some(out)
    }
}

/// Asks the session to make a real deck of the HTML one with the pptx
/// skill, so the slides stay text and shapes rather than pictures.
fn to_pptx(key: &str, file: &Path) {
    let text = format!(
        "Make a PowerPoint deck of {} with the pptx skill: the same slides, \
         as real text and shapes rather than pictures, notes from each slide's \
         <aside class=\"notes\">. Save it beside it as {}.pptx.",
        file.display(),
        stem(file)
    );
    super::pins::ask(key, file, &text);
}
