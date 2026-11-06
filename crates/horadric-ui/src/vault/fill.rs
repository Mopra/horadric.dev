//! Web logins filled in the browser pane, never read (docs/plan/vault.md,
//! "Web logins: filled, never read"). The agent names a kept login and the
//! refs of the fields; Horadric checks the tab is on one of the login's
//! own origins, that each ref is the kind of field it is said to be in the
//! page itself, and types the values in. The agent hears what was filled,
//! never a value.
//!
//! What the agent may do with the tab around a fill is kept here too:
//! every DevTools call it makes passes `devtools`, which refuses anything
//! that could read a filled field back while the page holding it shows,
//! remembers pages the agent read with more than the mcp's own scripts,
//! and remembers tabs the agent ran its own code in, since a listener or
//! a service worker it left there would hear the password typed.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use horadric_core::vault::secret::{log_line, missing_line, Mask, Owner};
use horadric_core::vault::web::{is_loopback, matches, origin, totp};
use horadric_hooks::listener::Reply;
use serde_json::{json, Value};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use super::run::{self, Answer, Asked};
use super::store::{self, Used, WebLogin};
use crate::browse::{self, error, Filled};
use crate::web;

/// How long a fill waits for the human to answer before it tells the agent
/// nobody did. Well under the listener's own wait for the app (90 s), so
/// the agent hears it rather than a timeout, and a late Always finds the
/// fill already given up.
const ASK_WAIT: Duration = Duration::from_secs(75);

/// Said for any call on a tab while a fill types into it, or a check of a
/// filled page runs: a navigation in between would carry the typing to
/// another page, or the check to another document.
pub const BUSY: &str = "Horadric is filling a login in this tab, or checking the page it filled; \
                        try again in a moment";

/// The shortest kept value masked in a browser answer. A shorter one
/// (a password `admin`, a port `3000`) turns up in pages by chance, and
/// masking it there garbles them; a filled field's value is never
/// reported anyway (`page::HELPERS`, `secret`).
const SHORTEST_MASKED: usize = 8;

/// Checks the ref names a field of the kind said, in the page itself and on
/// the origin said, and puts the keyboard in it with its text selected, to
/// be replaced. It marks the field, so the check after typing knows the
/// keyboard is still there and a screenshot knows which field to look at.
/// An `unload` listener keeps the page out of the back and forward cache,
/// so going back to it loads it afresh, empty.
const FIELD: &str = r#"(ref, kind, origin) => {
  if (location.origin !== origin) return { error: `the page moved to ${location.origin}` };
  if (window.opener) return { error: "this page was opened by another page, which could read what is typed into it; open the login page with browser_navigate and fill it there" };
  const H = (window.__horadric = window.__horadric || {});
  const refs = H.refs || [];
  const el = refs[Number(String(ref).replace(/^e/, ""))];
  if (!el || !el.isConnected) return { error: `there is no ${ref} on this page; take a new browser_snapshot` };
  if (el.ownerDocument !== document) return { error: `${ref} is in a frame; Horadric fills only the page itself` };
  const type = el.tagName === "INPUT" ? el.type : "";
  const fits = { user: ["text", "email", "tel"], password: ["password"], code: ["text", "number", "tel", "password"] }[kind];
  if (!fits.includes(type)) return { error: `${ref} is not a ${kind === "user" ? "user name" : kind} field` };
  if (kind === "code") {
    const named = (el.getAttribute("autocomplete") || "").includes("one-time-code");
    const digits = el.inputMode === "numeric";
    const short = ["number", "tel", "text"].includes(type) && el.maxLength > 0 && el.maxLength <= 10;
    if (!named && !digits && !short) return { error: `${ref} does not look like a one time code field (autocomplete="one-time-code", inputmode="numeric", or at most 10 characters long)` };
  }
  if (el.disabled || el.readOnly) return { error: `${ref} does not take typing` };
  el.scrollIntoView({ block: "center", behavior: "instant" });
  el.focus();
  if (el.getRootNode().activeElement !== el) return { error: `${ref} would not take the keyboard` };
  el.select();
  el.setAttribute("data-horadric-filled", kind);
  (H.filled = H.filled || []).push({ e: el, kind });
  if (!H.unload) { H.unload = true; addEventListener("unload", () => {}); }
  return { ok: true };
}"#;

/// After typing: the keyboard is still in the field marked for `kind` on
/// the same origin, and how many characters it holds, never what they are.
const TYPED: &str = r#"(kind, origin) => {
  let a = document.activeElement;
  while (a && a.shadowRoot && a.shadowRoot.activeElement) a = a.shadowRoot.activeElement;
  if (location.origin !== origin || !a || a.getAttribute("data-horadric-filled") !== kind) return { error: "the keyboard left the field while it was typed into" };
  return { length: String(a.value).length };
}"#;

/// Whether the filled page still hides what was filled: some filled field
/// is still on the page, and every filled password field on it is still a
/// password field, as the page's own Show password button may change. A
/// page that took the fields away, or swapped one for a copy, may show
/// the password anywhere, so that is false too. The fields are known by
/// the list FIELD keeps as well as by their mark, which the page may drop.
const HIDDEN: &str = r#"(() => {
  const H = window.__horadric || {};
  const all = (H.filled || []).slice();
  for (const e of document.querySelectorAll("[data-horadric-filled]")) all.push({ e, kind: e.getAttribute("data-horadric-filled") });
  const here = all.filter((f) => f.e && f.e.isConnected);
  return here.length > 0 && here.every((f) => f.kind !== "password" || f.e.type === "password");
})()"#;

/// Whether the page's site has a service worker, controlling this page or
/// only registered: a registered one still sees the next navigation, the
/// sign in request among them.
const WORKERS: &str = r#"(async () => {
  const s = navigator.serviceWorker;
  if (!s) return false;
  if (s.controller) return true;
  return (await s.getRegistrations()).length > 0;
})()"#;

#[derive(Default)]
struct TabState {
    /// The documents (DevTools loader ids) a login was filled in.
    filled: Vec<String>,
    /// The documents the agent read with more than the mcp's own scripts.
    marked: Vec<String>,
    /// The agent ran its own code in the tab, which may have left
    /// something behind for later pages: no login is filled in it again.
    tainted: bool,
    /// Runtime reports the tab's contexts to the app (`web::context_of`).
    tracked: bool,
    /// The origins whose service workers the app cleared before a fill.
    workers_cleared: Vec<String>,
    /// The TOTP seeds a code was filled from, by login name, so the codes
    /// near now are masked in what the tab answers.
    seeds: Vec<(String, String)>,
    busy: bool,
}

thread_local! {
    static TABS: RefCell<HashMap<u64, TabState>> = RefCell::new(HashMap::new());
}

fn with_tab<T>(tab: u64, f: impl FnOnce(&mut TabState) -> T) -> T {
    TABS.with(|t| f(t.borrow_mut().entry(tab).or_default()))
}

/// Whether a fill is typing into the tab now.
pub fn busy(tab: u64) -> bool {
    TABS.with(|t| t.borrow().get(&tab).is_some_and(|s| s.busy))
}

fn ever_filled(tab: u64) -> bool {
    TABS.with(|t| t.borrow().get(&tab).is_some_and(|s| !s.filled.is_empty()))
}

fn state(tab: u64, loader: Option<&str>) -> Filled {
    TABS.with(|t| match t.borrow().get(&tab) {
        None => Filled::Never,
        Some(s) if s.filled.is_empty() => Filled::Never,
        // Not knowing which page shows, take it for the filled one.
        Some(s) => match loader {
            Some(l) if !s.filled.iter().any(|f| f == l) => Filled::Before,
            _ => Filled::Now,
        },
    })
}

fn mark(tab: u64, loader: Option<&str>) {
    if let Some(l) = loader {
        with_tab(tab, |s| {
            if !s.marked.iter().any(|m| m == l) {
                s.marked.push(l.to_string());
            }
        });
    }
}

/// Holds the tab busy for as long as it lives, so a fill that ends any
/// way at all, a closed tab included, frees it.
struct Busy(u64);

impl Busy {
    fn take(tab: u64) -> Option<Busy> {
        with_tab(tab, |s| (!s.busy).then(|| s.busy = true)).map(|()| Busy(tab))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        with_tab(self.0, |s| s.busy = false);
    }
}

/// The document the tab shows, as DevTools names it: its loader id (which
/// a new page changes and a move within the page keeps), its address as
/// the browser knows it rather than as the page says, and its frame id,
/// to find the context its scripts run in.
#[derive(Clone)]
struct Page {
    loader: String,
    url: String,
    frame: String,
}

fn page_of(key: &str, tab: u64, done: impl FnOnce(Option<Page>) + 'static) {
    web::devtools(key, tab, "Page.getFrameTree", "{}", move |r| {
        let v: Option<Value> = r.ok().and_then(|t| serde_json::from_str(&t).ok());
        let frame = v.as_ref().and_then(|v| v.pointer("/frameTree/frame"));
        done(frame.and_then(|f| {
            Some(Page {
                loader: f.get("loaderId")?.as_str()?.to_string(),
                url: f.get("url")?.as_str()?.to_string(),
                frame: f.get("id")?.as_str()?.to_string(),
            })
        }))
    });
}

/// Runs one of the scripts above in the tab, in the context `context` when
/// given (and nowhere if it is gone), its value back, or why not.
fn eval(
    key: &str,
    tab: u64,
    expression: String,
    context: Option<&str>,
    done: impl FnOnce(Result<Value, String>) + 'static,
) {
    let mut params = json!({ "expression": expression, "returnByValue": true, "awaitPromise": true });
    if let Some(c) = context {
        params["uniqueContextId"] = json!(c);
    }
    web::devtools(key, tab, "Runtime.evaluate", &params.to_string(), move |r| {
        let v: Value = match r.map(|t| serde_json::from_str(&t)) {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return done(Err(e.to_string())),
            Err(e) => return done(Err(e)),
        };
        if v.get("exceptionDetails").is_some() {
            return done(Err("the page's script failed".into()));
        }
        let value = v.pointer("/result/value").cloned().unwrap_or(Value::Null);
        match value.get("error").and_then(Value::as_str) {
            Some(e) => done(Err(e.to_string())),
            None => done(Ok(value)),
        }
    });
}

fn script(fun: &str, args: &[Value]) -> String {
    let args: Vec<String> = args.iter().map(Value::to_string).collect();
    format!("({fun})({})", args.join(", "))
}

/// The mask for what tab `tab` of project `project` answers: the values
/// its own sessions could be given (the project's, its identity's and
/// those kept for everywhere), long enough not to turn up by chance, and
/// the one time codes near now of any seed a code was filled from here.
/// Another project's values are not its agent's to see in the first
/// place, so masking them would only garble its pages.
fn mask_for(project: &str, identity: Option<&str>, tab: u64) -> Mask {
    let v = store::read();
    let ours = |o: &Owner| match o {
        Owner::Project(p) => p == project,
        Owner::Identity(i) => Some(i.as_str()) == identity,
        Owner::Everywhere => true,
    };
    let mut values: Vec<(String, String)> = Vec::new();
    for s in v.secrets.iter().filter(|s| ours(&s.owner)) {
        values.push((s.name.clone(), s.value.clone()));
    }
    for l in v.logins.iter().filter(|l| ours(&l.owner)) {
        values.push((l.name.clone(), l.password.clone()));
        if let Some(seed) = &l.totp {
            values.push((l.name.clone(), seed.clone()));
        }
    }
    values.retain(|(_, v)| v.len() >= SHORTEST_MASKED);
    let now = unix();
    for (name, seed) in with_tab(tab, |s| s.seeds.clone()) {
        for at in [now.saturating_sub(30), now, now + 30] {
            if let Some(code) = totp(&seed, at) {
                values.push((name.clone(), code));
            }
        }
    }
    Mask::new(values)
}

/// Masks every value of `m` in each string of an answer. Strings are
/// masked one by one, after JSON's escapes are read, so a value with a
/// quote in it is still found. `skip` names keys whose strings are left
/// whole: a picture's data or a base64 body, which a value could turn up
/// in by chance and where none would be readable anyway.
fn mask(v: &mut Value, m: &mut Mask, skip: &[&str]) {
    match v {
        Value::String(s) => {
            let mut out = m.push(s.as_bytes());
            out.extend(m.finish());
            if out != s.as_bytes() {
                *s = String::from_utf8_lossy(&out).into_owned();
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| mask(x, m, &[])),
        Value::Object(o) => {
            let encoded = o.get("base64Encoded") == Some(&Value::Bool(true));
            for (k, x) in o.iter_mut() {
                let whole = skip.contains(&k.as_str()) || (encoded && k == "body");
                if !whole {
                    mask(x, m, &[]);
                }
            }
        }
        _ => {}
    }
}

/// Who an agent's call is from, for the mask: its project and identity.
pub struct Caller {
    pub project: String,
    pub identity: Option<String>,
}

/// Sends an agent's DevTools call and answers it, its text masked.
fn send(who: &Caller, tab: u64, method: &str, params: &Value, done: impl FnOnce(Value) + 'static) {
    let picture = matches!(method, "Page.captureScreenshot" | "Page.printToPDF");
    let (project, identity) = (who.project.clone(), who.identity.clone());
    web::devtools(&who.project, tab, method, &params.to_string(), move |r| {
        done(match r {
            Ok(text) => {
                let mut result = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
                let mut m = mask_for(&project, identity.as_deref(), tab);
                mask(&mut result, &mut m, if picture { &["data"] } else { &[] });
                json!({ "result": result })
            }
            Err(e) => error(&e),
        })
    });
}

/// The call with its script pinned to the main world of the document that
/// was checked, so a page that came back from the cache meanwhile does
/// not run it. None when that world is not known (the tab is between
/// documents), and the call should wait.
fn pinned(tab: u64, method: &str, params: &Value, page: Option<&Page>) -> Option<Value> {
    if method != "Runtime.evaluate" {
        return Some(params.clone());
    }
    let context = web::context_of(tab, &page?.frame)?;
    let mut p = params.clone();
    p["uniqueContextId"] = json!(context);
    Some(p)
}

const MOVED: &str = "the page is between documents; try again in a moment";

const SHOWN: &str = "a filled password shows on this page, or the field it was filled into is \
                     gone from it, so Horadric withholds the page: only browser_navigate, \
                     browser_history (back, forward or reload) and browser_close work. Hide the \
                     password again or reload the page.";

/// An agent's DevTools call on its tab, held to what a filled login
/// allows (see `browse::after_fill`). A call that reads the page past the
/// mcp's own scripts marks that page, and one that runs the agent's code
/// taints the whole tab; no login is filled in either. On a filled page
/// every call but a move off it holds the tab, checks the password is
/// still hidden, runs pinned to that page, and is checked again before
/// its answer goes back. A call allowed only because the page was not
/// the filled one is checked again after it ran, and its answer withheld
/// if the filled page came back meanwhile.
pub fn devtools(who: Caller, tab: u64, method: String, params: Value, reply: Reply) {
    let marks = !browse::quiet(&method, &params, false);
    if browse::taints(&method, &params) {
        with_tab(tab, |s| s.tainted = true);
    }
    if !ever_filled(tab) {
        if !marks {
            return send(&who, tab, &method, &params, move |a| reply.send(a));
        }
        // A tab never filled has nothing to withhold after the call, so
        // the page is looked at once, to mark it.
        let k = who.project.clone();
        return page_of(&k, tab, move |page| {
            mark(tab, page.as_ref().map(|p| p.loader.as_str()));
            send(&who, tab, &method, &params, move |a| reply.send(a));
        });
    }
    let k = who.project.clone();
    page_of(&k, tab, move |before| {
        let loader = before.as_ref().map(|p| p.loader.clone());
        if marks {
            mark(tab, loader.as_deref());
        }
        let now = state(tab, loader.as_deref());
        if let Err(e) = browse::after_fill(&method, &params, now) {
            return reply.send(error(&e));
        }
        let recheck = marks || browse::after_fill(&method, &params, Filled::Now).is_err();
        let Some(params) = pinned(tab, &method, &params, before.as_ref()) else {
            return reply.send(error(MOVED));
        };
        if now == Filled::Now && !browse::moves_only(&method) {
            return on_filled(who, tab, before, method, params, reply);
        }
        let k = who.project.clone();
        send(&who, tab, &method, &params, move |answer| {
            if !recheck {
                return reply.send(answer);
            }
            page_of(&k, tab, move |after| {
                let loader = after.map(|p| p.loader);
                if marks {
                    mark(tab, loader.as_deref());
                }
                if now != Filled::Now && state(tab, loader.as_deref()) == Filled::Now {
                    return reply.send(error(
                        "the page holding a filled login came back while the call ran, so its \
                         answer is withheld",
                    ));
                }
                reply.send(answer);
            });
        });
    });
}

/// A call on the page holding a filled login: the tab held from the first
/// check to the last, so nothing else moves it in between.
fn on_filled(who: Caller, tab: u64, page: Option<Page>, method: String, params: Value, reply: Reply) {
    let Some(held) = Busy::take(tab) else {
        return reply.send(error(BUSY));
    };
    let Some(context) = page.as_ref().and_then(|p| web::context_of(tab, &p.frame)) else {
        return reply.send(error(MOVED));
    };
    let k = who.project.clone();
    let c = context.clone();
    eval(&k, tab, HIDDEN.to_string(), Some(&context), move |r| {
        if !matches!(r, Ok(Value::Bool(true))) {
            return reply.send(error(SHOWN));
        }
        let input = method.starts_with("Input.");
        let k = who.project.clone();
        send(&who, tab, &method, &params, move |answer| {
            if input {
                drop(held);
                return reply.send(answer);
            }
            // The page may have shown the password while the call ran, as
            // a screenshot was taken, say: then its answer is dropped.
            eval(&k, tab, HIDDEN.to_string(), Some(&c), move |r| {
                drop(held);
                match r {
                    Ok(Value::Bool(true)) => reply.send(answer),
                    _ => reply.send(error(SHOWN)),
                }
            });
        });
    });
}


/// What an agent asked to fill, as `browser_fill_login` names it.
pub struct Request {
    pub project: String,
    pub identity: Option<String>,
    pub session: String,
    pub key: String,
    pub name: String,
    pub user: Option<String>,
    pub password: Option<String>,
    pub code: Option<String>,
}

fn unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn log(req: &Request, site: &str, outcome: &str) {
    store::log(&log_line(unix(), &req.project, &req.session, &req.name, site, outcome));
}

/// Fills a kept login into the session's own tab `tab`, answering `reply`
/// with what was filled or why not, never a value.
pub fn fill(req: Request, tab: u64, reply: Reply) {
    if !run::key_ok(&req.session, &req.key) {
        return reply.send(error(
            "Horadric did not know this session's key; only a session Horadric started can \
             fill a login",
        ));
    }
    let Some(login) = store::login(&req.project, req.identity.as_deref(), &req.name) else {
        return reply.send(error(&missing_line(&req.name)));
    };
    if req.code.is_some() && login.totp.is_none() {
        return reply.send(error(&format!(
            "{} keeps no one time code seed, so the code must come from the human: block \
             the quest on them (horadric quest blocked --you), saying which site wants one",
            req.name
        )));
    }
    if web::tied_to_popup(&req.project, tab) {
        return reply.send(error(
            "this tab was opened by another page, or opened one, and a page so tied can read \
             what is typed into the other; open the login page with browser_navigate in a tab \
             of its own and fill it there",
        ));
    }
    if with_tab(tab, |s| s.tainted) {
        return reply.send(error(
            "this tab ran your own code (browser_evaluate with your own script, or a DevTools \
             call that runs code or writes into the page), which can leave a listener behind \
             for later pages, so Horadric fills no login in it until it is closed. Close it \
             (browser_close), then open the login page in a new tab (browser_open) and fill \
             it there without running your own code first",
        ));
    }
    let Some(busy) = Busy::take(tab) else {
        return reply.send(error(BUSY));
    };
    // The project's key is its pane's key too.
    let key = req.project.clone();
    let k = key.clone();
    // From the first fill on, Runtime tells the app the tab's contexts, so
    // the agent's scripts can be pinned to the page they were checked on.
    let tracked = with_tab(tab, |s| std::mem::replace(&mut s.tracked, true));
    let pane = key.clone();
    let start = move || page_of(&k, tab, move |page| checked(req, login, key, tab, busy, page, reply));
    if tracked {
        start();
    } else {
        web::track_contexts(&pane, tab, start);
    }
}

/// Goes on with a fill once the page it would go into is known.
fn checked(
    req: Request,
    login: WebLogin,
    key: String,
    tab: u64,
    busy: Busy,
    page: Option<Page>,
    reply: Reply,
) {
    let Some(Page { loader, url, frame }) = page else {
        return reply.send(error("the page could not be read; try again once it loaded"));
    };
    let site = origin(&url).unwrap_or_else(|| url.clone());
    if !matches(&url, &login.origins) {
        log(&req, &site, "fill refused: another site");
        return reply.send(error(&format!(
            "this tab is on {site}, and {} is kept for {}: Horadric fills a login only on \
             its own site, whatever the page says",
            req.name,
            login.origins.join(", ")
        )));
    }
    if with_tab(tab, |s| s.marked.contains(&loader)) {
        return reply.send(error(
            "this page ran your own script or DevTools changes since it loaded, which \
             could hear what is typed; reload it (browser_history reload) and fill again \
             without them",
        ));
    }
    let job = Job {
        key,
        tab,
        loader,
        frame,
        origin: site,
        fields: fields(&req, &login),
        done: Vec::new(),
        always: false,
        reply,
        _busy: busy,
        req,
        login,
    };
    if with_tab(tab, |s| s.workers_cleared.contains(&job.origin)) {
        return ask_then_fill(job);
    }
    // A service worker the agent registered on this site from another tab
    // would see the sign in request, password and all. The first fill on
    // the site in this tab clears the site's workers when it has any; one
    // the site registers again after the reload is its own, since agent
    // code in this tab since would have tainted it.
    let context = job.context();
    eval(&job.key.clone(), tab, WORKERS.to_string(), context.as_deref(), move |r| match r {
        Ok(Value::Bool(false)) => ask_then_fill(job),
        Ok(Value::Bool(true)) => {
            let params = json!({ "origin": job.origin, "storageTypes": "service_workers" });
            let k = job.key.clone();
            web::devtools(&k, tab, "Storage.clearDataForOrigin", &params.to_string(), move |r| {
                if r.is_err() {
                    return job.fail("this site's service workers could not be cleared");
                }
                with_tab(tab, |s| s.workers_cleared.push(job.origin.clone()));
                log(&job.req, &job.origin, "fill stopped: service workers cleared");
                job.reply.send(error(
                    "Horadric cleared this site's service workers, which could have seen the \
                     login sent: reload the page (browser_history reload) and fill again",
                ));
            });
        }
        _ => job.fail("the page could not be checked for service workers; try again"),
    });
}

/// Asks the human, unless the login is allowed here for good, and fills.
fn ask_then_fill(job: Job) {
    // A page on this machine can be one the agent serves itself, so it
    // is asked about every time, whatever was allowed before.
    let local = is_loopback(&job.origin);
    if !local && job.login.allowed.contains(&job.req.project) {
        return next(job);
    }
    let host = job.origin.split("://").nth(1).unwrap_or(&job.origin).to_string();
    let mut text = format!(
        "An agent of {} asks Horadric to fill the web login {} in its browser tab. It never \
         sees the password.",
        crate::project::project_name(&job.req.project),
        job.req.name
    );
    if local {
        text.push_str(
            " The page is on this machine, where any program, the agent's own server \
             included, can serve one, so Horadric asks each time.",
        );
    }
    let asked = run::ask(
        &job.req.session,
        &format!("Sign in to {host} as {}?", job.login.user),
        &text,
        !local,
    );
    when_answered(asked, move |answer| match answer {
        Some(Answer::Once) => next(job),
        Some(Answer::Always) => {
            // Kept once the fill has gone in, not before: a fill that
            // stops, or an agent that stopped waiting, allows nothing.
            let mut job = job;
            job.always = true;
            next(job)
        }
        Some(Answer::No) => {
            log(&job.req, &job.origin, "fill refused by the human");
            job.reply.send(error(&format!(
                "the human said no to filling {} here",
                job.req.name
            )))
        }
        None => {
            log(&job.req, &job.origin, "fill not answered");
            job.reply.send(error(
                "nobody answered the question on the tile; ask the human, or block the \
                 quest on them (horadric quest blocked --you)",
            ))
        }
    });
}

fn allow(logins: &mut [WebLogin], name: &str, owner: &Owner, project: &str) {
    for l in logins.iter_mut().filter(|l| l.name == name && &l.owner == owner) {
        if !l.allowed.iter().any(|p| p == project) {
            l.allowed.push(project.to_string());
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    User,
    Password,
    Code,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::User => "user",
            Kind::Password => "password",
            Kind::Code => "code",
        }
    }

    fn said(self) -> &'static str {
        match self {
            Kind::User => "user name",
            Kind::Password => "password",
            Kind::Code => "one time code",
        }
    }
}

/// The fields to fill, in the order a person would.
fn fields(req: &Request, login: &WebLogin) -> Vec<(Kind, String)> {
    let mut out = Vec::new();
    for (kind, r) in [
        (Kind::User, &req.user),
        (Kind::Password, &req.password),
        (Kind::Code, &req.code),
    ] {
        if let Some(r) = r {
            out.push((kind, r.clone()));
        }
    }
    // A login with no user name kept has none to fill.
    if login.user.is_empty() {
        out.retain(|(k, _)| *k != Kind::User);
    }
    out
}

/// A fill under way, one field after another.
struct Job {
    key: String,
    tab: u64,
    /// The document the fill was checked on; a different one is a page
    /// that moved, and the fill stops.
    loader: String,
    /// Its frame, for the context the fill's own scripts run in.
    frame: String,
    origin: String,
    fields: Vec<(Kind, String)>,
    done: Vec<Kind>,
    /// The human answered Always, kept once the fill went in.
    always: bool,
    reply: Reply,
    _busy: Busy,
    req: Request,
    login: WebLogin,
}

impl Job {
    fn fail(self, why: &str) {
        log(&self.req, &self.origin, &format!("fill stopped: {why}"));
        let mut why = why.to_string();
        if !self.done.is_empty() {
            let said: Vec<&str> = self.done.iter().map(|k| k.said()).collect();
            why.push_str(&format!(" (the {} went in before it stopped)", said.join(" and ")));
        }
        self.reply.send(error(&why));
    }

    /// The main world of the checked document, when Runtime said it.
    fn context(&self) -> Option<String> {
        web::context_of(self.tab, &self.frame)
    }

    fn value(&self, kind: Kind) -> Option<String> {
        match kind {
            Kind::User => Some(self.login.user.clone()),
            Kind::Password => Some(self.login.password.clone()),
            Kind::Code => self.login.totp.as_deref().and_then(|s| totp(s, unix())),
        }
    }
}

/// Fills the next field, or answers once all are in. Before each the tab
/// must still show the document that was checked, on one of the login's
/// origins: a redirect between the check and the typing stops the fill.
fn next(mut job: Job) {
    if !job.reply.wanted() {
        return job.fail("the agent stopped waiting");
    }
    if job.fields.is_empty() {
        return finish(job);
    }
    let (kind, r) = job.fields.remove(0);
    // Taken as filled from the first key on: a page holding even part of
    // a login is held back like a full one.
    with_tab(job.tab, |s| {
        if !s.filled.contains(&job.loader) {
            s.filled.push(job.loader.clone());
        }
    });
    if kind == Kind::Code {
        if let Some(seed) = job.login.totp.clone() {
            let name = job.login.name.clone();
            with_tab(job.tab, |s| {
                if !s.seeds.iter().any(|(_, x)| *x == seed) {
                    s.seeds.push((name, seed));
                }
            });
        }
    }
    let (key, tab) = (job.key.clone(), job.tab);
    still_there(job, move |job| {
        let args = [json!(r), json!(kind.name()), json!(job.origin)];
        let context = job.context();
        eval(&key, tab, script(FIELD, &args), context.as_deref(), move |res| {
            if let Err(e) = res {
                return job.fail(&e);
            }
            still_there(job, move |mut job| {
                let Some(text) = job.value(kind) else {
                    return job.fail("the one time code could not be made from the kept seed");
                };
                let want = text.encode_utf16().count() as u64;
                let params = json!({ "text": text }).to_string();
                let (k, tab) = (job.key.clone(), job.tab);
                web::devtools(&k, tab, "Input.insertText", &params, move |res| {
                    if res.is_err() {
                        return job.fail("the browser did not take the typing");
                    }
                    let args = [json!(kind.name()), json!(job.origin)];
                    let context = job.context();
                    let k = job.key.clone();
                    eval(&k, tab, script(TYPED, &args), context.as_deref(), move |res| match res {
                        Ok(v) if v["length"].as_u64() == Some(want) => {
                            job.done.push(kind);
                            next(job)
                        }
                        Ok(_) => job.fail(&format!(
                            "the {} field did not take all of it; the page may limit its length",
                            kind.said()
                        )),
                        Err(e) => job.fail(&e),
                    });
                });
            });
        });
    });
}

/// Goes on with the fill only while the tab shows the checked document on
/// one of the login's origins.
fn still_there(job: Job, then: impl FnOnce(Job) + 'static) {
    let (key, tab) = (job.key.clone(), job.tab);
    page_of(&key, tab, move |page| match page {
        Some(p) if p.loader == job.loader && matches(&p.url, &job.login.origins) => then(job),
        _ => job.fail("the page moved before the login was typed in; take a new snapshot"),
    });
}

fn finish(job: Job) {
    let at = unix();
    let (name, owner) = (job.login.name.clone(), job.login.owner.clone());
    let used = Used {
        at,
        session: job.req.session.clone(),
        what: job.origin.clone(),
    };
    let (always, project) = (job.always, job.req.project.clone());
    let _ = store::change(|v| {
        for l in v.logins.iter_mut().filter(|l| l.name == name && l.owner == owner) {
            l.last_used = Some(used.clone());
        }
        if always {
            allow(&mut v.logins, &name, &owner, &project);
        }
    });
    crate::settings::vault_changed();
    log(&job.req, &job.origin, "filled");
    let filled: Vec<&str> = job.done.iter().map(|k| k.said()).collect();
    let user = job.done.contains(&Kind::User).then(|| job.login.user.clone());
    job.reply.send(json!({ "filled": filled, "as": user }));
}

type Then = Box<dyn FnOnce(Option<Answer>)>;

thread_local! {
    static WAITING: RefCell<Vec<(u64, Then)>> = const { RefCell::new(Vec::new()) };
    static INBOX: RefCell<Option<(Sender<(u64, Option<Answer>)>, Receiver<(u64, Option<Answer>)>)>> =
        const { RefCell::new(None) };
    static NEXT_WAIT: Cell<u64> = const { Cell::new(1) };
}

/// Runs `then` on this (the UI) thread with the human's answer: the wait
/// is on a thread of its own, and a thread timer here picks the answer up.
fn when_answered(asked: Asked, then: impl FnOnce(Option<Answer>) + 'static) {
    let id = NEXT_WAIT.with(|n| n.replace(n.get() + 1));
    let tx = INBOX.with(|i| {
        i.borrow_mut()
            .get_or_insert_with(mpsc::channel)
            .0
            .clone()
    });
    WAITING.with(|w| w.borrow_mut().push((id, Box::new(then))));
    thread::spawn(move || {
        let answer = asked.wait(ASK_WAIT);
        let _ = tx.send((id, answer));
    });
    unsafe {
        SetTimer(None, 0, 200, Some(answered));
    }
}

unsafe extern "system" fn answered(_: HWND, _: u32, timer: usize, _: u32) {
    let came: Vec<(u64, Option<Answer>)> = INBOX.with(|i| {
        i.borrow()
            .as_ref()
            .map(|(_, rx)| rx.try_iter().collect())
            .unwrap_or_default()
    });
    for (id, answer) in came {
        let then = WAITING.with(|w| {
            let mut w = w.borrow_mut();
            let at = w.iter().position(|(i, _)| *i == id)?;
            Some(w.remove(at).1)
        });
        if let Some(then) = then {
            then(answer);
        }
    }
    if WAITING.with(|w| w.borrow().is_empty()) {
        let _ = unsafe { KillTimer(None, timer) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masking_reads_strings_after_their_escapes() {
        let mut m = Mask::new(vec![("PW".to_string(), "a\"b".to_string())]);
        let mut v = json!({ "result": { "value": "pw is a\"b here" }, "data": "a\"b" });
        mask(&mut v, &mut m, &["data"]);
        assert_eq!(v["result"]["value"], "pw is [PW] here");
        assert_eq!(v["data"], "a\"b");
        let mut v = json!({ "body": "a\"b", "base64Encoded": true, "x": "a\"b" });
        mask(&mut v, &mut m, &[]);
        assert_eq!(v["body"], "a\"b");
        assert_eq!(v["x"], "[PW]");
    }

    #[test]
    fn a_page_is_filled_now_before_or_never() {
        let tab = 9_000_001;
        assert_eq!(state(tab, Some("L1")), Filled::Never);
        with_tab(tab, |s| s.filled.push("L1".into()));
        assert_eq!(state(tab, Some("L1")), Filled::Now);
        assert_eq!(state(tab, Some("L2")), Filled::Before);
        assert_eq!(state(tab, None), Filled::Now);
    }

    #[test]
    fn a_busy_tab_is_freed_however_the_fill_ends() {
        let tab = 9_000_002;
        let b = Busy::take(tab).unwrap();
        assert!(busy(tab));
        assert!(Busy::take(tab).is_none());
        drop(b);
        assert!(!busy(tab));
    }
}
