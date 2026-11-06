//! How a file tab's page reaches it: its folder mapped read only to a made
//! up host, markdown and CSV rendered on the way, and the page side's
//! scripts put in before the page's own.
//!
//! Each folder gets a host of its own, `<name>-<hash>.forge.horadric`, so
//! two projects' `index.html` never share a cache entry or storage.
//!
//! Paths under `/__forge/` on every host are the Forge's own, not the
//! folder's: the artefact runtime and converted Office files.
//!
//! A file outside any project is served from its own folder at a flat
//! host, which answers nothing in that folder's subfolders.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use horadric_core::forge::Kind;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2, ICoreWebView2Deferral, ICoreWebView2Environment,
    ICoreWebView2WebResourceRequestedEventArgs, ICoreWebView2_2,
    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT,
};
use webview2_com::{
    AddScriptToExecuteOnDocumentCreatedCompletedHandler, WebMessageReceivedEventHandler,
    WebResourceRequestedEventHandler,
};
use windows::core::{Interface, HSTRING, PWSTR};
use windows::Win32::System::Com::{CoTaskMemFree, STGM_READ, STGM_SHARE_DENY_NONE};
use windows::Win32::UI::Shell::{SHCreateMemStream, SHCreateStreamOnFileEx};

use crate::theme::{self, Color};

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

/// The host the folder `root` is served at when only its own files are
/// served, not its subfolders, apart from [`host_for`] so the two never
/// share a cache, and told apart by its ending.
pub fn host_for_flat(root: &Path) -> String {
    let host = host_for(root);
    format!("{}{FLAT}{SUFFIX}", host.trim_end_matches(SUFFIX))
}

/// What a flat host ends in before [`SUFFIX`]. A deep host ends in eight
/// hex digits, so never in this.
const FLAT: &str = "-f";

/// Whether `host` serves only its folder's own files.
pub fn is_flat(host: &str) -> bool {
    host.to_ascii_lowercase()
        .strip_suffix(SUFFIX)
        .is_some_and(|h| h.ends_with(FLAT))
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
pub(super) fn encode(segment: &str) -> String {
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
/// none of it, and never on a folder's page (its address ends in a
/// slash), which is the Forge's own and has nothing to pin.
///
/// Every page gets the deck shell, whatever the tab opened on: a tab
/// follows links and the folder pages to any file, and the shell does
/// nothing on a page with no slides.
///
/// The chrome is drawn in Horadric's look as it was when the tab was made:
/// its colours go in front of each stylesheet, on the shadow host and on
/// a deck's root, where only the Forge's `--hd-` names are set.
fn script(accent: Color) -> String {
    let look = super::home::vars(accent);
    let assets = serde_json::json!({
        "forgeCss": format!(":host {{ {look} }}
{FORGE_CSS}"),
        "deckCss": format!(":host, html.forge-deck {{ {look} }}
{DECK_CSS}"),
    });
    format!(
        "(function(){{\nif(!location.hostname.toLowerCase().endsWith({suffix:?}))return;\n\
         if(location.pathname.endsWith('/'))return;\n\
         window.__forgeAssets={assets};\n(function(){{\n{FORGE_JS}\n}})();\n\
         (function(){{\n{DECK_JS}\n}})();\n}})();",
        suffix = SUFFIX,
    )
}

/// Readies the new page of a file tab: `root` mapped to `host` read only,
/// markdown and CSV rendered as they are asked for, the page side's
/// scripts, and the page's messages routed to the Forge.
pub fn prepare(key: &str, id: u64, view: &ICoreWebView2, root: &Path, host: &str) {
    let flat = is_flat(host);
    unsafe {
        // No folder mapping: WebView2 answers a mapped host's requests
        // itself and never raises WebResourceRequested for them, so
        // markdown came through as plain text. Every request on the host
        // is answered by `render` instead, from the folder.
        for pattern in [format!("https://{host}/*")] {
            let _ = view.AddWebResourceRequestedFilter(
                &HSTRING::from(pattern),
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
            );
        }
        // Before ours: handlers run in the order they were added, so a
        // refusal set by ours is the one that stands.
        super::edit::serve(view, host, root);
        let served = root.to_path_buf();
        let project = key.to_string();
        let rendered = WebResourceRequestedEventHandler::create(Box::new(move |view, args| {
            if let (Some(view), Some(args)) = (view, args) {
                // Read here, on the UI thread, where the colours projects
                // were given live, and per request, so a new colour shows
                // on the next page.
                render(&view, &args, &served, flat, theme::accent(&project));
            }
            Ok(())
        }));
        let mut token = Default::default();
        let _ = view.add_WebResourceRequested(&rendered, &mut token);

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
        let _ = view.AddScriptToExecuteOnDocumentCreated(&HSTRING::from(script(theme::accent(key))), &added);
    }
}

/// The path prefix of the Forge's own files on every host.
const RESERVED: &str = "/__forge/";

/// The extensions answered by [`answer`] rather than read from the folder
/// as they are.
const RENDERED: [&str; 9] = [
    "md", "markdown", "csv", "jsx", "tsx", "mmd", "mermaid", "pptx", "docx",
];

/// The largest markdown or CSV file rendered. Rendering runs off the UI
/// thread, but a page of hundreds of megabytes would still stall the view.
const LARGEST: u64 = 20 << 20;

/// The most CSV rows put in the table; a sortable table of more is too
/// slow to be of use.
const MOST_ROWS: usize = 10_000;

/// What a reply carries.
pub enum Body {
    Bytes(Cow<'static, [u8]>),
    /// A file streamed as WebView2 reads it, never read whole into memory.
    File(PathBuf),
}

/// What a request is answered with.
pub struct Reply {
    pub status: u16,
    pub body: Body,
    pub content_type: &'static str,
    /// Kept a year: only for what never changes at its address.
    pub cache: bool,
}

impl Reply {
    pub fn ok(body: Cow<'static, [u8]>, content_type: &'static str) -> Reply {
        Reply {
            status: 200,
            body: Body::Bytes(body),
            content_type,
            cache: false,
        }
    }

    pub fn file(file: PathBuf, content_type: &'static str) -> Reply {
        Reply {
            status: 200,
            body: Body::File(file),
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

fn not_found() -> Reply {
    Reply::html("<!doctype html><title>Not found</title>".into()).status(404)
}

/// The path and query of an address, the path still percent encoded.
fn path_query(url: &str) -> (&str, &str) {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.find('/').map_or("/", |at| &rest[at..]);
    let rest = rest.split('#').next().unwrap_or_default();
    rest.split_once('?').unwrap_or((rest, ""))
}

/// How a request is to be answered.
enum Plan {
    Now(Reply),
    /// Made from the disk (a rendered file, a folder's listing): done off
    /// the UI thread.
    Later(Box<dyn FnOnce() -> Reply + Send>),
}

/// What the request for `url` on the host serving `root` comes to, None
/// to let the folder answer as it would. Only a `document` request (a
/// page, or a frame's) is given a rendered page; a script's `fetch` of a
/// CSV gets the CSV. A `flat` host serves none of its folder's subfolders.
fn plan(root: &Path, url: &str, document: bool, flat: bool, accent: Color) -> Option<Plan> {
    let (path, query) = path_query(url);
    if let Some(rest) = path.strip_prefix(RESERVED) {
        let found = if let Some(r) = rest.strip_prefix("runtime/") {
            super::runtime::asset(r)
        } else if let Some(r) = rest.strip_prefix("office/") {
            super::office::asset(r)
        } else {
            None
        };
        return Some(Plan::Now(found.unwrap_or_else(not_found)));
    }
    if flat && decode(path).trim_start_matches('/').contains(['/', '\\']) {
        return Some(Plan::Now(not_found()));
    }
    let Some(file) = file_in(root, url) else {
        return Some(Plan::Now(not_found()));
    };
    // A folder's page is the Forge's home. Its address ends in a slash so
    // the names it lists are links relative to it.
    if document && (path.ends_with('/') || file.is_dir()) {
        if !path.ends_with('/') {
            let to = format!("{path}/");
            return Some(Plan::Now(Reply::html(format!(
                "<!doctype html><meta http-equiv=\"refresh\" content=\"0;url={}\">",
                super::runtime::attr(&to)
            ))));
        }
        let (root, written) = (root.to_path_buf(), super::recent_written());
        return Some(Plan::Later(Box::new(move || {
            super::home::page(&root, &file, flat, &written, accent)
        })));
    }
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    if !ext.as_deref().is_some_and(|e| RENDERED.contains(&e)) {
        // A page is left to edit.rs's handler, which serves it marked
        // with where each element's text sits; it runs first and passes
        // on pages too large to mark.
        let page = matches!(ext.as_deref(), Some("html" | "htm"));
        let markable = std::fs::metadata(&file).is_ok_and(|m| m.len() <= super::edit::MARK_MAX as u64);
        if document && page && markable {
            return None;
        }
        if !file.is_file() {
            return Some(Plan::Now(not_found()));
        }
        return Some(Plan::Now(Reply::file(
            file.clone(),
            super::history::mime_of(&file),
        )));
    }
    let kind = horadric_core::forge::kind_of(&file)?;
    let raw = query.split('&').any(|q| q == "raw");
    if raw || !document {
        // Answered here rather than left to the folder, so nothing rests
        // on how a filtered request with no response set falls through.
        return Some(Plan::Now(Reply::file(file, raw_type(kind, raw))));
    }
    if matches!(kind, Kind::Pptx | Kind::Docx) {
        return Some(Plan::Now(super::office::page(root, &file, &look(accent))));
    }
    if matches!(kind, Kind::Jsx | Kind::Mermaid) {
        let page = super::runtime::artefact_page(path);
        return Some(Plan::Now(Reply::html(styled(page, &look(accent)))));
    }
    let title = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Read here, on the UI thread, where the theme and the project's
    // colour live; the worker only renders.
    let look = look(accent);
    Some(Plan::Later(Box::new(move || rendered(kind, &file, &title, &look))))
}

/// The type a file of `kind` is served as when it is not rendered. The
/// artefact page asks with `?raw` for its source as text.
fn raw_type(kind: Kind, raw: bool) -> &'static str {
    match kind {
        _ if raw => "text/plain; charset=utf-8",
        Kind::Markdown => "text/markdown; charset=utf-8",
        Kind::Csv => "text/csv; charset=utf-8",
        Kind::Jsx => "text/javascript; charset=utf-8",
        Kind::Pptx => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        Kind::Docx => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        _ => "text/plain; charset=utf-8",
    }
}

/// The page for a markdown or CSV `file`, in the theme and project
/// colour `look` gives. Reads the file, so it is for a worker thread.
fn rendered(kind: Kind, file: &Path, title: &str, look: &str) -> Reply {
    let read = std::fs::metadata(file).and_then(|meta| {
        if meta.len() > LARGEST {
            return Ok(None);
        }
        std::fs::read(file).map(Some)
    });
    let bytes = match read {
        Ok(Some(b)) => b,
        Ok(None) => {
            let text = format!("Too large to show here, over {} MB.", LARGEST >> 20);
            return Reply::html(message(title, &text, Mood::Quiet, look));
        }
        Err(e) => {
            let text = format!("Cannot be read: {e}");
            return Reply::html(message(title, &text, Mood::Trouble, look)).status(404);
        }
    };
    let src = String::from_utf8_lossy(&bytes);
    match kind {
        Kind::Markdown => Reply::html(styled(
            super::runtime::with_mermaid(horadric_core::forge::markdown_html(&src, title)),
            look,
        )),
        Kind::Csv => {
            let (src, cut) = first_rows(&src, MOST_ROWS);
            let mut page = horadric_core::forge::csv_html(src, title);
            if cut {
                let note = format!(
                    "<p class=\"forge-cut\">Only the first {MOST_ROWS} rows are shown.</p>\n</body>"
                );
                page = page.replacen("</body>", &note, 1);
            }
            Reply::html(styled(page, look))
        }
        _ => not_found(),
    }
}

/// Horadric's look as `--hd-` properties: the theme's colours and the
/// project's, with the Folio's ink for code, which only the pages read.
/// On the UI thread, where both live.
pub(super) fn look(accent: Color) -> String {
    let p = theme::palette();
    let read = p.term_fg.mix(p.term_bg, 0.09);
    let code = Color::rgb(0xE2C08D).mix(read, 0.25);
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "{} --hd-code: #{:02x}{:02x}{:02x};",
        super::home::vars(accent),
        b(code.r),
        b(code.g),
        b(code.b)
    )
}

/// `page` with `look` set after its own stylesheet, so the theme in use
/// wins over the default theme the page was made with.
pub(super) fn styled(page: String, look: &str) -> String {
    let style = format!("<style>:root {{ {look} }}</style>\n</head>");
    page.replacen("</head>", &style, 1)
}

/// How a page with one thing to say says it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mood {
    /// Work going on: runes kindling in turn, in the project's colour.
    Waiting,
    /// Nothing wrong, only nothing to show: one unlit rune.
    Quiet,
    /// Something failed: one rune in the error colour.
    Trouble,
}

/// A page saying one thing about the file `name`: its name engraved on
/// the plate over a small screen with the words on it, as the Forge's home
/// draws its listings. While `Waiting` it also asks again every few
/// seconds, in case the reload that ends the wait is missed.
pub(super) fn message(name: &str, text: &str, mood: Mood, look: &str) -> String {
    let (runes, class): (&[usize], &str) = match mood {
        Mood::Waiting => (&[3, 7, 10], "waiting"),
        Mood::Quiet => (&[9], "quiet"),
        Mood::Trouble => (&[2], "trouble"),
    };
    let refresh = if mood == Mood::Waiting {
        "<meta http-equiv=\"refresh\" content=\"3\">"
    } else {
        ""
    };
    let marks: String = runes.iter().map(|&i| rune_svg(i)).collect();
    let n = super::runtime::attr(name);
    styled(
        format!(
            "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">{refresh}\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
             <title>{n}</title>\n<style>{}{MESSAGE_STYLE}</style></head>\n\
             <body><div class=\"say {class}\"><div class=\"hd-legend name\">{n}</div>\
             <div class=\"hd-screen card\"><span class=\"runes\">{marks}</span><p>{}</p></div></div>\
             </body></html>\n",
            horadric_core::forge::LOOK,
            super::runtime::attr(text)
        ),
        look,
    )
}

/// One of the Folio's runes as a stroked mark, drawn in the text's colour.
pub(super) fn rune_svg(i: usize) -> String {
    let runes = &crate::sparks::RUNES;
    let mut d = String::new();
    for line in runes[i % runes.len()].iter() {
        for (j, (x, y)) in line.iter().enumerate() {
            d.push_str(&format!("{}{x} {y} ", if j == 0 { "M" } else { "L" }));
        }
    }
    format!(
        "<svg class=\"rune\" viewBox=\"-0.35 -0.08 0.7 1.16\" aria-hidden=\"true\">\
         <path vector-effect=\"non-scaling-stroke\" d=\"{}\"/></svg>",
        d.trim_end()
    )
}

/// A message page: its card in the middle of the plate.
const MESSAGE_STYLE: &str = r#"
body { display: flex; align-items: center; justify-content: center; padding: 24px; }
.say { width: min(460px, 100%); display: flex; flex-direction: column; gap: 10px; }
.say .name { padding: 0 4px; overflow-wrap: anywhere; }
.card { display: flex; flex-direction: column; align-items: center; gap: 14px;
  padding: 30px 24px 26px; text-align: center; }
.card p { margin: 0; font-size: 14px; line-height: 1.55; color: var(--hd-read); overflow-wrap: anywhere; }
.runes { display: inline-flex; gap: 6px; }
svg.rune { width: 13px; height: 22px; fill: none; stroke: currentColor; stroke-width: 1.9px;
  stroke-linecap: round; stroke-linejoin: round; overflow: visible; }
.quiet .runes { color: var(--hd-lamp-off); }
.trouble .runes { color: var(--hd-error); }
.waiting .runes { color: var(--hd-accent); }
.waiting svg.rune { animation: kindle 1.2s ease-in-out infinite; opacity: 0.25; }
.waiting svg.rune:nth-child(2) { animation-delay: 0.2s; }
.waiting svg.rune:nth-child(3) { animation-delay: 0.4s; }
@keyframes kindle { 40% { opacity: 1; } }
@media (prefers-reduced-motion: reduce) { .waiting svg.rune { animation: none; opacity: 1; } }
"#;

/// The first `rows` lines of `src` (the header among them), and whether
/// any were left out.
fn first_rows(src: &str, rows: usize) -> (&str, bool) {
    match src.match_indices('\n').nth(rows.saturating_sub(1)) {
        Some((at, _)) if at + 1 < src.len() => (&src[..at + 1], true),
        _ => (src, false),
    }
}

/// The answer to `url` on the host serving `root`, None to let the folder
/// answer as it would. Reads the file on the calling thread.
#[cfg(test)]
fn answer(root: &Path, url: &str, document: bool, flat: bool) -> Option<Reply> {
    Some(match plan(root, url, document, flat, crate::theme::ACCENTS[0].0)? {
        Plan::Now(reply) => reply,
        Plan::Later(make) => make(),
    })
}

/// A request waiting on a worker thread, with what is needed to answer it
/// on the UI thread.
struct Waiting {
    args: ICoreWebView2WebResourceRequestedEventArgs,
    deferral: ICoreWebView2Deferral,
    env: ICoreWebView2Environment,
}

thread_local! {
    static WAITING: RefCell<HashMap<u64, Waiting>> = RefCell::new(HashMap::new());
}

/// Pages workers rendered, by the number of the request they answer.
static RENDERED_PAGES: Mutex<Vec<(u64, Reply)>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(1);

/// Answers a request [`plan`] has a reply for. Left alone, a request
/// gets the file's bytes as they are.
fn render(
    view: &ICoreWebView2,
    args: &ICoreWebView2WebResourceRequestedEventArgs,
    root: &Path,
    flat: bool,
    accent: Color,
) {
    let Ok(request) = (unsafe { args.Request() }) else {
        return;
    };
    let url = read(|p| unsafe { request.Uri(p) });
    let mut context = COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL;
    let _ = unsafe { args.ResourceContext(&mut context) };
    let document = context == COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT;
    let Some(plan) = plan(root, &url, document, flat, accent) else {
        return;
    };
    let Ok(env) = (unsafe { view.cast::<ICoreWebView2_2>().and_then(|v| v.Environment()) }) else {
        return;
    };
    match plan {
        Plan::Now(reply) => respond(&env, args, &reply),
        Plan::Later(make) => {
            let Ok(deferral) = (unsafe { args.GetDeferral() }) else {
                return respond(&env, args, &make());
            };
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            WAITING.with(|w| {
                w.borrow_mut().insert(
                    n,
                    Waiting {
                        args: args.clone(),
                        deferral,
                        env,
                    },
                )
            });
            let started = std::thread::Builder::new()
                .name("horadric-forge-render".into())
                .spawn(move || {
                    let reply = make();
                    if let Ok(mut done) = RENDERED_PAGES.lock() {
                        done.push((n, reply));
                    }
                    super::wake();
                });
            if started.is_err() {
                if let Some(w) = WAITING.with(|w| w.borrow_mut().remove(&n)) {
                    respond(&w.env, &w.args, &not_found());
                    let _ = unsafe { w.deferral.Complete() };
                }
            }
        }
    }
}

/// On the UI thread: answers the requests whose pages the workers have
/// finished.
pub fn drain() {
    let done = RENDERED_PAGES
        .lock()
        .map(|mut d| std::mem::take(&mut *d))
        .unwrap_or_default();
    for (n, reply) in done {
        if let Some(w) = WAITING.with(|w| w.borrow_mut().remove(&n)) {
            respond(&w.env, &w.args, &reply);
            let _ = unsafe { w.deferral.Complete() };
        }
    }
}

fn respond(env: &ICoreWebView2Environment, args: &ICoreWebView2WebResourceRequestedEventArgs, reply: &Reply) {
    let stream = match &reply.body {
        Body::Bytes(b) => unsafe { SHCreateMemStream(Some(b)) },
        Body::File(file) => unsafe {
            SHCreateStreamOnFileEx(
                &HSTRING::from(file.as_os_str()),
                (STGM_READ | STGM_SHARE_DENY_NONE).0,
                0,
                false,
                None,
            )
            .ok()
        },
    };
    let (stream, status, headers) = match stream {
        Some(s) => (s, reply.status, reply.headers()),
        None => {
            let missing = not_found();
            let Body::Bytes(b) = &missing.body else { return };
            let Some(s) = (unsafe { SHCreateMemStream(Some(b)) }) else {
                return;
            };
            (s, missing.status, missing.headers())
        }
    };
    let reason = if status == 200 { "OK" } else { "Not Found" };
    unsafe {
        if let Ok(response) = env.CreateWebResourceResponse(
            &stream,
            i32::from(status),
            &HSTRING::from(reason),
            &HSTRING::from(headers),
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

    fn bytes(r: &Reply) -> &[u8] {
        match &r.body {
            Body::Bytes(b) => b,
            Body::File(_) => panic!("a file, not bytes"),
        }
    }

    #[test]
    fn artefacts_get_the_runtime_page_and_their_source_raw() {
        let dir = std::env::temp_dir().join(format!("forge-serve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("chart.jsx"), "export default () => <p>hi</p>").unwrap();
        let host = "x.forge.horadric";
        let page = answer(&dir, &format!("https://{host}/chart.jsx"), true, false).unwrap();
        assert!(page.content_type.starts_with("text/html"));
        let page = String::from_utf8_lossy(bytes(&page)).into_owned();
        assert!(page.contains("content=\"/chart.jsx?raw\""));
        let raw = answer(&dir, &format!("https://{host}/chart.jsx?raw"), false, false).unwrap();
        assert!(matches!(&raw.body, Body::File(f) if f == &dir.join("chart.jsx")));
        assert!(raw.content_type.starts_with("text/plain") && !raw.cache);
        let base = super::super::runtime::base();
        let js = answer(&dir, &format!("https://{host}{base}runtime.js"), false, false);
        assert!(js.is_some_and(|r| r.cache && r.status == 200));
        let nope = answer(&dir, &format!("https://{host}/__forge/nope"), true, false);
        assert_eq!(nope.unwrap().status, 404);
        // A page is left to edit.rs's marking handler; any other file is
        // read from the folder as it is, and a missing one is a 404.
        std::fs::write(dir.join("page.html"), "<p>hi</p>").unwrap();
        assert!(answer(&dir, &format!("https://{host}/page.html"), true, false).is_none());
        std::fs::write(dir.join("logo.svg"), "<svg/>").unwrap();
        let svg = answer(&dir, &format!("https://{host}/logo.svg"), false, false).unwrap();
        assert!(svg.content_type == "image/svg+xml" && matches!(svg.body, Body::File(_)));
        let gone = answer(&dir, &format!("https://{host}/gone.png"), false, false).unwrap();
        assert_eq!(gone.status, 404);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_page_gets_the_rendered_csv() {
        let dir = std::env::temp_dir().join(format!("forge-serve-csv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("data.csv"), "a,b
1,2
").unwrap();
        let url = "https://x.forge.horadric/data.csv";
        let page = answer(&dir, url, true, false).unwrap();
        assert!(page.content_type.starts_with("text/html"));
        let fetched = answer(&dir, url, false, false).unwrap();
        assert!(fetched.content_type.starts_with("text/csv"));
        assert!(matches!(fetched.body, Body::File(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_flat_host_serves_no_subfolder() {
        let root = Path::new(r"C:/Users/me/Desktop");
        let host = host_for_flat(root);
        assert!(is_flat(&host) && !is_flat(&host_for(root)));
        let deep = plan(root, &format!("https://{host}/.ssh/id_rsa"), false, true, crate::theme::ACCENTS[0].0);
        assert!(matches!(deep, Some(Plan::Now(r)) if r.status == 404));
        let hidden = plan(root, &format!("https://{host}/a%2Fb.png"), false, true, crate::theme::ACCENTS[0].0);
        assert!(matches!(hidden, Some(Plan::Now(r)) if r.status == 404));
        // A file at the top passes the subfolder check; this one is missing.
        let top = plan(root, &format!("https://{host}/notes.png"), false, true, crate::theme::ACCENTS[0].0);
        assert!(matches!(top, Some(Plan::Now(r)) if r.status == 404));
    }

    #[test]
    fn a_folder_s_page_is_the_forge_s_home() {
        let dir = std::env::temp_dir().join(format!("forge-serve-dir-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("docs").join("plan.md"), "# Plan").unwrap();
        let host = "https://x.forge.horadric";
        let home = answer(&dir, &format!("{host}/docs/"), true, false).unwrap();
        assert_eq!(home.status, 200);
        assert!(String::from_utf8_lossy(bytes(&home)).contains("href=\"plan.md\""));
        // Without its slash, the page's links would miss the folder.
        let bare = answer(&dir, &format!("{host}/docs"), true, false).unwrap();
        assert!(String::from_utf8_lossy(bytes(&bare)).contains("url=/docs/"));
        let fetched = answer(&dir, &format!("{host}/docs/"), false, false).unwrap();
        assert_eq!(fetched.status, 404);
        let climbed = answer(&dir, &format!("{host}/docs/%2E%2E/"), true, false).unwrap();
        assert_eq!(climbed.status, 404);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_long_csv_is_cut_to_its_first_rows() {
        assert_eq!(first_rows("h
1
2
", 2), ("h
1
", true));
        assert_eq!(first_rows("h
1
", 2), ("h
1
", false));
        assert_eq!(first_rows("h
1", 5), ("h
1", false));
    }

    #[test]
    fn nothing_climbs_out_of_the_folder() {
        let root = Path::new(r"C:\Work\site");
        assert!(file_in(root, "https://x.forge.horadric/a/%2E%2E/b").is_none());
        assert!(file_in(root, "https://x.forge.horadric/..%5Csecret").is_none());
        assert!(url_for("x", root, Path::new(r"C:\Work\other\a.md")).is_none());
    }

    /// Writes every page the Forge makes for a file to `FORGE_SAMPLES`,
    /// with the home page beside them, to open in the browser pane and see
    /// that they read as one family.
    #[test]
    #[ignore]
    fn write_sample_pages() {
        let Ok(out) = std::env::var("FORGE_SAMPLES") else { return };
        let out = PathBuf::from(out);
        let src = out.join("src");
        std::fs::create_dir_all(src.join("notes")).unwrap();
        let md = "# Release plan\n\nThe Forge shows **every page** in Horadric's look, with `inline code`, \
a [link](https://horadric.dev) and ~~old words~~.\n\n## Steps\n\n1. Read the plan\n2. Write the code\n   - a nested bullet\n   - another\n3. Ship\n\n\
- [x] Markdown restyled\n- [ ] CSV restyled\n\n> A quote from the human: the warm theme has to go.\n\n\
### Code\n\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\n#### Small heading\n\n\
| Name | Count | State |\n|:-----|------:|-------|\n| Tiles | 40 | done |\n| Windows | 1 | open |\n\n---\n\n\
<details><summary>More</summary>\n\nHidden words.\n\n</details>\n\n```mermaid\ngraph LR\n  A[Hook] --> B(State)\n  B --> C{Tile}\n```\n";
        std::fs::write(src.join("plan.md"), md).unwrap();
        std::fs::write(
            src.join("table.csv"),
            "Project,Sessions,Tokens,Note\nhoradric,4,\"12 400\",the app\nsite,1,830,\"web, small\"\nlore,2,\"3,5\",memory\n",
        )
        .unwrap();
        std::fs::write(src.join("notes").join("a.md"), "# a").unwrap();
        let accent = crate::theme::ACCENTS[2].0;
        let look = look(accent);
        let mermaid = out.join("mermaid.js");
        let _ = std::fs::write(&mermaid, include_bytes!("../../assets/forge/runtime/mermaid.js"));
        let local = |page: String| {
            page.replace(
                &format!("{}mermaid.js", super::super::runtime::base()),
                "mermaid.js",
            )
        };
        let text = |r: Reply| String::from_utf8_lossy(bytes(&r)).into_owned();
        let pages = [
            ("md.html", local(text(rendered(Kind::Markdown, &src.join("plan.md"), "plan.md", &look)))),
            ("csv.html", text(rendered(Kind::Csv, &src.join("table.csv"), "table.csv", &look))),
            ("missing.html", text(rendered(Kind::Csv, &src.join("gone.csv"), "gone.csv", &look))),
            ("waiting.html", message("Q3 review.pptx", "Converting with PowerPoint", Mood::Waiting, &look)),
            ("large.html", message("huge.csv", "Too large to show here, over 20 MB.", Mood::Quiet, &look)),
            ("home.html", text(super::super::home::page(&src, &src, false, &[], accent))),
        ];
        for (name, page) in pages {
            std::fs::write(out.join(name), page).unwrap();
        }
    }
}
