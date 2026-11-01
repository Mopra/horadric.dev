//! The DevTools protocol calls `horadric mcp` makes, stood in for where
//! there is no DevTools protocol: WebKit on the Mac. WebView2 takes them
//! as they are; WKWebView can only run script in the page and take a
//! picture of it. So each call becomes a script whose answer is the
//! protocol's answer as JSON, or a picture, and the agent's side stays the
//! same on both systems.
//!
//! What a script can not do is say so ([`plan`] errs), rather than
//! pretend. Its events are the page's own, not the user's: a page that
//! checks `isTrusted`, or a popup only a real click may open, sees the
//! difference.
//!
//! Pure, so it is tested on Windows too.

use serde_json::{json, Value};

/// What a protocol call becomes.
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    /// An async function body to run in the page. It returns the call's
    /// answer as a JSON string. When it can not be parsed (a statement
    /// where an expression was expected), `fallback` is tried instead.
    Script {
        body: String,
        fallback: Option<String>,
    },
    /// A picture of the page, PNG, of `clip` in page pixels when given (
    /// else what is in view), past the viewport when `beyond`.
    Picture { clip: Option<Clip>, beyond: bool },
}

/// A part of the page, in CSS pixels from the document's top left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Clip {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The methods stood in for, for the error that names them.
pub const METHODS: &[&str] = &[
    "Runtime.evaluate",
    "Input.dispatchMouseEvent",
    "Input.dispatchKeyEvent",
    "Input.insertText",
    "Page.captureScreenshot",
    "Page.getLayoutMetrics",
    "Page.navigate",
    "Page.reload",
];

/// What `method` with `params` becomes, or why it can not be done here.
pub fn plan(method: &str, params: &Value) -> Result<Plan, String> {
    let s = |k: &str| params.get(k).and_then(Value::as_str);
    let n = |k: &str| params.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    Ok(match method {
        "Runtime.evaluate" => {
            let expression = s("expression").ok_or("Runtime.evaluate needs an expression")?;
            evaluate(expression)
        }
        "Input.dispatchMouseEvent" => {
            let kind = s("type").ok_or("Input.dispatchMouseEvent needs a type")?;
            if !matches!(
                kind,
                "mouseMoved" | "mousePressed" | "mouseReleased" | "mouseWheel"
            ) {
                return Err(format!("{kind} is not a mouse event type"));
            }
            let event = json!({
                "type": kind,
                "x": n("x"),
                "y": n("y"),
                "button": s("button").unwrap_or("none"),
                "count": params.get("clickCount").and_then(Value::as_u64).unwrap_or(1),
                "dx": n("deltaX"),
                "dy": n("deltaY"),
                "modifiers": params.get("modifiers").and_then(Value::as_u64).unwrap_or(0),
            });
            script(MOUSE, &event)
        }
        "Input.dispatchKeyEvent" => {
            let kind = s("type").ok_or("Input.dispatchKeyEvent needs a type")?;
            if !matches!(kind, "keyDown" | "rawKeyDown" | "keyUp" | "char") {
                return Err(format!("{kind} is not a key event type"));
            }
            let event = json!({
                "type": kind,
                "key": s("key").unwrap_or(""),
                "code": s("code").unwrap_or(""),
                "keyCode": params.get("windowsVirtualKeyCode").and_then(Value::as_u64).unwrap_or(0),
                "text": s("text").unwrap_or(""),
                "modifiers": params.get("modifiers").and_then(Value::as_u64).unwrap_or(0),
            });
            script(KEY, &event)
        }
        "Input.insertText" => script(INSERT, &json!(s("text").unwrap_or(""))),
        "Page.getLayoutMetrics" => script(METRICS, &Value::Null),
        "Page.navigate" => {
            let url = s("url").ok_or("Page.navigate needs a url")?;
            script(NAVIGATE, &json!(url))
        }
        "Page.reload" => script(RELOAD, &Value::Null),
        "Page.captureScreenshot" => {
            if s("format").is_some_and(|f| f != "png") {
                return Err("the Mac's browser takes PNG pictures only".into());
            }
            let clip = params.get("clip").map(|c| Clip {
                x: c["x"].as_f64().unwrap_or(0.0),
                y: c["y"].as_f64().unwrap_or(0.0),
                width: c["width"].as_f64().unwrap_or(0.0),
                height: c["height"].as_f64().unwrap_or(0.0),
            });
            if clip.is_some_and(|c| c.width <= 0.0 || c.height <= 0.0) {
                return Err("the clip takes no space".into());
            }
            Plan::Picture {
                clip,
                beyond: params.get("captureBeyondViewport") == Some(&json!(true)),
            }
        }
        _ => {
            return Err(format!(
                "the Mac's browser is WebKit, which has no DevTools protocol: {method} is not \
                 one Horadric stands in for there (it does {}). Use browser_evaluate instead.",
                METHODS.join(", ")
            ))
        }
    })
}

/// Where a picture of `clip` is taken in a page scrolled to `scroll` with
/// a viewport of `view`, both CSS pixels: in view as it is when it fits,
/// else after the viewport grows to `grow` tall and the page scrolls to
/// the clip's top.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shot {
    InView(Clip),
    Grow { grow: f64, top: f64 },
}

/// Where to take a picture of `clip`, as [`Shot`] says.
pub fn shot(clip: Clip, scroll: (f64, f64), view: (f64, f64), tallest: f64) -> Shot {
    let fits = clip.x >= scroll.0
        && clip.y >= scroll.1
        && clip.x + clip.width <= scroll.0 + view.0 + 0.5
        && clip.y + clip.height <= scroll.1 + view.1 + 0.5;
    if fits {
        return Shot::InView(Clip {
            x: clip.x - scroll.0,
            y: clip.y - scroll.1,
            ..clip
        });
    }
    Shot::Grow {
        grow: clip.height.max(view.1).min(tallest),
        top: clip.y,
    }
}

/// The protocol's answer to a script that threw, as `horadric mcp` reads
/// it.
pub fn thrown(message: &str) -> Value {
    json!({
        "exceptionDetails": {
            "text": message,
            "exception": { "description": message },
        }
    })
}

/// Whether a script's error is that it could not be parsed.
pub fn unparsed(message: &str) -> bool {
    message.contains("SyntaxError")
}

fn script(fun: &str, arg: &Value) -> Plan {
    Plan::Script {
        body: format!("return JSON.stringify(await ({fun})({arg}));"),
        fallback: None,
    }
}

/// `expression` run in the page with its promise awaited. Put in the body
/// as it is, so a page whose security policy forbids `eval` still runs
/// it; through `eval` only when it is statements, which an expression
/// slot can not hold.
fn evaluate(expression: &str) -> Plan {
    let wrap = |run: &str| {
        format!(
            "let v;\ntry {{ {run} }} catch (e) {{ \
             return JSON.stringify({{ exceptionDetails: {{ text: String(e && e.message || e), \
             exception: {{ description: String(e && e.stack || e) }} }} }}); }}\n{REPLY}"
        )
    };
    Plan::Script {
        body: wrap(&format!("v = await (\n{expression}\n);")),
        fallback: Some(wrap(&format!(
            "v = await (0, eval)({});",
            Value::from(expression)
        ))),
    }
}

/// The value `v` as `Runtime.evaluate` answers with `returnByValue`.
const REPLY: &str = r#"const type = v === null ? "object" : typeof v;
try {
  return JSON.stringify({ result: { type, value: v === undefined ? null : v } });
} catch (e) {
  return JSON.stringify({ result: { type, value: null, description: String(v) } });
}"#;

/// A mouse event at a point in the viewport, as the page's own events: the
/// element there hears what a real mouse would send it, a wheel scrolls
/// what it is over when the page does not take it.
const MOUSE: &str = r#"async (m) => {
  let el = document.elementFromPoint(m.x, m.y) || document.body || document.documentElement;
  while (el && el.shadowRoot) {
    const inner = el.shadowRoot.elementFromPoint(m.x, m.y);
    if (!inner || inner === el) break;
    el = inner;
  }
  const button = { left: 0, middle: 1, right: 2 }[m.button] ?? 0;
  const buttons = m.type === "mousePressed" ? ({ left: 1, middle: 4, right: 2 }[m.button] ?? 0) : 0;
  const base = {
    bubbles: true, cancelable: true, composed: true, view: window,
    clientX: m.x, clientY: m.y, screenX: m.x, screenY: m.y,
    altKey: !!(m.modifiers & 1), ctrlKey: !!(m.modifiers & 2),
    metaKey: !!(m.modifiers & 4), shiftKey: !!(m.modifiers & 8),
    button, buttons, detail: m.count,
  };
  const pointer = (type) => el.dispatchEvent(new PointerEvent(type, { ...base, pointerId: 1, pointerType: "mouse", isPrimary: true }));
  const mouse = (type, extra) => el.dispatchEvent(new MouseEvent(type, { ...base, ...extra }));
  if (m.type === "mouseMoved") {
    pointer("pointerover"); mouse("mouseover");
    pointer("pointermove"); mouse("mousemove");
  } else if (m.type === "mousePressed") {
    pointer("pointerdown");
    if (mouse("mousedown")) {
      const f = el.closest("input, textarea, select, button, a[href], [tabindex], [contenteditable]");
      if (f && typeof f.focus === "function") f.focus();
      else if (document.activeElement && document.activeElement !== document.body) document.activeElement.blur();
    }
    if (m.button === "right") mouse("contextmenu");
  } else if (m.type === "mouseReleased") {
    pointer("pointerup"); mouse("mouseup");
    if (m.button === "left") {
      mouse("click");
      if (m.count === 2) mouse("dblclick");
    } else if (m.button === "middle") {
      mouse("auxclick");
    }
  } else if (m.type === "mouseWheel") {
    const go = el.dispatchEvent(new WheelEvent("wheel", { ...base, deltaX: m.dx, deltaY: m.dy, deltaMode: 0 }));
    if (go) {
      let s = el;
      const scrolls = (n) => n && n !== document.body && n !== document.documentElement
        && ((n.scrollHeight > n.clientHeight && m.dy) || (n.scrollWidth > n.clientWidth && m.dx))
        && /(auto|scroll|overlay)/.test(getComputedStyle(n).overflow + getComputedStyle(n).overflowY + getComputedStyle(n).overflowX);
      while (s && !scrolls(s)) s = s.parentElement;
      if (s) s.scrollBy({ left: m.dx, top: m.dy, behavior: "instant" });
      else window.scrollBy({ left: m.dx, top: m.dy, behavior: "instant" });
    }
    await new Promise((r) => requestAnimationFrame(() => r()));
  }
  return {};
}"#;

/// Text typed where the keyboard is, as an input method would put it.
const INSERT: &str = r#"async (text) => {
  const el = document.activeElement;
  if (!document.execCommand("insertText", false, text) && el && typeof el.setRangeText === "function") {
    el.setRangeText(text, el.selectionStart ?? el.value.length, el.selectionEnd ?? el.value.length, "end");
    el.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: text }));
  }
  return {};
}"#;

/// A key event to where the keyboard is. A page's own key event does
/// nothing by itself, so what the key does when the page lets it is done
/// here: typing, Enter, deleting, Tab between fields, scrolling.
const KEY: &str = r#"async (k) => {
  const el = document.activeElement || document.body;
  const mods = {
    altKey: !!(k.modifiers & 1), ctrlKey: !!(k.modifiers & 2),
    metaKey: !!(k.modifiers & 4), shiftKey: !!(k.modifiers & 8),
  };
  const init = { bubbles: true, cancelable: true, composed: true, key: k.key, code: k.code, keyCode: k.keyCode, which: k.keyCode, ...mods };
  if (k.type === "keyUp") { el.dispatchEvent(new KeyboardEvent("keyup", init)); return {}; }
  if (k.type === "char") {
    if (el.dispatchEvent(new KeyboardEvent("keypress", { ...init, charCode: k.text.charCodeAt(0) }))) document.execCommand("insertText", false, k.text);
    return {};
  }
  if (!el.dispatchEvent(new KeyboardEvent("keydown", init))) return {};
  const editable = el.isContentEditable || el.tagName === "TEXTAREA"
    || (el.tagName === "INPUT" && !/^(button|submit|reset|checkbox|radio|file|image|range|color)$/i.test(el.type));
  const shortcut = mods.ctrlKey || mods.metaKey || mods.altKey;
  if (k.key === "Enter") {
    if (el.dispatchEvent(new KeyboardEvent("keypress", { ...init, charCode: 13 }))) {
      if (el.tagName === "TEXTAREA" || el.isContentEditable) document.execCommand("insertLineBreak");
      else if (el.tagName === "INPUT" && el.form) {
        const submit = el.form.querySelector("button:not([type]), button[type=submit], input[type=submit]");
        if (submit) submit.click(); else el.form.requestSubmit();
      } else if (/^(A|BUTTON)$/.test(el.tagName) || el.getAttribute("role") === "button") el.click();
    }
  } else if (k.text && !shortcut) {
    if (el.dispatchEvent(new KeyboardEvent("keypress", { ...init, charCode: k.text.charCodeAt(0) }))) {
      if (editable) document.execCommand("insertText", false, k.text);
      else if (k.key === " " && /^(BUTTON|INPUT)$/.test(el.tagName)) el.click();
    }
  } else if (k.key === "Backspace" && editable) {
    document.execCommand("delete");
  } else if (k.key === "Delete" && editable) {
    document.execCommand("forwardDelete");
  } else if (k.key === "a" && (mods.ctrlKey || mods.metaKey)) {
    document.execCommand("selectAll");
  } else if (k.key === "Tab") {
    const all = Array.from(document.querySelectorAll("a[href], button, input, select, textarea, [tabindex], [contenteditable]"))
      .filter((n) => !n.disabled && n.tabIndex >= 0 && n.getClientRects().length);
    const at = all.indexOf(el);
    const next = all[(at + (mods.shiftKey ? all.length - 1 : 1)) % all.length];
    if (next) next.focus();
  } else if (!editable) {
    const page = innerHeight * 0.875;
    const by = { ArrowDown: 40, ArrowUp: -40, PageDown: page, PageUp: -page, " ": page }[k.key];
    if (by) scrollBy({ top: by, behavior: "instant" });
    else if (k.key === "Home") scrollTo({ top: 0, behavior: "instant" });
    else if (k.key === "End") scrollTo({ top: document.documentElement.scrollHeight, behavior: "instant" });
  }
  return {};
}"#;

const METRICS: &str = r#"async () => {
  const d = document.documentElement;
  const width = Math.max(d.scrollWidth, innerWidth), height = Math.max(d.scrollHeight, innerHeight);
  const viewport = { pageX: scrollX, pageY: scrollY, clientWidth: innerWidth, clientHeight: innerHeight };
  return {
    cssContentSize: { x: 0, y: 0, width, height },
    contentSize: { x: 0, y: 0, width, height },
    cssLayoutViewport: viewport,
    layoutViewport: viewport,
    cssVisualViewport: { ...viewport, offsetX: 0, offsetY: 0, scale: 1, zoom: 1 },
  };
}"#;

const NAVIGATE: &str = r#"async (url) => { location.href = url; return { frameId: "main" }; }"#;

const RELOAD: &str = r#"async () => { location.reload(); return {}; }"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn body(p: Plan) -> String {
        match p {
            Plan::Script { body, .. } => body,
            Plan::Picture { .. } => panic!("a picture, not a script"),
        }
    }

    #[test]
    fn an_expression_runs_as_it_is_and_statements_through_eval() {
        let Ok(Plan::Script { body, fallback }) =
            plan("Runtime.evaluate", &json!({ "expression": "document.title" }))
        else {
            panic!("not a script");
        };
        assert!(body.contains("v = await (\ndocument.title\n);"));
        assert!(!body.contains("eval"));
        assert!(fallback.unwrap().contains(r#"(0, eval)("document.title")"#));
        assert!(plan("Runtime.evaluate", &json!({})).is_err());
    }

    #[test]
    fn input_calls_carry_their_event() {
        let b = body(
            plan(
                "Input.dispatchMouseEvent",
                &json!({ "type": "mousePressed", "x": 10, "y": 20, "button": "left", "clickCount": 2 }),
            )
            .unwrap(),
        );
        assert!(b.contains(r#""count":2"#));
        assert!(b.contains(r#""type":"mousePressed""#));
        assert!(plan("Input.dispatchMouseEvent", &json!({ "type": "touch" })).is_err());
        let b = body(plan("Input.insertText", &json!({ "text": "a\"b" })).unwrap());
        assert!(b.contains(r#"("a\"b")"#));
        let b = body(
            plan(
                "Input.dispatchKeyEvent",
                &json!({ "type": "keyDown", "key": "Enter", "windowsVirtualKeyCode": 13, "text": "\r" }),
            )
            .unwrap(),
        );
        assert!(b.contains(r#""keyCode":13"#));
    }

    #[test]
    fn a_screenshot_is_a_picture() {
        assert_eq!(
            plan("Page.captureScreenshot", &json!({ "format": "png" })),
            Ok(Plan::Picture {
                clip: None,
                beyond: false
            })
        );
        let p = plan(
            "Page.captureScreenshot",
            &json!({ "clip": { "x": 1, "y": 2, "width": 3, "height": 4, "scale": 1 }, "captureBeyondViewport": true }),
        );
        assert_eq!(
            p,
            Ok(Plan::Picture {
                clip: Some(Clip {
                    x: 1.0,
                    y: 2.0,
                    width: 3.0,
                    height: 4.0
                }),
                beyond: true
            })
        );
        assert!(plan("Page.captureScreenshot", &json!({ "format": "jpeg" })).is_err());
    }

    #[test]
    fn what_webkit_can_not_do_says_so() {
        let e = plan("Network.getCookies", &json!({})).unwrap_err();
        assert!(e.contains("Network.getCookies"));
        assert!(e.contains("browser_evaluate"));
    }

    #[test]
    fn a_shot_in_view_is_taken_where_it_is() {
        let c = Clip {
            x: 10.0,
            y: 120.0,
            width: 50.0,
            height: 40.0,
        };
        assert_eq!(
            shot(c, (0.0, 100.0), (800.0, 600.0), 12_000.0),
            Shot::InView(Clip {
                x: 10.0,
                y: 20.0,
                width: 50.0,
                height: 40.0
            })
        );
        // Past the bottom: the viewport grows to hold it.
        let tall = Clip {
            height: 2000.0,
            ..c
        };
        assert_eq!(
            shot(tall, (0.0, 100.0), (800.0, 600.0), 12_000.0),
            Shot::Grow {
                grow: 2000.0,
                top: 120.0
            }
        );
        let page = Clip {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 50_000.0,
        };
        assert_eq!(
            shot(page, (0.0, 0.0), (800.0, 600.0), 12_000.0),
            Shot::Grow {
                grow: 12_000.0,
                top: 0.0
            }
        );
    }

    #[test]
    fn errors_read_as_the_protocol_has_them() {
        assert_eq!(thrown("boom")["exceptionDetails"]["text"], "boom");
        assert!(unparsed("SyntaxError: Unexpected token ';'"));
        assert!(!unparsed("TypeError: x is undefined"));
    }
}
