//! Pins on ordinary web pages in the browser pane: the human points at an
//! element of any site, says what to change, and the session gets the
//! address, the element and a crop, as it would for a file's pin.
//!
//! A web page is not ours, so none of this goes through the page's own
//! world. The pin layer (`assets/forge/page-pins.js`) runs in an isolated
//! world made over DevTools, which shares the page's DOM but none of its
//! objects, and it speaks only through the answer to the host's own
//! `Runtime.evaluate`. Web tabs get no message handler and no host object
//! for this, so a page's scripts have nothing to post a pin to. Each answer
//! carries a token only that world was given, the human must have just
//! pressed a key with the pane in front, and the tab must be the one shown.
//!
//! Runtime is never enabled for this: bot checks look for it, and only a
//! tab a login was filled in turns it on (`web::track_contexts`).

use std::cell::RefCell;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;
use std::time::Duration;

use horadric_core::forge::{At, Pin, Rect};
use serde_json::{json, Value};

use super::{pins, serve};
use crate::web;

const SCRIPT: &str = include_str!("../../assets/forge/page-pins.js");
const WORLD: &str = "horadric-pins";
/// How long after the human's last key an answer still counts as theirs:
/// the Enter that sent it is on its way through DevTools.
const SENT_WITHIN: Duration = Duration::from_secs(3);
/// The most of an element's text and markup a pin keeps; the message to
/// the session cuts them shorter still.
const TEXT_MAX: usize = 2000;
const HTML_MAX: usize = 4000;
const WORDS_MAX: usize = 4000;

/// A tab in pin mode on a web page.
struct On {
    key: String,
    tab: u64,
    /// The isolated world's context, None while it is being made.
    world: Option<i64>,
    token: String,
}

thread_local! {
    static ON: RefCell<Vec<On>> = const { RefCell::new(Vec::new()) };
}

/// Whether the project's shown tab is a web page in pin mode, for the
/// browser menu.
pub fn pinning(key: &str) -> bool {
    let Some(tab) = web::shown_tab(key) else {
        return false;
    };
    ON.with(|o| o.borrow().iter().any(|p| p.key == key && p.tab == tab))
}

/// Pin mode on or off in the project's shown tab, from the human's menu or
/// key. A Forge file gets its own pins; a web page these.
pub fn toggle(key: &str) {
    let Some(tab) = web::shown_tab(key) else {
        return;
    };
    let url = web::label_of(key, Some(tab))
        .map(|(_, u)| u)
        .unwrap_or_default();
    if serve::host_of(&url).is_some_and(|h| h.to_ascii_lowercase().ends_with(serve::SUFFIX)) {
        return pins::toggle_pinning(key);
    }
    prune();
    if let Some(world) = take(key, tab) {
        stop(key, tab, world);
        return;
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return;
    }
    web::focus(key);
    start(key, tab);
}

/// Forgets tabs that closed: the browser does not say.
fn prune() {
    ON.with(|o| {
        o.borrow_mut()
            .retain(|p| web::place_of(&p.key, p.tab).is_some())
    });
}

/// Takes the tab out of pin mode here, giving its world if it had one.
fn take(key: &str, tab: u64) -> Option<Option<i64>> {
    ON.with(|o| {
        let mut o = o.borrow_mut();
        let i = o.iter().position(|p| p.key == key && p.tab == tab)?;
        Some(o.remove(i).world)
    })
}

/// The tab's world and token while it is in pin mode in that world.
fn current(key: &str, tab: u64, world: i64) -> Option<String> {
    ON.with(|o| {
        (o.borrow().iter())
            .find(|p| p.key == key && p.tab == tab && p.world == Some(world))
            .map(|p| p.token.clone())
    })
}

/// A token the page cannot guess: the standard library's hasher keys come
/// from the system's randomness.
fn token() -> String {
    let a = RandomState::new().build_hasher().finish();
    let mut b = RandomState::new().build_hasher();
    b.write_u64(a);
    format!("{a:016x}{:016x}", b.finish())
}

fn start(key: &str, tab: u64) {
    let token = token();
    ON.with(|o| {
        o.borrow_mut().push(On {
            key: key.to_string(),
            tab,
            world: None,
            token: token.clone(),
        })
    });
    let owned = key.to_string();
    call(key, tab, "Page.getFrameTree", "{}", move |tree| {
        let frame = tree.and_then(|v| {
            v.pointer("/frameTree/frame/id")?
                .as_str()
                .map(str::to_string)
        });
        let Some(frame) = frame else {
            take(&owned, tab);
            return;
        };
        // The world sees the page's DOM but not its objects; it is given
        // no more reach than the page itself has.
        let params = json!({"frameId": frame, "worldName": WORLD, "grantUniveralAccess": false});
        let key = owned.clone();
        call(
            &owned,
            tab,
            "Page.createIsolatedWorld",
            &params.to_string(),
            move |made| {
                let world = made.and_then(|v| v.get("executionContextId")?.as_i64());
                let still = ON.with(|o| {
                    let mut o = o.borrow_mut();
                    let p = o
                        .iter_mut()
                        .find(|p| p.key == key && p.tab == tab && p.token == token)?;
                    p.world = world;
                    world
                });
                match still {
                    Some(world) => next(&key, tab, world, false, Some(&token)),
                    // Toggled off meanwhile, or no world could be made.
                    None => {
                        take(&key, tab);
                    }
                }
            },
        );
    });
}

/// Waits in the tab's world for the human's next pin. `took` says the last
/// one was handed on, so its rune is drawn. The token goes in only on the
/// first call, made the moment the world was: a later call could, after a
/// navigation, find its context number reused by another page's world.
fn next(key: &str, tab: u64, world: i64, took: bool, token: Option<&str>) {
    let css = serve::forge_css(crate::theme::accent(key));
    let expression = format!(
        "(function(t){{if(t&&!window.__hdPinsToken)window.__hdPinsToken=t;\
         return ({SCRIPT})({css},{took}).then(a=>a&&JSON.stringify({{t:window.__hdPinsToken,pin:a}}));}})({})",
        serde_json::to_string(&token).unwrap_or_else(|_| "null".into()),
        css = serde_json::to_string(&css).unwrap_or_else(|_| "\"\"".into()),
    );
    let params = json!({
        "expression": expression,
        "contextId": world,
        "awaitPromise": true,
        "returnByValue": true,
    });
    let owned = key.to_string();
    call(
        key,
        tab,
        "Runtime.evaluate",
        &params.to_string(),
        move |answer| {
            let Some(token) = current(&owned, tab, world) else {
                return;
            };
            let said = answer
                .and_then(|v| v.pointer("/result/value")?.as_str().map(str::to_string))
                .and_then(|s| serde_json::from_str::<Value>(&s).ok());
            // Esc, the page hidden or gone: pin mode is over.
            let Some(said) = said else {
                take(&owned, tab);
                return;
            };
            let ours = said.get("t").and_then(Value::as_str) == Some(token.as_str());
            let theirs =
                web::shown_tab(&owned) == Some(tab) && pins::human_within(&owned, SENT_WITHIN);
            let pin = said
                .get("pin")
                .and_then(Value::as_str)
                .and_then(|p| serde_json::from_str(p).ok());
            match pin.filter(|_| ours && theirs) {
                Some(pin) => pinned(&owned, tab, world, &pin),
                None => {
                    eprintln!("horadric: page pin from tab {tab} of {owned} refused");
                    take(&owned, tab);
                    stop(&owned, tab, Some(world));
                }
            }
        },
    );
}

/// The human's pin, as the world answered it: cropped, handed to its
/// session, and the next one waited for.
fn pinned(key: &str, tab: u64, world: i64, said: &Value) {
    let text = |k: &str, max: usize| {
        let s = said
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        (!s.is_empty()).then(|| clip(s, max))
    };
    let Some(words) = text("words", WORDS_MAX) else {
        return next(key, tab, world, false, None);
    };
    // Where the browser is, not where the page says it is.
    let Some(url) = web::label_of(key, Some(tab)).map(|(_, u)| u) else {
        return;
    };
    let rect = said.get("rect").and_then(|r| {
        let n = |k: &str| r.get(k).and_then(Value::as_f64).filter(|n| n.is_finite());
        Some(Rect {
            x: n("x")?,
            y: n("y")?,
            w: n("w")?.max(0.0),
            h: n("h")?.max(0.0),
        })
    });
    let pin = Pin {
        file: PathBuf::new(),
        url: Some(url),
        words,
        at: At {
            selector: text("selector", TEXT_MAX),
            text: text("text", TEXT_MAX),
            html: text("html", HTML_MAX),
            rect,
            ..At::default()
        },
        crop: None,
    };
    let driver =
        web::place_of(key, tab).and_then(|(at, _, _)| web::drivers(key).get(at).cloned().flatten());
    let Some(r) = rect.filter(|r| r.w >= 1.0 && r.h >= 1.0) else {
        pins::give_page(key, pin, driver);
        return next(key, tab, world, true, None);
    };
    let params = crate::pick::crop_params([r.x, r.y, r.w, r.h]);
    let owned = key.to_string();
    web::devtools(key, tab, "Page.captureScreenshot", &params, move |shot| {
        web::forget_call(&owned, tab);
        let mut pin = pin;
        pin.crop = shot.ok().and_then(|s| pins::save_crop(&s));
        pins::give_page(&owned, pin, driver);
        if current(&owned, tab, world).is_some() {
            next(&owned, tab, world, true, None);
        }
    });
    web::forget_call(key, tab);
}

/// Ends pin mode in the tab's world, keeping the runes drawn.
fn stop(key: &str, tab: u64, world: Option<i64>) {
    let Some(world) = world else { return };
    let params = json!({
        "expression": "window.__hdPins&&window.__hdPins.stop(false)",
        "contextId": world,
    });
    call(key, tab, "Runtime.evaluate", &params.to_string(), |_| {});
}

/// A DevTools call of the host's own, its answer read as JSON. It is not an
/// agent's, so it never counts as one having just driven the tab.
fn call(
    key: &str,
    tab: u64,
    method: &str,
    params: &str,
    done: impl FnOnce(Option<Value>) + 'static,
) {
    let owned = key.to_string();
    web::devtools(key, tab, method, params, move |answer| {
        web::forget_call(&owned, tab);
        done(answer.ok().and_then(|a| serde_json::from_str(&a).ok()));
    });
    web::forget_call(key, tab);
}

fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}...", &s[..i]),
        None => s.to_string(),
    }
}
