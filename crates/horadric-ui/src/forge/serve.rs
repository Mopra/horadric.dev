//! How a file tab's page reaches it: its folder mapped read only to a made
//! up host, markdown and CSV rendered on the way, and the page side's
//! scripts put in before the page's own.
//!
//! Each folder gets a host of its own, `<name>-<hash>.forge.horadric`, so
//! two projects' `index.html` never share a cache entry or storage.
//!
//! Paths under `/__forge/` on every host are the Forge's own, not the
//! folder's: the artefact runtime and converted Office files.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use horadric_core::forge::Kind;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2, ICoreWebView2WebResourceRequestedEventArgs, ICoreWebView2_2, ICoreWebView2_3,
    COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_DENY, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
};
use webview2_com::{
    AddScriptToExecuteOnDocumentCreatedCompletedHandler, WebMessageReceivedEventHandler,
    WebResourceRequestedEventHandler,
};
use windows::core::{Interface, HSTRING, PWSTR};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::SHCreateMemStream;

/// The suffix every file tab's host ends in.
pub const SUFFIX: &str = ".forge.horadric";

const FORGE_JS: &str = include_str!("../../assets/forge/forge.js");
const FORGE_CSS: &str = include_str!("../../assets/forge/forge.css");
const DECK_JS: &str = include_str!("../../assets/forge/deck.js");
const DECK_CSS: &str = include_str!("../../assets/forge/deck.css");

/// The host the folder `root` is served at: its name, made safe for a
/// host, and a hash of its whole path so two folders of one name differ.
pub fn host_for(root: &Path) -> String {
    let full = root.to_string_lossy().replace('/', "\\").to_lowercase();
    let full = full.trim_end_matches('\\');
    let name: String = full
        .rsplit('\\')
        .next()
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(24)
        .collect();
    let name = name.trim_matches('-');
    let name = if name.is_empty() { "files" } else { name };
    // FNV-1a: stable across runs and builds, which a saved tab needs.
    let mut h: u32 = 0x811c_9dc5;
    for b in full.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{name}-{h:08x}{SUFFIX}")
}

/// The address of `file` in `root` served at `host`. None for a file
/// outside the folder.
pub fn url_for(host: &str, root: &Path, file: &Path) -> Option<String> {
    let r = root.to_string_lossy().replace('/', "\\");
    let f = file.to_string_lossy().replace('/', "\\");
    let r = r.trim_end_matches('\\');
    if !f.to_lowercase().starts_with(&r.to_lowercase()) {
        return None;
    }
    let rest = f.get(r.len()..).filter(|rest| rest.starts_with('\\'))?;
    let path: Vec<String> = rest
        .split('\\')
        .filter(|s| !s.is_empty())
        .map(encode)
        .collect();
    if path.is_empty() {
        return None;
    }
    Some(format!("https://{host}/{}", path.join("/")))
}

/// A path segment as an address may hold it.
fn encode(segment: &str) -> String {
    let mut out = String::new();
    for b in segment.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The host of an address, without its port.
pub fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let end = rest.find(['/', ':', '?', '#']).unwrap_or(rest.len());
    Some(&rest[..end])
}

/// The file an address of a file tab names inside `root`, refusing any
/// path that climbs out of it.
fn file_in(root: &Path, url: &str) -> Option<PathBuf> {
    let rest = url.split_once("://")?.1;
    let path = rest.find('/').map_or("", |at| &rest[at + 1..]);
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let mut file = root.to_path_buf();
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        let seg = decode(seg);
        if seg == ".." || seg == "." || seg.contains(['\\', '/', ':']) {
            return None;
        }
        file.push(seg);
    }
    Some(file)
}

/// The folder and file a file tab's address names, the folder found by
/// its host through `root_of`. None for any other address.
pub fn parse_url(url: &str, root_of: impl Fn(&str) -> Option<PathBuf>) -> Option<(PathBuf, PathBuf)> {
    if !url.starts_with("https://") {
        return None;
    }
    let host = host_of(url)?.to_ascii_lowercase();
    if !host.ends_with(SUFFIX) {
        return None;
    }
    let root = root_of(&host)?;
    let file = file_in(&root, url)?;
    Some((root, file))
}

/// Whether a page may load `file` itself, a stylesheet, script, picture or
/// font, so a change to it shows in the page that loads it.
pub fn is_subresource(file: &Path) -> bool {
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "css" | "js" | "mjs" | "json" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "avif"
            | "ico" | "woff" | "woff2" | "ttf" | "otf" | "mp4" | "webm"
    )
}

/// What runs first in every page of the tab: the page side of the Forge,
/// only on the tab's own host, so a link followed out to the web carries
/// none of it.
fn script(kind: Option<Kind>) -> String {
    // A converted .pptx is served as a deck of slide pictures.
    let deck = matches!(kind, Some(Kind::Html | Kind::Deck | Kind::Pptx));
    let assets = serde_json::json!({
        "forgeCss": FORGE_CSS,
        "deckCss": if deck { DECK_CSS } else { "" },
    });
    let mut s = format!(
        "(function(){{\nif(!location.hostname.toLowerCase().endsWith({suffix:?}))return;\n\
         window.__forgeAssets={assets};\n(function(){{\n{FORGE_JS}\n}})();\n",
        suffix = SUFFIX,
    );
    if deck {
        s.push_str(&format!("(function(){{\n{DECK_JS}\n}})();\n"));
    }
    s.push_str("})();");
    s
}

/// Readies the new page of a file tab: `root` mapped to `host` read only,
/// markdown and CSV rendered as they are asked for, the page side's
/// scripts, and the page's messages routed to the Forge.
pub fn prepare(key: &str, id: u64, view: &ICoreWebView2, root: &Path, host: &str, kind: Option<Kind>) {
    unsafe {
        match view.cast::<ICoreWebView2_3>() {
            Ok(v) => {
                if let Err(e) = v.SetVirtualHostNameToFolderMapping(
                    &HSTRING::from(host),
                    &HSTRING::from(root.as_os_str()),
                    COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_DENY,
                ) {
                    eprintln!("horadric: cannot serve {} to the Forge: {e}", root.display());
                }
            }
            Err(e) => eprintln!("horadric: this WebView2 cannot serve files: {e}"),
        }
        let mut patterns = vec![format!("https://{host}{RESERVED}*")];
        for ext in RENDERED {
            patterns.push(format!("https://{host}/*.{ext}"));
            patterns.push(format!("https://{host}/*.{ext}?*"));
        }
        for pattern in patterns {
            let _ = view.AddWebResourceRequestedFilter(
                &HSTRING::from(pattern),
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
            );
        }
        let served = root.to_path_buf();
        let rendered = WebResourceRequestedEventHandler::create(Box::new(move |view, args| {
            if let (Some(view), Some(args)) = (view, args) {
                render(&view, &args, &served);
            }
            Ok(())
        }));
        let mut token = Default::default();
        let _ = view.add_WebResourceRequested(&rendered, &mut token);
        super::edit::serve(view, host, root);

        let owned = key.to_string();
        let said = WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
            if let Some(args) = args {
                let source = read(|p| args.Source(p));
                let json = read(|p| args.WebMessageAsJson(p));
                super::heard_page(&owned, id, &source, &json);
            }
            Ok(())
        }));
        let _ = view.add_WebMessageReceived(&said, &mut token);

        let added =
            AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(|_, _| Ok(())));
        let _ = view.AddScriptToExecuteOnDocumentCreated(&HSTRING::from(script(kind)), &added);
    }
}

/// The path prefix of the Forge's own files on every host.
const RESERVED: &str = "/__forge/";

/// The extensions answered by [`answer`] rather than read from the folder
/// as they are.
const RENDERED: [&str; 9] = [
    "md", "markdown", "csv", "jsx", "tsx", "mmd", "mermaid", "pptx", "docx",
];

/// What a request is answered with.
pub struct Reply {
    pub status: u16,
    pub body: Cow<'static, [u8]>,
    pub content_type: &'static str,
    /// Kept a year: only for what never changes at its address.
    pub cache: bool,
}

impl Reply {
    pub fn ok(body: Cow<'static, [u8]>, content_type: &'static str) -> Reply {
        Reply {
            status: 200,
            body,
            content_type,
            cache: false,
        }
    }

    pub fn html(page: String) -> Reply {
        Reply::ok(page.into_bytes().into(), "text/html; charset=utf-8")
    }

    pub fn status(mut self, status: u16) -> Reply {
        self.status = status;
        self
    }

    pub fn cached(mut self) -> Reply {
        self.cache = true;
        self
    }

    fn headers(&self) -> String {
        let cache = if self.cache {
            "public, max-age=31536000, immutable"
        } else {
            "no-store"
        };
        format!("Content-Type: {}\r\nCache-Control: {cache}", self.content_type)
    }
}

/// The path and query of an address, the path still percent encoded.
fn path_query(url: &str) -> (&str, &str) {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.find('/').map_or("/", |at| &rest[at..]);
    let rest = rest.split('#').next().unwrap_or_default();
    rest.split_once('?').unwrap_or((rest, ""))
}

/// The answer to `url` on the host serving `root`, None to let the folder
/// answer as it would.
pub fn answer(root: &Path, url: &str) -> Option<Reply> {
    let (path, query) = path_query(url);
    if let Some(rest) = path.strip_prefix(RESERVED) {
        let found = if let Some(r) = rest.strip_prefix("runtime/") {
            super::runtime::asset(r)
        } else if let Some(r) = rest.strip_prefix("office/") {
            super::office::asset(r)
        } else {
            None
        };
        return Some(found.unwrap_or_else(|| {
            Reply::html("<!doctype html><title>Not found</title>".into()).status(404)
        }));
    }
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    // Other files are the folder's, and an HTML file would be read here
    // only to learn whether it is a deck.
    if !ext.is_some_and(|e| RENDERED.contains(&e.as_str())) {
        return None;
    }
    let file = file_in(root, url)?;
    let title = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let kind = horadric_core::forge::kind_of(&file)?;
    if matches!(kind, Kind::Pptx | Kind::Docx) {
        return Some(super::office::page(root, &file));
    }
    let bytes = match std::fs::read(&file) {
        Ok(b) => b,
        Err(e) => {
            let page = format!(
                "<!doctype html><title>{t}</title><p>{t} cannot be read: {e}</p>",
                t = super::runtime::attr(&title)
            );
            return Some(Reply::html(page).status(404));
        }
    };
    let src = String::from_utf8_lossy(&bytes);
    Some(match kind {
        Kind::Markdown => Reply::html(super::runtime::with_mermaid(
            horadric_core::forge::markdown_html(&src, &title),
        )),
        Kind::Csv => Reply::html(horadric_core::forge::csv_html(&src, &title)),
        // The artefact page fetches its source with ?raw. Answered here too
        // rather than left to the folder, so nothing rests on how a filtered
        // request with no response set falls through.
        Kind::Jsx | Kind::Mermaid if query.split('&').any(|q| q == "raw") => {
            Reply::ok(bytes.into(), "text/plain; charset=utf-8")
        }
        Kind::Jsx | Kind::Mermaid => Reply::html(super::runtime::artefact_page(path)),
        _ => return None,
    })
}

/// Answers a request [`answer`] has a reply for. Left alone, a request
/// gets the file's bytes as they are.
fn render(view: &ICoreWebView2, args: &ICoreWebView2WebResourceRequestedEventArgs, root: &Path) {
    let Ok(request) = (unsafe { args.Request() }) else {
        return;
    };
    let url = read(|p| unsafe { request.Uri(p) });
    let Some(reply) = answer(root, &url) else {
        return;
    };
    let reason = if reply.status == 200 { "OK" } else { "Not Found" };
    unsafe {
        let Ok(env) = view.cast::<ICoreWebView2_2>().and_then(|v| v.Environment()) else {
            return;
        };
        let Some(stream) = SHCreateMemStream(Some(&reply.body)) else {
            return;
        };
        if let Ok(response) = env.CreateWebResourceResponse(
            &stream,
            i32::from(reply.status),
            &HSTRING::from(reason),
            &HSTRING::from(reply.headers()),
        ) {
            let _ = args.SetResponse(&response);
        }
    }
}

/// A string WebView2 hands out, freed after it is copied.
fn read(get: impl FnOnce(*mut PWSTR) -> windows::core::Result<()>) -> String {
    let mut p = PWSTR::null();
    if get(&mut p).is_err() || p.is_null() {
        return String::new();
    }
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_served_at_a_host_of_its_own() {
        let a = host_for(Path::new(r"C:\Work\Deck Studio"));
        let b = host_for(Path::new(r"C:\Other\Deck Studio"));
        assert!(a.starts_with("deck-studio-") && a.ends_with(SUFFIX));
        assert_ne!(a, b);
        assert_eq!(a, host_for(Path::new("c:/work/deck studio/")));
    }

    #[test]
    fn a_file_goes_there_and_back() {
        let root = Path::new(r"C:\Work\site");
        let file = Path::new(r"C:\Work\site\docs\my deck #1.html");
        let host = host_for(root);
        let url = url_for(&host, root, file).unwrap();
        assert!(url.ends_with("/docs/my%20deck%20%231.html"));
        let (r, f) = parse_url(&url, |h| (h == host).then(|| root.to_path_buf())).unwrap();
        assert_eq!(r, root);
        assert_eq!(f, file);
    }

    #[test]
    fn artefacts_get_the_runtime_page_and_their_source_raw() {
        let dir = std::env::temp_dir().join(format!("forge-serve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("chart.jsx"), "export default () => <p>hi</p>").unwrap();
        let host = "x.forge.horadric";
        let page = answer(&dir, &format!("https://{host}/chart.jsx")).unwrap();
        assert!(page.content_type.starts_with("text/html"));
        let page = String::from_utf8_lossy(&page.body).into_owned();
        assert!(page.contains("content=\"/chart.jsx?raw\""));
        let raw = answer(&dir, &format!("https://{host}/chart.jsx?raw")).unwrap();
        assert_eq!(&*raw.body, b"export default () => <p>hi</p>");
        assert!(!raw.cache);
        let base = super::super::runtime::base();
        let js = answer(&dir, &format!("https://{host}{base}runtime.js"));
        assert!(js.is_some_and(|r| r.cache && r.status == 200));
        assert_eq!(answer(&dir, &format!("https://{host}/__forge/nope")).unwrap().status, 404);
        // Anything else is the folder's to serve as it is.
        assert!(answer(&dir, &format!("https://{host}/page.html")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_climbs_out_of_the_folder() {
        let root = Path::new(r"C:\Work\site");
        assert!(file_in(root, "https://x.forge.horadric/a/%2E%2E/b").is_none());
        assert!(file_in(root, "https://x.forge.horadric/..%5Csecret").is_none());
        assert!(url_for("x", root, Path::new(r"C:\Work\other\a.md")).is_none());
    }
}
