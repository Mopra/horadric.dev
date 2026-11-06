//! The browser pane's half that is the same on both systems: which tab a
//! session's agent drives, what typed text means as an address, how the
//! tabs move, and the agent's calls as `horadric mcp` sends them. Windows
//! shows the pages in WebView2 (`web`), the Mac in WebKit (`mac/web`).

use serde_json::{json, Value};

/// What every Claude Code session given the browser tools is told about
/// them. The server's own instructions were not enough: Claude Code defers
/// MCP tools behind a search, and an agent that thinks of Playwright or
/// Chrome first never looks for them.
pub const AGENT_PROMPT: &str = "This session runs in Horadric, which gives it a browser pane \
beside the user's terminals: the mcp__horadric__browser_* tools (browser_open, \
browser_navigate, browser_snapshot, browser_click, browser_type, browser_screenshot and more). \
Use them whenever a task needs a web page, testing a site you built included, in place of \
Playwright, Puppeteer, the Chrome DevTools MCP, Claude in Chrome, or starting Chrome, Edge or a \
headless browser yourself. The user sees that pane, and not a browser you start. Use another \
browser only when the user asks for one. The pane is also your hands in web dashboards: when a \
task means changing a setting in a service's console (turning on sign-in methods in Clerk, an \
environment variable in Vercel, a DNS record, an OAuth app), go and do it there yourself instead \
of writing the user a list of steps. The user is usually logged in already. Stop and ask only \
when a login needs their password or a code, or before a step that costs money, deletes \
something or cannot be undone.";

/// Keeps what every page writes to its console, and its errors, the last
/// 500, for an agent to read by dispatching a `horadric-console` event.
/// Bot checks like GoDaddy's look for traces of automation, so nothing is
/// left on `window` and the console's functions are proxies: they keep
/// their names and read as native code.
pub const KEEP_CONSOLE: &str = r#"(() => {
  const kept = [];
  addEventListener("horadric-console", (e) => {
    e.detail.out = kept.slice();
    if (e.detail.clear) kept.length = 0;
  });
  const say = (a) => {
    if (typeof a === "string") return a;
    if (a instanceof Error) return a.stack || String(a);
    try { return JSON.stringify(a); } catch { return String(a); }
  };
  const keep = (level, args) => {
    kept.push({ level, text: Array.from(args, say).join(" ").slice(0, 2000), at: Date.now() });
    if (kept.length > 500) kept.shift();
  };
  for (const level of ["log", "info", "warn", "error", "debug"]) {
    console[level] = new Proxy(console[level], {
      apply(was, self, args) { keep(level, args); return Reflect.apply(was, self, args); },
    });
  }
  addEventListener("error", (e) => keep("error", [e.error || e.message]));
  addEventListener("unhandledrejection", (e) => keep("error", ["Unhandled rejection:", e.reason]));
})();"#;

/// Which tab a session's agent works in, from the session driving each tab
/// (None where no live one does) and the one shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// The tab at this place: its own, or the shown one lent for a look.
    At(usize),
    /// The shown tab, which no agent has, becomes this session's.
    Claim(usize),
    /// A tab of its own, opened beside the others.
    New,
    /// The project has no page.
    Nothing,
}

/// A session keeps to the tab it works in, whichever one the user shows.
/// Without one it takes the shown tab when it is blank and no other agent
/// has it. A shown page no agent has is the user's: taking it would lose
/// their work the moment the agent opens a page of its own. So, as with
/// another agent's tab, to open a page it gets a tab of its own, and to
/// read or act on one it looks at the shown tab without taking it, which
/// is what "look at this page" means.
pub fn pick_tab(
    drivers: &[Option<&str>],
    active: usize,
    shown_blank: bool,
    session: &str,
    make: bool,
) -> Pick {
    if let Some(i) = drivers.iter().position(|d| *d == Some(session)) {
        return Pick::At(i);
    }
    match drivers.get(active) {
        Some(None) if shown_blank => Pick::Claim(active),
        _ if make => Pick::New,
        Some(_) => Pick::At(active),
        None => Pick::Nothing,
    }
}

/// Whether a tab shows no page and is not on its way to one.
pub fn is_blank(url: &str, pending: Option<&str>) -> bool {
    pending.is_none_or(|p| p == "about:blank") && (url.is_empty() || url == "about:blank")
}

/// Which tab is shown after `closed` of `len` is closed while `active` was:
/// the same one where it is still there, the one after a closed shown tab,
/// or the one before when it was the last.
pub fn after_close(active: usize, closed: usize, len: usize) -> usize {
    if closed < active {
        active - 1
    } else if closed == active {
        active.min(len.saturating_sub(2))
    } else {
        active
    }
}

/// Which place the shown tab, at `active`, has after the tab at `from`
/// moves to `to`: the moved one's new place when it is that one, else a
/// step over when the move crossed it.
pub fn after_move(active: usize, from: usize, to: usize) -> usize {
    if active == from {
        to
    } else if from < active && active <= to {
        active - 1
    } else if to <= active && active < from {
        active + 1
    } else {
        active
    }
}

/// The tab after `active` of `len`, or before it, round from the end to
/// the start as in a browser.
pub fn cycled(active: usize, len: usize, forward: bool) -> usize {
    match (len, forward) {
        (0, _) => 0,
        (_, true) => (active + 1) % len,
        (_, false) => (active + len - 1) % len,
    }
}

/// What a tab is called: its page's title, else its address without the
/// scheme, else New tab.
pub fn tab_name(title: &str, url: &str) -> String {
    if !title.trim().is_empty() && title != url {
        return title.trim().to_string();
    }
    let bare = url.split_once("://").map_or(url, |(_, rest)| rest);
    if bare.is_empty() || url == "about:blank" {
        "New tab".to_string()
    } else {
        bare.trim_end_matches('/').to_string()
    }
}

/// A step through a tab's history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Back,
    Forward,
    Reload,
}

/// What was typed into the address prompt, as an address: as it is with a
/// scheme, a local server over http, a host over https, anything else a
/// search.
pub fn address(typed: &str) -> Option<String> {
    let t = normalised(typed);
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    if t.contains("://") || t.starts_with("about:") || t.starts_with("file:") {
        return opens(t).then(|| t.to_string());
    }
    let host = t.split(['/', '?', '#']).next().unwrap_or(t);
    let name = host.rsplit_once(':').map_or(host, |(h, _)| h);
    let local = name == "localhost" || name == "127.0.0.1" || name == "[::1]" || name == "0.0.0.0";
    if local {
        return Some(format!("http://{t}"));
    }
    let hostlike = !t.contains(char::is_whitespace)
        && (name.contains('.') || host.contains(':'))
        && !name.starts_with('.')
        && !name.ends_with('.');
    if hostlike {
        return Some(format!("https://{t}"));
    }
    let q: String = t
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    Some(format!("https://www.google.com/search?q={q}"))
}

/// One call from `horadric mcp`, as the listener hands it on.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    Info,
    /// Open the pane, and a tab of the session's own at the address when
    /// one is given.
    Open(Option<String>),
    Navigate(String),
    History(Step),
    Close,
    /// A DevTools protocol method and its parameters.
    DevTools(String, Value),
    /// Type a kept web login into the page: its name, the session's key,
    /// and the refs of the fields to fill, as `browser_fill_login` names
    /// them. The values stay in the app.
    #[cfg(windows)]
    FillLogin {
        name: String,
        key: String,
        user: Option<String>,
        password: Option<String>,
        code: Option<String>,
    },
}

impl Call {
    /// The call in `body`, with addresses as [`address`] reads them, or
    /// what the agent is told is wrong with it.
    pub fn parse(body: &Value) -> Result<Call, String> {
        let text = |k: &str| body.get(k).and_then(Value::as_str);
        Ok(match text("op") {
            Some("info") => Call::Info,
            Some("open") => Call::Open(text("url").and_then(address).map(no_script).transpose()?),
            Some("navigate") => Call::Navigate(no_script(
                text("url").and_then(address).ok_or("say where: a url")?,
            )?),
            Some("back") => Call::History(Step::Back),
            Some("forward") => Call::History(Step::Forward),
            Some("reload") => Call::History(Step::Reload),
            Some("close") => Call::Close,
            Some("devtools") => {
                let method = text("method").ok_or("say which DevTools method")?;
                let params = body.get("params").cloned().unwrap_or_else(|| json!({}));
                let mut params = devtools_allowed(method, params)?;
                if method == "Page.navigate" {
                    let url = no_script(params["url"].as_str().unwrap_or_default().to_string())?;
                    params["url"] = json!(url);
                }
                Call::DevTools(method.to_string(), params)
            }
            #[cfg(windows)]
            Some("fill_login") => {
                let name = text("name").ok_or("say which login: name")?;
                let field = |k: &str| text(k).filter(|r| !r.is_empty()).map(str::to_string);
                let (user, password, code) = (field("user"), field("password"), field("code"));
                if user.is_none() && password.is_none() && code.is_none() {
                    return Err("say which fields to fill: the refs of user, password or code \
                                from browser_snapshot"
                        .into());
                }
                if user.is_some() && password.is_none() {
                    return Err("a user name is filled with its password: give the password \
                                field's ref too"
                        .into());
                }
                Call::FillLogin {
                    name: name.to_string(),
                    key: text("key").unwrap_or_default().to_string(),
                    user,
                    password,
                    code,
                }
            }
            _ => return Err("unknown browser call".into()),
        })
    }
}

/// An address as a URL parser reads it: tabs and line breaks dropped
/// wherever they are, and control characters and spaces cut from both
/// ends. `java\tscript:` is `javascript:` to the browser, so it must be to
/// the check too.
fn normalised(url: &str) -> String {
    let kept: String = url.chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    kept.trim_matches(|c: char| c <= ' ').to_string()
}

/// Whether an address is one a tab may be sent to: http, https, a file,
/// or a blank page. A `javascript:` or `data:` address runs the agent's
/// own script in the page, which a page holding a filled login must never
/// do, and nothing else needs one.
fn opens(url: &str) -> bool {
    let u = normalised(url).to_ascii_lowercase();
    u.starts_with("http://")
        || u.starts_with("https://")
        || u.starts_with("file:")
        || u == "about:blank"
}

fn no_script(url: String) -> Result<String, String> {
    if !opens(&url) {
        return Err(
            "only http, https, file and about:blank addresses are opened; to run a script, \
             use browser_evaluate"
                .into(),
        );
    }
    Ok(normalised(&url))
}

/// Where a tab stands with web logins, for what an agent may call on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filled {
    Never,
    /// A login was filled in an earlier page of this tab.
    Before,
    /// The page it shows now holds a filled login.
    Now,
}

/// The Page methods that read nothing of the page's own text or memory.
const QUIET_PAGE: &[&str] = &[
    "Page.enable",
    "Page.disable",
    "Page.navigate",
    "Page.reload",
    "Page.stopLoading",
    "Page.getNavigationHistory",
    "Page.getFrameTree",
    "Page.getLayoutMetrics",
    "Page.captureScreenshot",
    "Page.bringToFront",
    "Page.handleJavaScriptDialog",
];

/// Whether a DevTools call neither reads the page's fields nor puts the
/// agent's own code into it: input, the quiet Page calls, emulation, and
/// the mcp's own page scripts (with refs only, when `refs_only`). Such a
/// call is allowed on a page holding a filled login, and does not keep
/// one from being filled.
pub fn quiet(method: &str, params: &Value, refs_only: bool) -> bool {
    use horadric_core::vault::web::page;
    if method.starts_with("Input.") || method.starts_with("Emulation.") {
        return true;
    }
    if QUIET_PAGE.contains(&method) {
        return true;
    }
    if method != "Runtime.evaluate" {
        return false;
    }
    let Some(p) = params.as_object() else {
        return false;
    };
    // includeCommandLineAPI, a context of another frame, or an object
    // handle back would each reach past the script itself.
    let plain = p.iter().all(|(k, v)| match k.as_str() {
        "expression" => true,
        "returnByValue" => v == true,
        "awaitPromise" | "userGesture" => v.is_boolean(),
        _ => false,
    });
    let Some(expression) = p.get("expression").and_then(Value::as_str) else {
        return false;
    };
    plain
        && if refs_only {
            page::safe_after_fill(expression)
        } else {
            page::known(expression)
        }
}

/// Whether an agent may make a DevTools call on a tab where a login was
/// filled, or why not. While the page holding it shows, only quiet calls
/// pass: a script, the DOM, the console, the accessibility tree or a DOM
/// snapshot could each hand the agent the password. Reading a request's
/// body or the page's heap is refused for the tab's whole life, since the
/// sign in request and the old page's memory still hold it after the page
/// is left.
pub fn after_fill(method: &str, params: &Value, state: Filled) -> Result<(), String> {
    if state == Filled::Never {
        return Ok(());
    }
    let sticky = method == "Network.getRequestPostData"
        || method == "Network.getResponseBody"
        || method.starts_with("HeapProfiler.");
    if sticky {
        return Err(format!(
            "{method} is refused in this tab: a login was filled in it, and the sign in \
             request or the page's memory still holds the password"
        ));
    }
    // The app pins its checks to the document they were made on through
    // Runtime's reports, and keeps service workers out of the tab's
    // requests through Network, which these would stop.
    if method == "Runtime.disable" || method == "Network.disable" {
        return Err(format!("{method} is refused in this tab: a login was filled in it"));
    }
    if state == Filled::Now && !quiet(method, params, true) {
        return Err(format!(
            "{method} is refused on this page: Horadric filled a login here, and until the \
             page is left only browser_snapshot, browser_click, browser_press, browser_scroll, \
             browser_screenshot and browser_wait (by ref, no selectors) work. Sign in, or \
             navigate away."
        ));
    }
    if state == Filled::Now {
        input_after_fill(method, params)?;
    }
    Ok(())
}

/// The input a page holding a filled login refuses: a shortcut (Ctrl, Alt
/// or the Windows key, an editing command, Shift with Delete or Insert)
/// could copy a revealed password to the clipboard, a right click opens a
/// menu with Copy in it, and typed or dropped text is not needed to sign
/// in. Plain keys and left clicks pass.
fn input_after_fill(method: &str, p: &Value) -> Result<(), String> {
    let refuse = |what: &str| {
        Err(format!(
            "{what} is refused on this page: Horadric filled a login here, and it could copy \
             the password out. Click the sign in button, or navigate away."
        ))
    };
    match method {
        "Input.insertText" | "Input.imeSetComposition" => refuse("typing text"),
        "Input.dispatchDragEvent" => refuse("dragging"),
        "Input.dispatchKeyEvent" => {
            let modifiers = p["modifiers"].as_u64().unwrap_or(0);
            let key = p["key"].as_str().unwrap_or("");
            let code = p["code"].as_str().unwrap_or("");
            let vk = p["windowsVirtualKeyCode"]
                .as_u64()
                .or_else(|| p["nativeVirtualKeyCode"].as_u64())
                .unwrap_or(0);
            let commands = p["commands"].as_array().is_some_and(|c| !c.is_empty())
                || p["commands"].is_string();
            let held = ["Control", "Meta", "Alt", "AltGraph", "OS", "ContextMenu"];
            let shortcut = modifiers & 0b111 != 0
                || commands
                || held.contains(&key)
                || held.iter().any(|h| code.starts_with(h))
                || matches!(vk, 17 | 18 | 91 | 92 | 93 | 162..=165)
                || (modifiers & 8 != 0
                    && (matches!(key, "Delete" | "Insert") || matches!(vk, 45 | 46)));
            if shortcut {
                refuse("a key with Ctrl, Alt or the Windows key, or an editing command")
            } else {
                Ok(())
            }
        }
        "Input.dispatchMouseEvent" | "Input.emulateTouchFromMouseEvent" => {
            match p["button"].as_str() {
                Some("right" | "middle") => refuse("a right or middle click"),
                _ => Ok(()),
            }
        }
        _ => Ok(()),
    }
}

/// The calls that only move the tab on or look at where it is, allowed on
/// a page holding a filled login even when its password shows: the way
/// off the page.
pub fn moves_only(method: &str) -> bool {
    matches!(
        method,
        "Page.navigate"
            | "Page.reload"
            | "Page.stopLoading"
            | "Page.getNavigationHistory"
            | "Page.getFrameTree"
            | "Page.enable"
            | "Page.disable"
            | "Page.bringToFront"
            | "Page.handleJavaScriptDialog"
    )
}

/// The Runtime calls that run nothing and read nothing of the page.
const RUNTIME_QUIET: &[&str] = &[
    "Runtime.enable",
    "Runtime.getHeapUsage",
    "Runtime.getIsolateId",
    "Runtime.releaseObject",
    "Runtime.releaseObjectGroup",
    "Runtime.discardConsoleEntries",
];

/// Whether a DevTools call can run the agent's own code in the tab, or
/// leave something there that outlasts the page: a script, a function
/// called on a page object, a binding every later page gets, or an event
/// handler written into the DOM. Code run in a page can register a
/// service worker or poison what later pages of its site load, so such a
/// call taints the whole tab, not only its page, and no login is filled
/// in it again until it is closed. The mcp's own scripts are not the
/// agent's code.
pub fn taints(method: &str, params: &Value) -> bool {
    if method == "Runtime.evaluate" {
        return !quiet(method, params, false);
    }
    if method.starts_with("Runtime.") {
        return !RUNTIME_QUIET.contains(&method);
    }
    if let Some(name) = method.strip_prefix("DOM.") {
        return (name.starts_with("set")
            && !matches!(name, "setInspectedNode" | "setNodeStackTracesEnabled"))
            || matches!(name, "copyTo" | "moveTo" | "undo" | "redo");
    }
    false
}

/// Whole DevTools domains an agent may use: each acts on its own tab's
/// page and nothing beyond it. Debugger, DOMDebugger and the profilers are
/// out: a breakpoint's condition is a script that outlives the page and
/// runs in every later one, a keylogger on a login page the app never saw
/// the agent touch. Of what is here, only Runtime and the DOM's setters
/// can put the agent's code in a page (`taints`); Emulation, Overlay,
/// Animation and the Network calls below may last past a page but run no
/// code.
const PAGE_DOMAINS: &[&str] = &[
    "Accessibility",
    "Animation",
    "Console",
    "CSS",
    "DOM",
    "DOMSnapshot",
    "Emulation",
    "Input",
    "LayerTree",
    "Log",
    "Overlay",
    "Performance",
    "Runtime",
];

/// Single methods of domains that also reach the whole browser (Page,
/// Network), picked one by one, each with the parameters it may take. A
/// parameter not named is refused: Page.reload's `scriptToEvaluateOnLoad`
/// would put the agent's script into the reloaded page unseen, and
/// Page.navigate's `frameId` would aim a navigation at a frame. None of
/// the parameters here takes a script, a file path or a URL to load but
/// Page.navigate's own, which `no_script` checks. Network.setBypassService
/// Worker is the app's: it holds it on after a fill.
const PAGE_METHODS: &[(&str, &[&str])] = &[
    ("Page.enable", &[]),
    ("Page.disable", &[]),
    ("Page.navigate", &["url", "referrer"]),
    ("Page.reload", &["ignoreCache"]),
    ("Page.stopLoading", &[]),
    (
        "Page.captureScreenshot",
        &["format", "quality", "clip", "fromSurface", "captureBeyondViewport", "optimizeForSpeed"],
    ),
    ("Page.getLayoutMetrics", &[]),
    ("Page.getFrameTree", &[]),
    ("Page.getResourceTree", &[]),
    ("Page.getResourceContent", &["frameId", "url"]),
    ("Page.getNavigationHistory", &[]),
    (
        "Page.printToPDF",
        &[
            "landscape", "printBackground", "scale", "paperWidth", "paperHeight", "marginTop",
            "marginBottom", "marginLeft", "marginRight", "pageRanges", "preferCSSPageSize",
            "generateTaggedPDF", "generateDocumentOutline",
        ],
    ),
    ("Page.bringToFront", &[]),
    ("Page.handleJavaScriptDialog", &["accept", "promptText"]),
    ("Network.enable", &["maxTotalBufferSize", "maxResourceBufferSize", "maxPostDataSize"]),
    ("Network.disable", &[]),
    ("Network.getResponseBody", &["requestId"]),
    ("Network.getRequestPostData", &["requestId"]),
    ("Network.setCacheDisabled", &["cacheDisabled"]),
    (
        "Network.emulateNetworkConditions",
        &["offline", "latency", "downloadThroughput", "uploadThroughput", "connectionType"],
    ),
    ("Network.setExtraHTTPHeaders", &["headers"]),
    ("Network.setUserAgentOverride", &["userAgent", "acceptLanguage", "platform", "userAgentMetadata"]),
];

/// Methods of the allowed domains that reach the user's files: each puts a
/// local path into the page, where any site could upload it (the Claude
/// login in ~/.claude, say).
const FILE_METHODS: &[&str] = &[
    "DOM.setFileInputFiles",
    "DOM.getFileInfo",
    "Input.dispatchDragEvent",
];

/// Whether an agent may send DevTools `method`, and the params it goes
/// with. The pane holds the user's own logins for every site, and an
/// agent can be talked into anything by a page it reads, so only what
/// acts on its own tab passes: no cookie jar, storage, downloads, other
/// targets or the browser itself.
pub fn devtools_allowed(method: &str, params: Value) -> Result<Value, String> {
    let refuse = |why: &str| Err(format!("{method} is not allowed for agents: {why}"));
    let Some((domain, name)) = method.split_once('.') else {
        return Err(format!("{method} is not a DevTools method (Domain.method)"));
    };
    if name == "setDownloadBehavior" {
        return refuse("it would send the user's downloads elsewhere");
    }
    match domain {
        "Storage" | "Browser" | "Target" | "Fetch" | "SystemInfo" | "ServiceWorker" => {
            return refuse("it reaches past this tab into the whole browser")
        }
        "Debugger" | "DOMDebugger" | "Profiler" | "HeapProfiler" => {
            return refuse("what it sets lasts past the page and runs in later ones")
        }
        _ => {}
    }
    if matches!(
        method,
        "Page.addScriptToEvaluateOnNewDocument" | "Network.setRequestInterception"
    ) {
        return refuse("it puts code or a hand into every page the tab opens later");
    }
    if FILE_METHODS.contains(&method) {
        return refuse("it hands the page files from the user's disk");
    }
    if let Some((_, takes)) = PAGE_METHODS.iter().find(|(m, _)| *m == method) {
        let extra = match &params {
            Value::Null => None,
            Value::Object(o) => o.keys().find(|k| !takes.contains(&k.as_str())).cloned(),
            _ => return refuse("its params must be an object"),
        };
        if let Some(k) = extra {
            return refuse(&format!(
                "it takes only {} here, not {k}",
                if takes.is_empty() { "no params".to_string() } else { takes.join(", ") }
            ));
        }
        return Ok(params);
    }
    // Audited for parameters that take a script, a URL to load or a file
    // path: Runtime's run the agent's code and so taint (`taints`), the
    // DOM's file calls and Input's drag are refused above, and the rest
    // (CSS text, emulation, overlays, reads) load nothing and run nothing.
    if PAGE_DOMAINS.contains(&domain) {
        return Ok(params);
    }
    // Even without urls, getCookies answers for every frame on the page,
    // and the agent can frame or navigate to any site the user is logged
    // in to, so no cookie call is safe.
    if domain == "Network" && name.to_ascii_lowercase().contains("cookie") {
        return refuse("it reads or changes the user's logins on other sites");
    }
    refuse("only methods that act on this tab's page are (Runtime, DOM, CSS, Input, Emulation, Log, Console, Performance, Accessibility, and Page and Network calls on this page)")
}

/// Whether `step` can be taken in a tab whose history goes `back` and
/// `forward` as said, or why not.
pub fn can_step(step: Step, back: bool, forward: bool) -> Result<(), String> {
    match step {
        Step::Back if !back => Err("there is nothing to go back to".into()),
        Step::Forward if !forward => Err("there is nothing to go forward to".into()),
        _ => Ok(()),
    }
}

/// Where a session's tab stands, as an agent is told: whether the project
/// has a page, the tab's place of how many (`place`: at, of, shown in its
/// pane), whether the user can see it, and its page.
pub fn info(
    open: bool,
    pane_shown: bool,
    project: &str,
    place: Option<(usize, usize, bool)>,
    title: &str,
    url: &str,
) -> Value {
    json!({
        "open": open,
        "shown": pane_shown && place.is_some_and(|(_, _, front)| front),
        "project": project,
        "tab": place.map(|(at, _, _)| at + 1),
        "tabs": place.map(|(_, n, _)| n),
        "url": url,
        "title": title,
    })
}

pub fn closed() -> Value {
    error("the browser is not open; open it first")
}

pub fn error(why: &str) -> Value {
    json!({ "error": why })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_addresses_become_urls() {
        assert_eq!(address("  "), None);
        assert_eq!(
            address("https://example.com/a").as_deref(),
            Some("https://example.com/a")
        );
        assert_eq!(address("about:blank").as_deref(), Some("about:blank"));
        assert_eq!(
            address("localhost:3000/login").as_deref(),
            Some("http://localhost:3000/login")
        );
        assert_eq!(
            address("127.0.0.1:8080").as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert_eq!(
            address("example.com").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            address("rust borrow checker").as_deref(),
            Some("https://www.google.com/search?q=rust+borrow+checker")
        );
        assert_eq!(
            address("c++ & rust").as_deref(),
            Some("https://www.google.com/search?q=c%2B%2B+%26+rust")
        );
        // A word with no dot is a search, not a host.
        assert_eq!(
            address("horadric").as_deref(),
            Some("https://www.google.com/search?q=horadric")
        );
    }

    #[test]
    fn closing_a_tab_shows_its_neighbour() {
        // Before the shown one: the same page, one place left.
        assert_eq!(after_close(2, 0, 4), 1);
        // The shown one: the one after it takes its place.
        assert_eq!(after_close(1, 1, 4), 1);
        // The shown one, last: the one before it.
        assert_eq!(after_close(3, 3, 4), 2);
        // After the shown one: nothing moves.
        assert_eq!(after_close(1, 3, 4), 1);
    }

    #[test]
    fn an_agent_keeps_to_its_own_tab() {
        // Its own tab, though the user shows another.
        assert_eq!(
            pick_tab(&[Some("a"), None], 1, false, "a", true),
            Pick::At(0)
        );
        // None of its own: the blank shown tab no agent has becomes its.
        assert_eq!(
            pick_tab(&[Some("b"), None], 1, true, "a", true),
            Pick::Claim(1)
        );
        assert_eq!(
            pick_tab(&[Some("b"), None], 1, true, "a", false),
            Pick::Claim(1)
        );
        // The shown tab is another agent's: a tab of its own to open a
        // page, a look at the shown one to read.
        assert_eq!(pick_tab(&[Some("b"), None], 0, false, "a", true), Pick::New);
        assert_eq!(
            pick_tab(&[Some("b"), None], 0, false, "a", false),
            Pick::At(0)
        );
        // No page at all.
        assert_eq!(pick_tab(&[], 0, true, "a", true), Pick::New);
        assert_eq!(pick_tab(&[], 0, true, "a", false), Pick::Nothing);
    }

    #[test]
    fn an_agent_leaves_the_users_page_alone() {
        // The user's page: looked at in place, never taken.
        assert_eq!(pick_tab(&[None], 0, false, "a", false), Pick::At(0));
        assert_eq!(pick_tab(&[None], 0, false, "a", true), Pick::New);
    }

    #[test]
    fn a_tab_is_blank_without_a_page_or_one_coming() {
        assert!(is_blank("", None));
        assert!(is_blank("about:blank", None));
        assert!(is_blank("", Some("about:blank")));
        assert!(!is_blank("", Some("https://example.com")));
        assert!(!is_blank("https://example.com/", None));
    }

    #[test]
    fn the_shown_tab_stays_shown_through_a_move() {
        let order = |from: usize, to: usize| {
            let mut v = vec!['a', 'b', 'c', 'd'];
            let t = v.remove(from);
            v.insert(to, t);
            v
        };
        for (from, to) in [(0, 3), (3, 0), (1, 2), (2, 1), (1, 1)] {
            for active in 0..4 {
                let was = ['a', 'b', 'c', 'd'][active];
                assert_eq!(order(from, to)[after_move(active, from, to)], was);
            }
        }
    }

    #[test]
    fn tabs_cycle_round() {
        assert_eq!(cycled(0, 3, true), 1);
        assert_eq!(cycled(2, 3, true), 0);
        assert_eq!(cycled(0, 3, false), 2);
        assert_eq!(cycled(0, 0, true), 0);
    }

    #[test]
    fn a_tab_is_named_by_its_title_or_address() {
        assert_eq!(tab_name("Example", "https://example.com/"), "Example");
        assert_eq!(tab_name("", "https://example.com/a/"), "example.com/a");
        assert_eq!(tab_name("", "about:blank"), "New tab");
        assert_eq!(tab_name("", ""), "New tab");
        // A page with no title of its own reports its address as one.
        assert_eq!(
            tab_name("localhost:3000", "localhost:3000"),
            "localhost:3000"
        );
    }

    #[test]
    fn agent_calls_are_read_from_their_body() {
        let p = |v: Value| Call::parse(&v);
        assert_eq!(p(json!({ "op": "info" })), Ok(Call::Info));
        assert_eq!(p(json!({ "op": "open" })), Ok(Call::Open(None)));
        assert_eq!(
            p(json!({ "op": "open", "url": "localhost:3000" })),
            Ok(Call::Open(Some("http://localhost:3000".into())))
        );
        assert!(p(json!({ "op": "navigate" })).is_err());
        assert_eq!(p(json!({ "op": "back" })), Ok(Call::History(Step::Back)));
        assert_eq!(
            p(json!({ "op": "devtools", "method": "Runtime.evaluate" })),
            Ok(Call::DevTools("Runtime.evaluate".into(), json!({})))
        );
        assert!(p(json!({ "op": "devtools" })).is_err());
        assert!(p(json!({ "op": "fly" })).is_err());
        assert!(p(json!({ "op": "devtools", "method": "Network.getAllCookies" })).is_err());
    }

    #[test]
    fn agents_get_devtools_only_for_their_own_page() {
        let ok = |m: &str| devtools_allowed(m, json!({ "x": 1 }));
        for m in [
            "Runtime.evaluate",
            "Input.dispatchMouseEvent",
            "Input.insertText",
            "DOM.getDocument",
            "Emulation.setDeviceMetricsOverride",
        ] {
            assert_eq!(ok(m), Ok(json!({ "x": 1 })), "{m}");
        }
        for m in ["Page.captureScreenshot", "Page.getLayoutMetrics", "Page.navigate",
            "Network.getResponseBody", "Page.printToPDF"] {
            assert_eq!(devtools_allowed(m, json!({})), Ok(json!({})), "{m}");
            assert!(ok(m).is_err(), "{m} takes no x");
        }
        for m in [
            "Network.getAllCookies",
            "Network.getCookies",
            "DOM.setFileInputFiles",
            "DOM.getFileInfo",
            "Input.dispatchDragEvent",
            "IO.read",
            "Page.addCompilationCache",
            "Network.setCookie",
            "Network.setCookies",
            "Network.deleteCookies",
            "Network.clearBrowserCookies",
            "Storage.getCookies",
            "Browser.setDownloadBehavior",
            "Page.setDownloadBehavior",
            "Browser.close",
            "Target.createTarget",
            "Target.attachToTarget",
            "Fetch.enable",
            "SystemInfo.getInfo",
            "Network.loadNetworkResource",
            "Page.addScriptToEvaluateOnNewDocument",
            "Tracing.start",
            "Debugger.setBreakpointByUrl",
            "DOMDebugger.setEventListenerBreakpoint",
            "Profiler.start",
            "HeapProfiler.takeHeapSnapshot",
            "ServiceWorker.enable",
            "Network.setRequestInterception",
            "evaluate",
        ] {
            let e = ok(m).unwrap_err();
            assert!(e.contains(m), "{m}: {e}");
        }
    }

    #[test]
    fn history_steps_only_where_there_is_history() {
        assert!(can_step(Step::Back, false, true).is_err());
        assert!(can_step(Step::Forward, true, false).is_err());
        assert!(can_step(Step::Reload, false, false).is_ok());
        assert!(can_step(Step::Back, true, false).is_ok());
    }

    #[test]
    fn a_tab_is_shown_only_when_its_pane_is_and_it_is_in_front() {
        let i = info(true, true, "p", Some((1, 3, false)), "T", "https://x/");
        assert_eq!(i["shown"], false);
        assert_eq!(i["tab"], 2);
        assert_eq!(i["tabs"], 3);
        let i = info(true, true, "p", Some((0, 1, true)), "", "");
        assert_eq!(i["shown"], true);
        assert_eq!(info(false, false, "p", None, "", "")["tab"], Value::Null);
    }

    #[test]
    fn a_filled_page_allows_only_quiet_calls() {
        use horadric_core::vault::web::page;
        let eval = |e: &str| json!({ "expression": e, "returnByValue": true, "awaitPromise": true });
        let snap = eval(&page::snapshot());
        let click = eval(&page::call(page::POINT, &[json!("e2"), Value::Null]));
        let by_selector = eval(&page::call(page::POINT, &[Value::Null, json!("#pw")]));
        let own = eval("document.querySelector('[type=password]').value");
        for state in [Filled::Never, Filled::Before, Filled::Now] {
            assert!(after_fill("Runtime.evaluate", &snap, state).is_ok());
            assert!(after_fill("Runtime.evaluate", &click, state).is_ok());
            assert!(after_fill("Page.captureScreenshot", &json!({}), state).is_ok());
        }
        assert!(after_fill("Input.insertText", &json!({ "text": "x" }), Filled::Before).is_ok());
        assert!(after_fill("Input.insertText", &json!({ "text": "x" }), Filled::Now).is_err());
        assert!(after_fill("Runtime.evaluate", &own, Filled::Before).is_ok());
        assert!(after_fill("Runtime.evaluate", &own, Filled::Now).is_err());
        assert!(after_fill("Runtime.evaluate", &by_selector, Filled::Now).is_err());
        for m in ["DOM.getOuterHTML", "DOMSnapshot.captureSnapshot", "Accessibility.getFullAXTree",
            "Runtime.callFunctionOn", "Console.enable", "Log.enable", "Debugger.enable", "CSS.addRule"] {
            assert!(after_fill(m, &json!({}), Filled::Now).is_err(), "{m}");
            assert!(after_fill(m, &json!({}), Filled::Before).is_ok(), "{m}");
        }
        // The sign in request and the old page's heap hold it for good.
        for m in ["Network.getRequestPostData", "Network.getResponseBody", "HeapProfiler.takeHeapSnapshot"] {
            assert!(after_fill(m, &json!({}), Filled::Before).is_err(), "{m}");
            assert!(after_fill(m, &json!({}), Filled::Never).is_ok(), "{m}");
        }
        // A known script with more than plain options is not quiet.
        let mut wide = snap.clone();
        wide["includeCommandLineAPI"] = json!(true);
        assert!(after_fill("Runtime.evaluate", &wide, Filled::Now).is_err());
        // Only the agent's own code marks a page against a fill.
        assert!(quiet("Runtime.evaluate", &by_selector, false));
        assert!(!quiet("Runtime.evaluate", &own, false));
        assert!(!quiet("DOM.setAttributeValue", &json!({}), false));
    }

    #[test]
    fn javascript_addresses_are_not_opened() {
        let nav = |u: &str| Call::parse(&json!({ "op": "navigate", "url": u }));
        assert!(nav("javascript://x%0afetch('//x.example')").is_err());
        assert!(nav(" JavaScript://%0aalert(1)").is_err());
        assert!(nav("java\tscript://%0aalert(1)").is_err());
        assert!(nav("\u{1}javascript://%0aalert(1)").is_err());
        assert!(nav("jav\r\nascript://x").is_err());
        // A data: address typed is read as a host, never opened as data.
        assert!(!matches!(nav("data:text/html,<script>1</script>"), Ok(Call::Navigate(u)) if u.starts_with("data:")));
        assert!(nav("https://example.com").is_ok());
        assert!(nav("about:blank").is_ok());
        assert!(nav("file:///c:/x.html").is_ok());
        assert_eq!(nav("ht\ttps://example.com"), Ok(Call::Navigate("https://example.com".into())));
        let dev = |u: &str| {
            Call::parse(&json!({ "op": "devtools", "method": "Page.navigate", "params": { "url": u } }))
        };
        assert!(dev("javascript:1").is_err());
        assert!(dev("java\nscript:1").is_err());
        assert!(dev("vbscript:1").is_err());
        assert!(dev("data:text/html,<script>1</script>").is_err());
        assert_eq!(
            dev("\thttps://a.example"),
            Ok(Call::DevTools("Page.navigate".into(), json!({ "url": "https://a.example" })))
        );
        assert_eq!(address("java\tscript://x%0aalert(1)"), None);
    }

    #[test]
    fn page_and_network_calls_take_only_their_own_params() {
        let a = |m: &str, p: Value| devtools_allowed(m, p);
        assert!(a("Page.reload", json!({ "ignoreCache": true })).is_ok());
        assert!(a("Page.reload", json!({ "scriptToEvaluateOnLoad": "alert(1)" })).is_err());
        assert!(a("Page.navigate", json!({ "url": "https://a.example", "referrer": "https://b.example" })).is_ok());
        assert!(a("Page.navigate", json!({ "url": "https://a.example", "frameId": "F" })).is_err());
        assert!(a("Page.navigate", json!({ "url": "https://a.example", "transitionType": "link" })).is_err());
        assert!(a("Page.handleJavaScriptDialog", json!({ "accept": true, "promptText": "x" })).is_ok());
        assert!(a("Page.captureScreenshot", json!({ "format": "png", "clip": {}, "captureBeyondViewport": true })).is_ok());
        assert!(a("Page.printToPDF", json!({ "headerTemplate": "<img src=x>" })).is_err());
        assert!(a("Page.reload", json!(["x"])).is_err());
        assert!(a("Page.reload", Value::Null).is_ok());
        assert!(a("Network.setBypassServiceWorker", json!({ "bypass": false })).is_err());
        let reload = json!({ "op": "devtools", "method": "Page.reload",
            "params": { "scriptToEvaluateOnLoad": "addEventListener('input', () => 0)" } });
        assert!(Call::parse(&reload).is_err());
    }

    #[test]
    fn a_filled_page_takes_no_shortcut() {
        let key = |m: u64, k: &str| json!({ "type": "keyDown", "key": k, "modifiers": m });
        let now = |m: &str, p: Value| after_fill(m, &p, Filled::Now);
        assert!(now("Input.dispatchKeyEvent", key(0, "Enter")).is_ok());
        assert!(now("Input.dispatchKeyEvent", key(8, "Tab")).is_ok());
        assert!(now("Input.dispatchKeyEvent", key(2, "c")).is_err());
        assert!(now("Input.dispatchKeyEvent", key(4, "c")).is_err());
        assert!(now("Input.dispatchKeyEvent", key(1, "c")).is_err());
        assert!(now("Input.dispatchKeyEvent", key(0, "Control")).is_err());
        assert!(now("Input.dispatchKeyEvent", key(8, "Delete")).is_err());
        assert!(now("Input.dispatchKeyEvent", json!({ "key": "a", "commands": ["copy"] })).is_err());
        assert!(now("Input.dispatchMouseEvent", json!({ "type": "mousePressed", "button": "left" })).is_ok());
        assert!(now("Input.dispatchMouseEvent", json!({ "type": "mousePressed", "button": "right" })).is_err());
        assert!(now("Input.dispatchDragEvent", json!({})).is_err());
        assert!(after_fill("Input.dispatchKeyEvent", &key(2, "c"), Filled::Before).is_ok());
        assert!(after_fill("Runtime.disable", &json!({}), Filled::Before).is_err());
    }

    #[test]
    fn code_the_agent_writes_taints_the_tab() {
        use horadric_core::vault::web::page;
        let eval = |e: &str| json!({ "expression": e, "returnByValue": true });
        assert!(taints("Runtime.evaluate", &eval("1 + 1")));
        assert!(!taints("Runtime.evaluate", &eval(&page::snapshot())));
        assert!(!taints("Runtime.evaluate", &eval(&page::console(false))));
        for m in ["Runtime.callFunctionOn", "Runtime.compileScript", "Runtime.runScript",
            "Runtime.addBinding", "DOM.setAttributeValue", "DOM.setOuterHTML", "DOM.setAttributesAsText"] {
            assert!(taints(m, &json!({})), "{m}");
        }
        for m in ["DOM.getDocument", "CSS.addRule", "Emulation.setScriptExecutionDisabled",
            "Runtime.enable", "Page.navigate", "Input.insertText"] {
            assert!(!taints(m, &json!({})), "{m}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_fill_names_its_login_and_fields() {
        let call = Call::parse(&json!({ "op": "fill_login", "name": "OPTI", "key": "k",
            "user": "e1", "password": "e2", "code": null }))
        .unwrap();
        assert_eq!(
            call,
            Call::FillLogin {
                name: "OPTI".into(),
                key: "k".into(),
                user: Some("e1".into()),
                password: Some("e2".into()),
                code: None
            }
        );
        assert!(Call::parse(&json!({ "op": "fill_login", "name": "OPTI" })).is_err());
        assert!(Call::parse(&json!({ "op": "fill_login", "name": "OPTI", "user": "e1" })).is_err());
        assert!(Call::parse(&json!({ "op": "fill_login", "name": "OPTI", "code": "e4" })).is_ok());
    }
}
