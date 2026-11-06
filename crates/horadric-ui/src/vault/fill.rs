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
//! and remembers pages the agent ran its own code in, since a listener it
//! left there would hear the password typed.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use horadric_core::vault::secret::{log_line, missing_line, Mask, Owner};
use horadric_core::vault::web::{matches, origin, totp};
use horadric_hooks::listener::Reply;
use serde_json::{json, Value};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use super::run::{self, Answer, Asked};
use super::store::{self, Used, WebLogin};
use crate::browse::{self, error, Filled};
use crate::web;

/// How long a fill waits for the human to answer before it tells the agent
/// nobody did. Under the mcp's own wait for the app, so the agent hears it.
const ASK_WAIT: Duration = Duration::from_secs(90);

/// Said for any call on a tab while a fill types into it: a navigation in
/// between would carry the typing to another page.
pub const BUSY: &str = "Horadric is filling a login in this tab; try again in a moment";

/// Checks the ref names a field of the kind said, in the page itself and on
/// the origin said, and puts the keyboard in it with its text selected, to
/// be replaced. It marks the field, so the check after typing knows the
/// keyboard is still there and a screenshot knows which field to look at.
const FIELD: &str = r#"(ref, kind, origin) => {
  if (location.origin !== origin) return { error: `the page moved to ${location.origin}` };
  if (window.opener) return { error: "this page was opened by another page, which could read what is typed into it; open the login page with browser_navigate and fill it there" };
  const refs = (window.__horadric || {}).refs || [];
  const el = refs[Number(String(ref).replace(/^e/, ""))];
  if (!el || !el.isConnected) return { error: `there is no ${ref} on this page; take a new browser_snapshot` };
  if (el.ownerDocument !== document) return { error: `${ref} is in a frame; Horadric fills only the page itself` };
  const type = el.tagName === "INPUT" ? el.type : "";
  const fits = { user: ["text", "email", "tel"], password: ["password"], code: ["text", "number", "tel", "password"] }[kind];
  if (!fits.includes(type)) return { error: `${ref} is not a ${kind === "user" ? "user name" : kind} field` };
  if (el.disabled || el.readOnly) return { error: `${ref} does not take typing` };
  el.scrollIntoView({ block: "center", behavior: "instant" });
  el.focus();
  if (el.getRootNode().activeElement !== el) return { error: `${ref} would not take the keyboard` };
  el.select();
  el.setAttribute("data-horadric-filled", kind);
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

/// Whether every filled password field still hides what it holds, as the
/// page's own Show password button may change.
const HIDDEN: &str = r#"Array.from(document.querySelectorAll('[data-horadric-filled="password"]')).every((e) => e.type === "password")"#;

#[derive(Default)]
struct TabState {
    /// The documents (DevTools loader ids) a login was filled in.
    filled: Vec<String>,
    /// The documents the agent ran its own code in.
    marked: Vec<String>,
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

/// The document the tab shows, as DevTools names it (its loader id, which
/// a new page changes and a move within the page keeps), and its address,
/// as the browser knows it rather than as the page says.
fn page_of(key: &str, tab: u64, done: impl FnOnce(Option<(String, String)>) + 'static) {
    web::devtools(key, tab, "Page.getFrameTree", "{}", move |r| {
        let v: Option<Value> = r.ok().and_then(|t| serde_json::from_str(&t).ok());
        let frame = v.as_ref().and_then(|v| v.pointer("/frameTree/frame"));
        done(frame.and_then(|f| {
            Some((
                f.get("loaderId")?.as_str()?.to_string(),
                f.get("url")?.as_str()?.to_string(),
            ))
        }))
    });
}

/// Runs one of the scripts above in the tab, its value back, or why not.
fn eval(key: &str, tab: u64, expression: String, done: impl FnOnce(Result<Value, String>) + 'static) {
    let params = json!({ "expression": expression, "returnByValue": true, "awaitPromise": true });
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

/// Every value the Vault keeps, by name, for the mask.
fn kept() -> Vec<(String, String)> {
    let v = store::read();
    let mut all: Vec<(String, String)> = v
        .secrets
        .iter()
        .map(|s| (s.name.clone(), s.value.clone()))
        .collect();
    for l in &v.logins {
        all.push((l.name.clone(), l.password.clone()));
        if let Some(seed) = &l.totp {
            all.push((l.name.clone(), seed.clone()));
        }
    }
    all
}

/// Masks every kept value in each string of an answer. Strings are masked
/// one by one, after JSON's escapes are read, so a value with a quote in
/// it is still found. `skip` names a key whose string is left whole: a
/// picture's data, which a short value could turn up in by chance.
fn mask(v: &mut Value, values: &[(String, String)], skip: Option<&str>) {
    match v {
        Value::String(s) => {
            let mut m = Mask::new(values.to_vec());
            let mut out = m.push(s.as_bytes());
            out.extend(m.finish());
            *s = String::from_utf8_lossy(&out).into_owned();
        }
        Value::Array(a) => a.iter_mut().for_each(|x| mask(x, values, None)),
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                if skip != Some(k.as_str()) {
                    mask(x, values, None);
                }
            }
        }
        _ => {}
    }
}

/// Sends an agent's DevTools call and answers it, its text masked.
fn send(key: &str, tab: u64, method: String, params: Value, done: impl FnOnce(Value) + 'static) {
    let picture = matches!(method.as_str(), "Page.captureScreenshot" | "Page.printToPDF");
    web::devtools(key, tab, &method, &params.to_string(), move |r| {
        done(match r {
            Ok(text) => {
                let mut result = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
                let values = kept();
                if !values.is_empty() {
                    mask(&mut result, &values, picture.then_some("data"));
                }
                json!({ "result": result })
            }
            Err(e) => error(&e),
        })
    });
}

/// An agent's DevTools call on its tab, held to what a filled login
/// allows (see `browse::after_fill`). A call that could put the agent's
/// own code into the page marks that page, and no login is filled there.
/// A call allowed only because the page was not the filled one is checked
/// again after it ran, and its answer withheld if the filled page came
/// back meanwhile.
pub fn devtools(key: &str, tab: u64, method: String, params: Value, reply: Reply) {
    let marks = !browse::quiet(&method, &params, false);
    let ever = TABS.with(|t| t.borrow().get(&tab).is_some_and(|s| !s.filled.is_empty()));
    if method.starts_with("Input.") || (!marks && !ever) {
        return send(key, tab, method, params, move |a| reply.send(a));
    }
    let key = key.to_string();
    page_of(&key.clone(), tab, move |before| {
        let loader = before.map(|(l, _)| l);
        if marks {
            mark(tab, loader.as_deref());
        }
        let now = state(tab, loader.as_deref());
        if let Err(e) = browse::after_fill(&method, &params, now) {
            return reply.send(error(&e));
        }
        let recheck = marks || browse::after_fill(&method, &params, Filled::Now).is_err();
        let picture = method == "Page.captureScreenshot";
        let k = key.clone();
        let go = move |reply: Reply| {
            let k2 = k.clone();
            send(&k, tab, method, params, move |answer| {
                if !recheck {
                    return reply.send(answer);
                }
                page_of(&k2, tab, move |after| {
                    let loader = after.map(|(l, _)| l);
                    if marks {
                        mark(tab, loader.as_deref());
                    }
                    if now != Filled::Now && state(tab, loader.as_deref()) == Filled::Now {
                        return reply.send(error(
                            "the page holding a filled login came back while the call ran, \
                             so its answer is withheld",
                        ));
                    }
                    reply.send(answer);
                });
            });
        };
        if now == Filled::Now && picture {
            // A password the page's own Show password button turned to text
            // would be in the picture.
            return eval(&key, tab, HIDDEN.to_string(), move |r| match r {
                Ok(Value::Bool(true)) => go(reply),
                _ => reply.send(error(
                    "a filled password shows as text on this page; hide it again before a \
                     screenshot",
                )),
            });
        }
        go(reply);
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
            "this tab was opened by another page, or opened one, and a page so tied can read              what is typed into the other; open the login page with browser_navigate in a tab              of its own and fill it there",
        ));
    }
    let Some(busy) = Busy::take(tab) else {
        return reply.send(error(BUSY));
    };
    // The project's key is its pane's key too.
    let key = req.project.clone();
    let k = key.clone();
    page_of(&k, tab, move |page| {
        let Some((loader, url)) = page else {
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
            origin: site,
            fields: fields(&req, &login),
            done: Vec::new(),
            reply,
            _busy: busy,
            req,
            login,
        };
        if job.login.allowed.contains(&job.req.project) {
            return next(job);
        }
        let host = job.origin.split("://").nth(1).unwrap_or(&job.origin).to_string();
        let asked = run::ask(
            &job.req.session,
            &format!("Sign in to {host} as {}?", job.login.user),
            &format!(
                "An agent of {} asks Horadric to fill the web login {} in its browser tab. \
                 It never sees the password.",
                crate::project::project_name(&job.req.project),
                job.req.name
            ),
            true,
        );
        when_answered(asked, move |answer| match answer {
            Some(Answer::Once) => next(job),
            Some(Answer::Always) => {
                let (name, owner, project) =
                    (job.login.name.clone(), job.login.owner.clone(), job.req.project.clone());
                let _ = store::change(|v| allow(&mut v.logins, &name, &owner, &project));
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
    origin: String,
    fields: Vec<(Kind, String)>,
    done: Vec<Kind>,
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
    let (key, tab) = (job.key.clone(), job.tab);
    still_there(job, move |job| {
        let args = [json!(r), json!(kind.name()), json!(job.origin)];
        eval(&key, tab, script(FIELD, &args), move |res| {
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
                    eval(&job.key.clone(), tab, script(TYPED, &args), move |res| match res {
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
        Some((loader, url)) if loader == job.loader && matches(&url, &job.login.origins) => {
            then(job)
        }
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
    let _ = store::change(|v| {
        for l in v.logins.iter_mut().filter(|l| l.name == name && l.owner == owner) {
            l.last_used = Some(used.clone());
        }
    });
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
        let values = vec![("PW".to_string(), "a\"b".to_string())];
        let mut v = json!({ "result": { "value": "pw is a\"b here" }, "data": "a\"b" });
        mask(&mut v, &values, Some("data"));
        assert_eq!(v["result"]["value"], "pw is [PW] here");
        assert_eq!(v["data"], "a\"b");
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
