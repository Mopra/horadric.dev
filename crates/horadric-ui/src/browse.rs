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
    let t = typed.trim();
    if t.is_empty() {
        return None;
    }
    if t.contains("://") || t.starts_with("about:") || t.starts_with("file:") {
        return Some(t.to_string());
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
}

impl Call {
    /// The call in `body`, with addresses as [`address`] reads them, or
    /// what the agent is told is wrong with it.
    pub fn parse(body: &Value) -> Result<Call, String> {
        let text = |k: &str| body.get(k).and_then(Value::as_str);
        Ok(match text("op") {
            Some("info") => Call::Info,
            Some("open") => Call::Open(text("url").and_then(address)),
            Some("navigate") => {
                Call::Navigate(text("url").and_then(address).ok_or("say where: a url")?)
            }
            Some("back") => Call::History(Step::Back),
            Some("forward") => Call::History(Step::Forward),
            Some("reload") => Call::History(Step::Reload),
            Some("close") => Call::Close,
            Some("devtools") => {
                let method = text("method").ok_or("say which DevTools method")?;
                let params = body.get("params").cloned().unwrap_or_else(|| json!({}));
                let params = devtools_allowed(method, params)?;
                Call::DevTools(method.to_string(), params)
            }
            _ => return Err("unknown browser call".into()),
        })
    }
}

/// Whole DevTools domains an agent may use: each acts on its own tab's
/// page and nothing beyond it.
const PAGE_DOMAINS: &[&str] = &[
    "Accessibility",
    "Animation",
    "Console",
    "CSS",
    "Debugger",
    "DOM",
    "DOMDebugger",
    "DOMSnapshot",
    "Emulation",
    "HeapProfiler",
    "Input",
    "LayerTree",
    "Log",
    "Overlay",
    "Performance",
    "Profiler",
    "Runtime",
];

/// Single methods of domains that also reach the whole browser (Page,
/// Network), picked one by one.
const PAGE_METHODS: &[&str] = &[
    "Page.enable",
    "Page.disable",
    "Page.navigate",
    "Page.reload",
    "Page.stopLoading",
    "Page.captureScreenshot",
    "Page.getLayoutMetrics",
    "Page.getFrameTree",
    "Page.getResourceTree",
    "Page.getResourceContent",
    "Page.getNavigationHistory",
    "Page.printToPDF",
    "Page.bringToFront",
    "Page.handleJavaScriptDialog",
    "Network.enable",
    "Network.disable",
    "Network.getResponseBody",
    "Network.getRequestPostData",
    "Network.setCacheDisabled",
    "Network.setBypassServiceWorker",
    "Network.emulateNetworkConditions",
    "Network.setExtraHTTPHeaders",
    "Network.setUserAgentOverride",
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
        "Storage" | "Browser" | "Target" | "Fetch" | "SystemInfo" => {
            return refuse("it reaches past this tab into the whole browser")
        }
        _ => {}
    }
    if FILE_METHODS.contains(&method) {
        return refuse("it hands the page files from the user's disk");
    }
    if PAGE_DOMAINS.contains(&domain) || PAGE_METHODS.contains(&method) {
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
            "Page.captureScreenshot",
            "Page.getLayoutMetrics",
            "Page.navigate",
            "DOM.getDocument",
            "Emulation.setDeviceMetricsOverride",
            "Network.getResponseBody",
            "Page.printToPDF",
        ] {
            assert_eq!(ok(m), Ok(json!({ "x": 1 })), "{m}");
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
}
