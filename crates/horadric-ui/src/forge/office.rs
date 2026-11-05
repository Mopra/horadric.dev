//! `.pptx` and `.docx` in a file tab: converted by `forge_office` on a
//! thread, a small page shown meanwhile, then a deck of slide pictures (so
//! the deck shell and slide pins apply) or the PDF in WebView2's viewer.
//!
//! The converted files live in the app's state folder under
//! `forge-office`, served on every file tab's host at `/__forge/office/`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use super::runtime::attr;
use super::serve::Reply;
use crate::forge_office::{self, Converted, Converter};

/// What a file's size and time were when it was converted. A save changes
/// one, so a conversion is never shown for bytes it was not made from.
type Stamp = (u64, Option<SystemTime>);

enum State {
    Converting,
    Done(Converted),
    Failed(String),
}

/// By file, lower cased: what its conversion came to.
static OFFICE: Mutex<Option<HashMap<String, (Stamp, State)>>> = Mutex::new(None);

/// Where conversions are cached, None without a state folder.
pub fn cache_root() -> Option<PathBuf> {
    horadric_hooks::state_dir().map(|d| d.join("forge-office"))
}

/// The page for the office file `file` in `root`: its conversion when it
/// is done, else a page saying it is under way. A finished conversion
/// reloads the tab through the watcher's queue, as a save would.
pub fn page(root: &Path, file: &Path) -> Reply {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let deck = file
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pptx"));
    let Ok(meta) = std::fs::metadata(file) else {
        return Reply::html(message_page(&name, &format!("{name} cannot be read."), false))
            .status(404);
    };
    let Some(cache) = cache_root() else {
        return Reply::html(message_page(&name, "There is no state folder to convert into.", false));
    };
    let stamp: Stamp = (meta.len(), meta.modified().ok());
    let key = file.to_string_lossy().to_lowercase();

    let mut office = OFFICE.lock().unwrap_or_else(|p| p.into_inner());
    let map = office.get_or_insert_with(HashMap::new);
    if let Some((s, state)) = map.get(&key) {
        if *s == stamp {
            match state {
                State::Converting => {
                    return Reply::html(message_page(&name, &converting(deck), true))
                }
                State::Failed(why) => return Reply::html(message_page(&name, why, false)),
                State::Done(Converted::Slides(pngs)) => {
                    if pngs.iter().all(|p| p.is_file()) {
                        return Reply::html(deck_page(&name, &cache, pngs));
                    }
                }
                State::Done(Converted::Pdf(pdf)) => {
                    if let Ok(bytes) = std::fs::read(pdf) {
                        return Reply::ok(bytes.into(), "application/pdf");
                    }
                }
            }
            // Done, but the cache was cleared since: converted again below.
        }
    }
    map.insert(key.clone(), (stamp, State::Converting));
    drop(office);

    let root = root.to_path_buf();
    let rel = file
        .strip_prefix(&root)
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    forge_office::convert_async(file, &cache, move |result| {
        let state = match result {
            Ok(c) => State::Done(c),
            Err(e) => State::Failed(e.to_string()),
        };
        if let Ok(mut office) = OFFICE.lock() {
            if let Some(entry) = office.as_mut().and_then(|m| m.get_mut(&key)) {
                // A later save started a conversion of its own.
                if entry.0 == stamp {
                    entry.1 = state;
                }
            }
        }
        super::watch::touched(&root, rel);
    });
    Reply::html(message_page(&name, &converting(deck), true))
}

/// The answer to `/__forge/office/<rest>`: a slide picture or PDF in the
/// cache, nothing outside it.
pub fn asset(rest: &str) -> Option<Reply> {
    let mut path = cache_root()?;
    for seg in rest.split('/').filter(|s| !s.is_empty()) {
        if seg == ".." || seg == "." || seg.contains(['\\', ':', '%']) {
            return None;
        }
        path.push(seg);
    }
    let ty = match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "pdf" => "application/pdf",
        _ => return None,
    };
    let bytes = std::fs::read(&path).ok()?;
    // Named by the hash of what it was made from, so it never changes.
    Some(Reply::ok(bytes.into(), ty).cached())
}

/// The address a converted file in `cache` is served at.
fn url_of(cache: &Path, file: &Path) -> Option<String> {
    let rel = file.strip_prefix(cache).ok()?;
    let segs: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if segs.is_empty() || segs.iter().any(|s| s == ".." || s.contains('%')) {
        return None;
    }
    Some(format!("/__forge/office/{}", segs.join("/")))
}

/// What the page says while it converts, naming the program that will.
fn converting(deck: bool) -> String {
    let found = forge_office::available();
    let office = if deck { Converter::PowerPoint } else { Converter::Word };
    let by = if found.contains(&office) || !found.contains(&Converter::LibreOffice) {
        if deck { "PowerPoint" } else { "Word" }
    } else {
        "LibreOffice"
    };
    format!("Converting with {by}")
}

/// A deck of one picture per slide, laid out as the deck shell expects:
/// a `<section class="slide">` each.
pub fn deck_page(name: &str, cache: &Path, pngs: &[PathBuf]) -> String {
    let mut out = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>{}</title>\n<style>\n\
         html, body {{ margin: 0; background: #121110; }}\n\
         section.slide {{ position: relative; width: 1280px; height: 720px; margin: 0 auto 24px; background: #fff; }}\n\
         section.slide img {{ display: block; width: 100%; height: 100%; object-fit: contain; }}\n\
         </style>\n</head>\n<body>\n",
        attr(name)
    );
    for (i, png) in pngs.iter().enumerate() {
        let Some(src) = url_of(cache, png) else { continue };
        let n = i + 1;
        out.push_str(&format!(
            "<section class=\"slide\" data-slide=\"{n}\"><img src=\"{}\" alt=\"Slide {n}\" draggable=\"false\"></section>\n",
            attr(&src)
        ));
    }
    out.push_str("</body>\n</html>\n");
    out
}

/// A small page with one line on it. While `waiting` it also asks again
/// every few seconds, in case the reload that ends the wait is missed.
fn message_page(name: &str, text: &str, waiting: bool) -> String {
    let refresh = if waiting {
        "<meta http-equiv=\"refresh\" content=\"3\">\n"
    } else {
        ""
    };
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n{refresh}\
         <title>{}</title>\n<style>\n\
         html, body {{ height: 100%; margin: 0; background: #121110; color: #c9c1b4; }}\n\
         body {{ display: flex; align-items: center; justify-content: center; font: 14px/1.5 \"Segoe UI\", system-ui, sans-serif; }}\n\
         p {{ max-width: 36em; text-align: center; }}\n\
         </style>\n</head>\n<body>\n<p>{}</p>\n</body>\n</html>\n",
        attr(name),
        attr(text)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_converted_deck_is_a_slide_per_picture() {
        let cache = Path::new(r"C:\State\forge-office");
        let pngs = [cache.join("ab12").join("slide1.png"), cache.join("ab12").join("slide2.png")];
        let html = deck_page("Q3 <draft>.pptx", cache, &pngs);
        assert_eq!(html.matches("<section class=\"slide\"").count(), 2);
        assert!(html.contains("src=\"/__forge/office/ab12/slide1.png\""));
        assert!(html.contains("data-slide=\"2\""));
        assert!(html.contains("<title>Q3 &lt;draft&gt;.pptx</title>"));
        // A file outside the cache is never given an address.
        assert!(url_of(cache, Path::new(r"C:\Other\x.png")).is_none());
    }

    #[test]
    fn the_cache_is_served_and_nothing_beside_it() {
        assert!(asset("../state.json").is_none());
        assert!(asset("ab12/..%2Fstate.json").is_none());
        assert!(asset("ab12/notes.txt").is_none());
    }

    #[test]
    fn the_wait_page_asks_again_and_a_message_does_not() {
        assert!(message_page("a.pptx", "Converting with PowerPoint", true).contains("refresh"));
        let no = message_page("a.pptx", "To show slides, install PowerPoint", false);
        assert!(!no.contains("refresh") && no.contains("install PowerPoint"));
    }
}
