//! The Forge's tools in `horadric mcp`: a session shows the files it makes
//! for the human in the Forge (a tab of the browser pane), reads the
//! human's pins on them, looks at them rendered, and finds the project's
//! house style for decks. All but `forge_style` go to the app the way the
//! browser tools do; the house style is only files, read here.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::{text, tool};

/// The starting deck shipped with Horadric, for a project with no house
/// style of its own.
const DECK_HTML: &str = include_str!("../../horadric-ui/assets/forge/templates/deck.html");
const DECK_CSS: &str = include_str!("../../horadric-ui/assets/forge/templates/deck.css");

/// Where a project keeps its house style.
const STYLE_DIR: &str = ".horadric/forge";

/// Whether this build has the Forge: only Windows shows files yet.
pub(super) const HAS_FORGE: bool = cfg!(windows);

pub(super) fn tools() -> Vec<Value> {
    if !HAS_FORGE {
        return Vec::new();
    }
    let path = || json!({ "type": "string", "description": "The file, relative to your working folder or absolute" });
    vec![
        tool(
            "forge_open",
            "Show a file in the Forge, a tab of this project's browser pane beside the user's \
             terminals: HTML, a deck, markdown, an image, SVG, PDF or CSV. Use it for files you \
             made for the user to look at. With at, it goes to a slide, a source line or a page.",
            json!({
                "path": path(),
                "at": {
                    "type": "object",
                    "description": "Where to go, one of slide, line or page (from 1)",
                    "properties": {
                        "slide": { "type": "integer" },
                        "line": { "type": "integer" },
                        "page": { "type": "integer" },
                    },
                },
            }),
            &["path"],
        ),
        tool(
            "forge_pins",
            "The user's open pins in the Forge: spots they marked in a file and what they want \
             changed there, as they were sent to you. All of this project's, or one file's.",
            json!({ "path": path() }),
            &[],
        ),
        tool(
            "forge_render",
            "A picture of a file as the Forge draws it, at 1600 x 900, a deck at one slide. Drawn \
             in a hidden tab, so the user's view does not move. Look at your work with it before \
             you say a deck or page is done.",
            json!({
                "path": path(),
                "slide": { "type": "integer", "description": "For a deck, the slide to picture (from 1); the first when left out" },
            }),
            &["path"],
        ),
        tool(
            "forge_style",
            "The project's house style for decks: its stylesheet and starting deck, or \
             Horadric's own when it has none, and how decks are made here. Call it before you \
             make a deck.",
            json!({ "project": { "type": "string", "description": "The project's folder; your working folder when left out" } }),
            &[],
        ),
    ]
}

/// Answers a `forge_*` tool through `app`, None for any other tool.
pub(super) fn call(
    name: &str,
    args: &Value,
    app: &mut impl FnMut(Value) -> Result<Value, String>,
) -> Option<Result<Vec<Value>, String>> {
    if !HAS_FORGE || !name.starts_with("forge_") {
        return None;
    }
    let path = args.get("path").and_then(Value::as_str).map(absolute);
    Some(match name {
        "forge_open" => (|| {
            let path = path.ok_or("say which file: path")?;
            let mut body = json!({ "op": "forge_open", "path": path });
            if let Some(at) = args.get("at").filter(|a| a.is_object()) {
                body["at"] = at.clone();
            }
            let a = app(body)?;
            let shown = if a["shown"] == true {
                "The user can see it."
            } else {
                "It is in the browser pane, which the user does not see right now."
            };
            Ok(vec![text(format!("Opened {} in the Forge. {shown}", path))])
        })(),
        "forge_pins" => (|| {
            let mut body = json!({ "op": "forge_pins" });
            if let Some(p) = &path {
                body["path"] = json!(p);
            }
            let a = app(body)?;
            let said = a["text"].as_str().unwrap_or("").trim().to_string();
            Ok(vec![text(if said.is_empty() {
                "There are no open pins.".to_string()
            } else {
                said
            })])
        })(),
        "forge_render" => (|| {
            let path = path.ok_or("say which file: path")?;
            let mut body = json!({ "op": "forge_render", "path": path });
            if let Some(n) = args.get("slide").and_then(Value::as_u64) {
                body["slide"] = json!(n);
            }
            let a = app(body)?;
            let data = a["data"].as_str().ok_or("the Forge sent no picture")?;
            let mut out = vec![json!({ "type": "image", "data": data, "mimeType": "image/png" })];
            let shown = &a["shown"];
            if let (Some(n), Some(of)) = (shown["shown"].as_u64(), shown["slides"].as_u64()) {
                out.push(text(format!("Slide {n} of {of}.")));
            }
            Ok(out)
        })(),
        "forge_style" => style(args.get("project").and_then(Value::as_str)).map(|t| vec![text(t)]),
        _ => Err(format!("there is no tool {name}")),
    })
}

/// `p` made absolute against this server's folder, which is the session's.
fn absolute(p: &str) -> String {
    let path = Path::new(p);
    if path.is_absolute() {
        return p.to_string();
    }
    std::env::current_dir()
        .map(|d| d.join(path).to_string_lossy().to_string())
        .unwrap_or_else(|_| p.to_string())
}

/// The house style's paths for the project in `project` (this folder when
/// None) and how decks are made here.
fn style(project: Option<&str>) -> Result<String, String> {
    let top = match project {
        Some(p) => PathBuf::from(absolute(p)),
        None => std::env::current_dir().map_err(|e| e.to_string())?,
    };
    let dir = top.join(STYLE_DIR);
    let own = house_files(&dir);
    let (css, deck, whose) = match own {
        (Some(css), Some(deck)) => (
            css,
            deck,
            format!("This project's house style, in {}.", dir.display()),
        ),
        (css, deck) => {
            let shipped = shipped()?;
            let whose = match (&css, &deck) {
                (None, None) => format!(
                    "This project has no house style ({} does not hold one), so these are \
                     Horadric's own. To give it one, copy both into that folder and change the \
                     variables at the top of the stylesheet.",
                    dir.display()
                ),
                _ => format!(
                    "This project's house style in {}, with Horadric's own for what it lacks.",
                    dir.display()
                ),
            };
            (
                css.unwrap_or_else(|| shipped.join("deck.css")),
                deck.unwrap_or_else(|| shipped.join("deck.html")),
                whose,
            )
        }
    };
    Ok(format!(
        "{whose}\n\nStylesheet: {}\nStarting deck: {}\n\n{HOW}",
        css.display(),
        deck.display()
    ))
}

/// How decks are made here, said with the paths.
const HOW: &str = "How decks are made here: one HTML file per deck, a <section class=\"slide\"> \
per slide, designed at 16:9 (1920 x 1080). Start from a copy of the starting deck and keep its \
slide classes. Speaker notes go in an <aside class=\"notes\"> inside their slide. Link the \
stylesheet with a relative href from the deck: the deck is served with its project, so a link \
into .horadric/forge/ works; when the stylesheet is Horadric's own, copy it beside the deck. Keep \
everything in the one file and that stylesheet, with nothing fetched from the web. Do not write \
slide navigation, scaling or a present mode: the Forge adds them. Show the deck with forge_open \
and check each slide with forge_render before you say it is done.";

/// The stylesheet and starting deck in a house style folder: the ones
/// named deck, else the first of each kind.
fn house_files(dir: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return (None, None);
    };
    let mut files: Vec<PathBuf> = read
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    let pick = |ext: &str| {
        let of_kind = |p: &&PathBuf| p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext));
        files
            .iter()
            .filter(of_kind)
            .find(|p| {
                p.file_stem()
                    .is_some_and(|s| s.eq_ignore_ascii_case("deck"))
            })
            .or_else(|| files.iter().find(of_kind))
            .cloned()
    };
    (pick("css"), pick("html"))
}

/// Horadric's own starting deck, written out where a session can read and
/// copy it, kept to this build's.
fn shipped() -> Result<PathBuf, String> {
    let dir = horadric_hooks::state_dir()
        .ok_or("no folder for Horadric's state")?
        .join("forge")
        .join("templates");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for (name, body) in [("deck.html", DECK_HTML), ("deck.css", DECK_CSS)] {
        let file = dir.join(name);
        if std::fs::read_to_string(&file).ok().as_deref() != Some(body) {
            std::fs::write(&file, body)
                .map_err(|e| format!("cannot write {}: {e}", file.display()))?;
        }
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_house_style_prefers_files_named_deck() {
        let dir = std::env::temp_dir().join(format!("horadric-forge-style-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["a.css", "deck.css", "intro.html"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        let (css, deck) = house_files(&dir);
        assert_eq!(css, Some(dir.join("deck.css")));
        assert_eq!(deck, Some(dir.join("intro.html")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_place_is_passed_on_as_given() {
        let mut sent = Vec::new();
        let mut app = |b: Value| {
            sent.push(b);
            Ok(json!({ "shown": true }))
        };
        let a = call(
            "forge_open",
            &json!({ "path": "C:/x/deck.html", "at": { "slide": 3 } }),
            &mut app,
        );
        if HAS_FORGE {
            assert!(a.unwrap().is_ok());
            assert_eq!(sent[0]["at"]["slide"], 3);
            assert_eq!(sent[0]["op"], "forge_open");
        } else {
            assert!(a.is_none());
        }
    }
}
