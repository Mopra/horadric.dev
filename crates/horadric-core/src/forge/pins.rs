//! Pins: a place in a file, or on a web page, plus the human's words, and
//! the text a session gets for them.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The human pointing at a spot in a file and saying what to change there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pin {
    /// Empty for a pin on a web page, which has `url` instead.
    pub file: PathBuf,
    /// The web page pinned, as the browser knows its address rather than
    /// as the page says it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub words: String,
    pub at: At,
    /// An image of the spot, for a session that can look at it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<PathBuf>,
}

/// Where in the file a pin is. The page fills what it knows: a deck the
/// slide, markdown the source lines, HTML the selector and element, an
/// image or PDF only the box. This is the page's JSON as it sends it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct At {
    /// 1 based.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slide: Option<u32>,
    /// 1 based.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    /// 1 based, inclusive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<(u32, u32)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    /// The element's text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The element's outer HTML, as the page trimmed it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    /// A CSV cell, as the table's `data-row` (header 0) and `data-col`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub col: Option<u32>,
    /// In CSS pixels of the page, or the image's own pixels for an image.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rect: Option<Rect>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl At {
    pub fn from_json(json: &str) -> serde_json::Result<At> {
        serde_json::from_str(json)
    }
}

/// How much of an element's text and HTML goes in the message: enough to
/// find it in the source, not a whole slide's markup per pin.
const TEXT_MAX: usize = 300;
const HTML_MAX: usize = 600;

/// The text sent to a session for `pins`, written so Claude can act
/// without guessing: the file relative to the project, the slide, page,
/// lines or selector, what the element says, the box, the crop, and the
/// human's words last, so the place is read before the request.
pub fn pin_message(pins: &[Pin], project_root: &Path) -> String {
    let mut out = String::new();
    let pages = pins.iter().filter(|p| p.url.is_some()).count();
    let spots = match (pages, pins.len() - pages) {
        (0, _) => "in files in the Forge",
        (_, 0) => "on web pages in the browser",
        _ => "in files in the Forge and on web pages",
    };
    match pins.len() {
        0 => return out,
        1 if pages == 1 => out.push_str(
            "The human pinned a spot on a web page in the browser and said what to change there.\n\n",
        ),
        1 => out.push_str(
            "The human pinned a spot in a file in the Forge and said what to change there.\n\n",
        ),
        n => out.push_str(&format!(
            "The human pinned {n} spots {spots} and said what to change at each.\n\n"
        )),
    }
    for (n, pin) in pins.iter().enumerate() {
        let at = &pin.at;
        let mut place = vec![match &pin.url {
            Some(url) => url.clone(),
            None => relative(&pin.file, project_root),
        }];
        if let Some(s) = at.slide {
            place.push(format!("slide {s}"));
        }
        if let Some(p) = at.page {
            place.push(format!("page {p}"));
        }
        if let Some((a, b)) = at.lines {
            place.push(if a == b {
                format!("line {a}")
            } else {
                format!("lines {a} to {b}")
            });
        }
        if let (Some(r), Some(c)) = (at.row, at.col) {
            place.push(if r == 0 {
                format!("header of column {}", c + 1)
            } else {
                format!("row {r}, column {}", c + 1)
            });
        }
        if let Some(sel) = at.selector.as_deref().filter(|s| !s.trim().is_empty()) {
            place.push(format!("`{}`", sel.trim()));
        }
        let lead = if pins.len() == 1 {
            String::new()
        } else {
            format!("{}. ", n + 1)
        };
        let pad = " ".repeat(lead.len());
        out.push_str(&format!("{lead}{}\n", place.join(", ")));
        if let Some(text) = at.text.as_deref().map(squash).filter(|t| !t.is_empty()) {
            out.push_str(&format!("{pad}Text there: \"{}\"\n", clip(&text, TEXT_MAX)));
        }
        if let Some(html) = at.html.as_deref().map(squash).filter(|t| !t.is_empty()) {
            out.push_str(&format!("{pad}Element: {}\n", clip(&html, HTML_MAX)));
        }
        if let Some(r) = at.rect {
            out.push_str(&format!(
                "{pad}Box: x {}, y {}, {} by {} px\n",
                r.x.round(),
                r.y.round(),
                r.w.round(),
                r.h.round()
            ));
        }
        if let Some(crop) = &pin.crop {
            out.push_str(&format!("{pad}Image of the spot: {}\n", crop.display()));
        }
        out.push_str(&format!("{pad}Change: {}\n", pin.words.trim()));
        if n + 1 < pins.len() {
            out.push('\n');
        }
    }
    out
}

/// `file` relative to `root` with forward slashes, or whole when it is
/// outside it. Compared without case, as Windows paths are.
fn relative(file: &Path, root: &Path) -> String {
    let parts: Vec<Component> = file.components().collect();
    let base: Vec<Component> = root.components().collect();
    let same = |a: &Component, b: &Component| {
        a.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
    };
    if !base.is_empty()
        && parts.len() > base.len()
        && parts.iter().zip(&base).all(|(a, b)| same(a, b))
    {
        parts[base.len()..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    } else {
        file.display().to_string()
    }
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((at, _)) => format!("{}...", &s[..at]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forge_pin_json_shape() {
        let at = At::from_json(
            r#"{"slide":3,"lines":[4,9],"selector":"h2","text":"Hi","rect":{"x":1.5,"y":2,"w":30,"h":40},"extra":1}"#,
        )
        .unwrap();
        assert_eq!(at.slide, Some(3));
        assert_eq!(at.lines, Some((4, 9)));
        assert_eq!(at.page, None);
        assert_eq!(at.rect, Some(Rect { x: 1.5, y: 2.0, w: 30.0, h: 40.0 }));
        assert_eq!(At::from_json("{}").unwrap(), At::default());
    }

    #[test]
    fn forge_pin_message_one() {
        let pin = Pin {
            file: PathBuf::from("C:/Work/Proj/decks/q3.html"),
            url: None,
            words: " make it bolder ".into(),
            at: At {
                slide: Some(3),
                selector: Some("section.slide:nth-of-type(3) > h2".into()),
                text: Some("Quarterly\n   results".into()),
                html: Some("<h2>Quarterly results</h2>".into()),
                rect: Some(Rect { x: 120.4, y: 40.0, w: 300.0, h: 80.0 }),
                ..At::default()
            },
            crop: Some(PathBuf::from("C:/tmp/pin1.png")),
        };
        let msg = pin_message(&[pin], Path::new("c:/work/proj"));
        assert_eq!(
            msg,
            "The human pinned a spot in a file in the Forge and said what to change there.\n\n\
             decks/q3.html, slide 3, `section.slide:nth-of-type(3) > h2`\n\
             Text there: \"Quarterly results\"\n\
             Element: <h2>Quarterly results</h2>\n\
             Box: x 120, y 40, 300 by 80 px\n\
             Image of the spot: C:/tmp/pin1.png\n\
             Change: make it bolder\n"
        );
    }

    #[test]
    fn forge_pin_message_many() {
        let pins = [
            Pin {
                file: PathBuf::from("/p/notes.md"),
                url: None,
                words: "shorter".into(),
                at: At { lines: Some((5, 7)), ..At::default() },
                crop: None,
            },
            Pin {
                file: PathBuf::from("/elsewhere/data.csv"),
                url: None,
                words: "fix the total".into(),
                at: At { row: Some(4), col: Some(2), text: Some("x".repeat(400)), ..At::default() },
                crop: None,
            },
        ];
        let msg = pin_message(&pins, Path::new("/p"));
        assert!(msg.starts_with("The human pinned 2 spots"));
        assert!(msg.contains("1. notes.md, lines 5 to 7\n   Change: shorter\n\n2. "));
        assert!(msg.contains("data.csv, row 4, column 3\n"));
        assert!(msg.contains(&format!("   Text there: \"{}...\"\n", "x".repeat(300))));
        assert_eq!(pin_message(&[], Path::new("/p")), "");
    }

    #[test]
    fn forge_pin_message_on_a_web_page() {
        let page = Pin {
            file: PathBuf::new(),
            url: Some("https://example.com/pricing".into()),
            words: "this plan is wrong".into(),
            at: At { selector: Some("#plans > div:nth-of-type(2)".into()), ..At::default() },
            crop: None,
        };
        let msg = pin_message(std::slice::from_ref(&page), Path::new("/p"));
        assert!(msg.starts_with("The human pinned a spot on a web page in the browser"));
        assert!(msg.contains("
https://example.com/pricing, `#plans > div:nth-of-type(2)`
"));
        let file = Pin { file: PathBuf::from("/p/a.md"), url: None, ..page.clone() };
        let both = pin_message(&[file, page], Path::new("/p"));
        assert!(both.starts_with("The human pinned 2 spots in files in the Forge and on web pages"));
        assert!(both.contains("1. a.md, "));
    }
}
