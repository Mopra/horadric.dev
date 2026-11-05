//! The Forge's half of `forge_render`, the tool a session checks its work
//! with: the file drawn in a tab of its own at a fixed size, pictured and
//! closed again, so the tab the human looks at is never moved or resized.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;
use std::time::Duration;

use horadric_core::forge::Kind;
use serde_json::{json, Value};

use super::office::{self, Conversion};
use super::{kind, served, settings, FileTab, TABS};
use crate::web;

/// Told every session given the Horadric tools, after the browser's part.
/// The house style is left to `forge_style` rather than told when a deck
/// opens: one prompt line and a tool are simpler than a message mid turn.
pub const PROMPT: &str = "Files you write for the user to look at (decks, reports, pages, diagrams) open in the Forge, a tab of the browser pane: show one with forge_open. Prefer one HTML deck or one single file artefact to Office files. Before making a deck, call forge_style for this project's house style. Check the result with forge_render before you say it is done.";

/// The size a render is drawn at: a slide at 16:9, wide enough to read.
const WIDTH: u32 = 1600;
const HEIGHT: u32 = 900;

/// Readies the page for its picture: the deck shell's strip and controls
/// and the pin runes hidden, the slide asked for shown, fonts loaded and a
/// frame drawn. Answers how many slides there are and which is shown.
const READY: &str = r#"(async (slide) => {
  const frame = () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  const isDeck = !!document.querySelector("section.slide");
  for (let i = 0; isDeck && !window.__forgeDeck && i < 40; i++) await new Promise((r) => setTimeout(r, 50));
  const deck = window.__forgeDeck;
  if (deck) {
    deck.setMode("single");
    deck.setStrip(false);
    if (slide) deck.goto(slide);
  }
  const style = document.createElement("style");
  style.textContent = "forge-overlay, forge-deck-ui { display: none !important; }";
  document.documentElement.appendChild(style);
  if (document.fonts) await document.fonts.ready;
  await frame();
  await new Promise((r) => setTimeout(r, 250));
  await frame();
  if (!deck) return { slides: 0 };
  const el = document.querySelector("section.slide.forge-current") || document.querySelector("section.slide");
  const r = el.getBoundingClientRect();
  return { slides: deck.count, shown: deck.current, rect: { x: r.x, y: r.y, width: r.width, height: r.height } };
})"#;

/// The longest a render may take, an Office file's conversion included.
const TIMEOUT: Duration = Duration::from_secs(30);

/// A render's ending, run once: by the render or by its timeout,
/// whichever comes first.
type Finish = Rc<Cell<Option<Box<dyn FnOnce(Result<Value, String>)>>>>;

thread_local! {
    static RUNNING: RefCell<HashMap<u64, Finish>> = RefCell::new(HashMap::new());
}

/// Renders whose time ran out, told by the timer threads.
static TIMED_OUT: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// On the UI thread: ends the renders whose time ran out, closing their
/// tabs.
pub(super) fn drain() {
    let out = TIMED_OUT
        .lock()
        .map(|mut t| std::mem::take(&mut *t))
        .unwrap_or_default();
    for id in out {
        if let Some(finish) = RUNNING.with(|r| r.borrow_mut().remove(&id)) {
            if let Some(f) = finish.take() {
                f(Err(format!(
                    "the file was not ready to picture within {} seconds",
                    TIMEOUT.as_secs()
                )));
            }
        }
    }
}

/// Draws `file` in a new tab of the project `key`'s browser, behind the
/// one shown, pictures it at slide `slide` when it is a deck, closes the
/// tab and gives `done` the PNG as base64 and what was shown, or why not.
pub fn render(
    key: &str,
    root: Option<&Path>,
    file: &Path,
    slide: Option<u32>,
    done: impl FnOnce(Result<Value, String>) + 'static,
) {
    let Some(id) = open_apart(key, root, file) else {
        return done(Err("the Forge does not show this file".into()));
    };
    let k = key.to_string();
    let once: Finish = Rc::new(Cell::new(Some(Box::new(move |r| {
        RUNNING.with(|run| run.borrow_mut().remove(&id));
        // Closed, the tab is out of the browser's saved tabs as well.
        web::close_tab(&k, id);
        super::prune();
        done(r);
    }))));
    RUNNING.with(|r| r.borrow_mut().insert(id, Rc::clone(&once)));
    let timer = std::thread::Builder::new()
        .name("horadric-forge-render-timeout".into())
        .spawn(move || {
            std::thread::sleep(TIMEOUT);
            if let Ok(mut t) = TIMED_OUT.lock() {
                t.push(id);
            }
            super::wake();
        });
    if timer.is_err() {
        eprintln!("horadric: a forge render runs without a timeout");
    }
    let finish = move |r: Result<Value, String>| {
        if let Some(f) = once.take() {
            f(r);
        }
    };
    let k = key.to_string();
    let office = matches!(kind(file), Some(Kind::Pptx | Kind::Docx)).then(|| file.to_path_buf());
    when_shown(key, id, office, move |shown| {
        if let Err(e) = shown {
            return finish(Err(e));
        }
        let size = json!({
            "width": WIDTH, "height": HEIGHT, "deviceScaleFactor": 1, "mobile": false
        });
        let k2 = k.clone();
        web::devtools(
            &k,
            id,
            "Emulation.setDeviceMetricsOverride",
            &size.to_string(),
            move |r| {
                if let Err(e) = r {
                    return finish(Err(e));
                }
                let call = json!({
                    "expression": format!("{READY}({})", slide.unwrap_or(0)),
                    "awaitPromise": true,
                    "returnByValue": true,
                });
                let k3 = k2.clone();
                web::devtools(&k2, id, "Runtime.evaluate", &call.to_string(), move |r| {
                    let shown = match r {
                        Ok(text) => serde_json::from_str::<Value>(&text)
                            .ok()
                            .and_then(|v| v.pointer("/result/value").cloned())
                            .unwrap_or(Value::Null),
                        Err(e) => return finish(Err(e)),
                    };
                    // A slide is letterboxed in the view: only the slide, at the
                    // full width, is pictured.
                    let mut shot = json!({ "format": "png" });
                    if let Some(r) = shown
                        .get("rect")
                        .filter(|r| r["width"].as_f64() > Some(0.0))
                    {
                        let scale = WIDTH as f64 / r["width"].as_f64().unwrap_or(1.0);
                        shot["clip"] = json!({
                            "x": r["x"], "y": r["y"], "width": r["width"], "height": r["height"],
                            "scale": scale,
                        });
                    }
                    let shot = shot.to_string();
                    web::devtools(&k3, id, "Page.captureScreenshot", &shot, move |r| {
                        finish(r.and_then(|text| {
                            let v: Value =
                                serde_json::from_str(&text).map_err(|e| e.to_string())?;
                            let data = v["data"].as_str().ok_or("the page sent no picture")?;
                            let mut shown = shown;
                            if let Some(o) = shown.as_object_mut() {
                                o.remove("rect");
                            }
                            Ok(json!({ "data": data, "shown": shown }))
                        }))
                    });
                });
            },
        );
    });
}

/// Runs `then` once the tab `id` has loaded the page to picture: for an
/// Office file, the one its conversion made, not the page saying it is
/// under way. The finished conversion reloads the tab.
fn when_shown(
    key: &str,
    id: u64,
    office: Option<PathBuf>,
    then: impl FnOnce(Result<(), String>) + 'static,
) {
    let k = key.to_string();
    web::after_load(key, id, move |ok| {
        if !ok {
            return then(Err("the file did not finish loading".into()));
        }
        let Some(file) = office else {
            return then(Ok(()));
        };
        match office::conversion(&file) {
            Conversion::Waiting => when_shown(&k, id, Some(file), then),
            Conversion::Ready => then(Ok(())),
            Conversion::Failed(why) => then(Err(why)),
        }
    });
}

/// A new tab for `file`, never the one shown and never one it has already:
/// those are the human's, and a render resizes the page it draws in.
fn open_apart(key: &str, root: Option<&Path>, file: &Path) -> Option<u64> {
    kind(file)?;
    let (root, host) = served(root, file)?;
    let url = super::serve::url_for(&host, &root, file)?;
    settings::remember_root(&host, &root);
    let id = web::open_file_tab(key, &url, false);
    TABS.with(|t| {
        t.borrow_mut().push(FileTab {
            key: key.to_string(),
            id,
            file: file.to_path_buf(),
            root,
            host,
            scroll: None,
        })
    });
    Some(id)
}
