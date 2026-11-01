//! The browser pane on the Mac: a WKWebView per tab, a project's tabs in
//! one pane on the stage beside its sessions, as WebView2's are on
//! Windows. Which tab an agent drives and how its calls read are shared
//! ([`crate::browse`]); only the page differs.
//!
//! WebKit has no DevTools protocol, so the protocol calls `horadric mcp`
//! makes are stood in for by script in the page and a picture of it
//! ([`crate::webkit_devtools`]). The agent's side is the same on both.
//!
//! The stage lays its panes out again whenever it shows another project,
//! and a WKWebView taken out of its window keeps its page. So the views
//! belong to this module: the shown tab of a shown pane sits in the pane,
//! the rest wait in a window off every screen, where they keep running
//! and can still be pictured. Every project, and every Horadric on the
//! Mac, shares WebKit's default store: log in once and it stays.
//!
//! Everything here runs on the main thread. WebKit calls back on it.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use block2::RcBlock;
use horadric_core::agent::Agent;
use horadric_core::Phase;
use horadric_hooks::listener::{BrowserCall, Reply};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{
    define_class, msg_send, sel, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly,
    Message,
};
use objc2_app_kit::{
    NSBackingStoreType, NSBezelStyle, NSBitmapImageFileType, NSBitmapImageRep, NSButton,
    NSFont, NSImage, NSLineBreakMode, NSResponder, NSTextField, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{
    NSDictionary, NSError, NSNumber, NSObject, NSPoint, NSRect, NSSize, NSString, NSURLRequest,
    NSURL,
};
use objc2_web_kit::{
    WKContentWorld, WKNavigation, WKNavigationAction, WKNavigationDelegate,
    WKSnapshotConfiguration, WKUIDelegate, WKUserScript, WKUserScriptInjectionTime, WKWebView,
    WKWebViewConfiguration, WKWebsiteDataStore, WKWindowFeatures,
};
use serde_json::{json, Value};

use crate::browse::{self, closed, error, Call, Pick, Step, AGENT_PROMPT, KEEP_CONSOLE};
use crate::console::{self, Console};
use crate::layout::Rect;
use crate::project::project_name;
use crate::store;
use crate::theme;
use crate::webkit_devtools::{self as devtools, Clip, Plan, Shot};

use super::app::App;
use super::paint::{cg_rect, Painter};
use super::queue;

/// The stage pane of project `key` is `web:<key>`.
const PANE: &str = "web:";
/// The tab strip and the address bar above the page.
const ROW_H: f64 = 26.0;
const BAR_H: f64 = 2.0 * ROW_H + 6.0;
const TAB_W: f64 = 150.0;
const NAV_W: f64 = 30.0;
/// The size a page lays out at while it waits off screen.
const PARKED: (f64, f64) = (1280.0, 800.0);
/// The tallest full page picture, in CSS pixels, as on Windows.
const TALLEST: f64 = 12_000.0;
/// How long a load is waited on before the agent hears it did not end.
const LOAD_WAIT: Duration = Duration::from_secs(30);
/// How often a load is looked at, and the shown pane's bar brought up to
/// date with titles a page sets after it loads.
const LOOK: Duration = Duration::from_millis(200);
/// Consoles of browser panes are not the app's, so their serials start
/// far from its own.
const SERIALS: usize = 1 << 40;

struct Tab {
    id: u64,
    view: Retained<WKWebView>,
    /// The session whose agent works in this tab, if one does.
    driver: Option<String>,
    /// An address asked for that the page has not reached yet.
    pending: Option<String>,
    /// Whether the last load failed, for the agent waiting on it.
    failed: bool,
}

impl Tab {
    fn url(&self) -> String {
        unsafe { self.view.URL() }
            .and_then(|u| u.absoluteString())
            .map(|s| s.to_string())
            .unwrap_or_default()
    }

    fn title(&self) -> String {
        unsafe { self.view.title() }
            .map(|s| s.to_string())
            .unwrap_or_default()
    }
}

struct Web {
    tabs: Vec<Tab>,
    active: usize,
    console: Arc<Console>,
    pane: Option<Retained<WebPane>>,
}

thread_local! {
    static WEBS: RefCell<HashMap<String, Web>> = RefCell::new(HashMap::new());
    static PARK: RefCell<Option<Retained<NSWindow>>> = const { RefCell::new(None) };
    static WATCHER: RefCell<Option<Retained<Watcher>>> = const { RefCell::new(None) };
    static NEXT: Cell<u64> = const { Cell::new(1) };
    static CONFIG: RefCell<Option<Option<PathBuf>>> = const { RefCell::new(None) };
    static WATCHING: Cell<bool> = const { Cell::new(false) };
}

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("the browser pane lives on the main thread")
}

/// The flags that give a Claude session Horadric's MCP server, whose tools
/// drive this pane, and tell it so. Its config is written once a run.
pub fn claude_args() -> Vec<String> {
    let config = CONFIG.with(|c| {
        c.borrow_mut()
            .get_or_insert_with(|| store::write_mcp_config(&console::host_program()))
            .clone()
    });
    let mut args = Agent::Claude.mcp_args(config.as_deref().and_then(|p| p.to_str()));
    if config.is_some() {
        args.push(format!("--append-system-prompt={AGENT_PROMPT}"));
    }
    args
}

// The app's side: the stage, the menus and the agents' calls.

/// The stage pane id of project `key`'s browser.
pub fn pane_id(key: &str) -> String {
    format!("{PANE}{key}")
}

/// Whether `id` is a browser pane's.
pub fn owns(id: &str) -> bool {
    id.starts_with(PANE)
}

pub fn is_open(key: &str) -> bool {
    WEBS.with(|w| w.borrow().contains_key(key))
}

/// The pane console of project `key`'s browser, while it is open.
pub fn console_of(key: &str) -> Option<Arc<Console>> {
    WEBS.with(|w| w.borrow().get(key).map(|web| Arc::clone(&web.console)))
}

/// What the pane's header says after its name: the shown page's title.
pub fn detail(key: &str) -> String {
    read_tab(key, None, |t| browse::tab_name(&t.title(), &t.url())).unwrap_or_default()
}

/// Opens project `key`'s browser for the user, a blank tab in it when it
/// had none, and shows it on the stage.
pub fn open_for_user(app: &mut App, key: &str) {
    if !is_open(key) || tab_count(key) == 0 {
        ensure(key);
        add_tab(key, None, None, true);
    }
    app.show_project(key, Some(&pane_id(key)));
}

/// The pane's cross: the browser and every tab in it go.
pub fn close_pane(app: &mut App, id: &str) -> bool {
    let Some(key) = id.strip_prefix(PANE) else {
        return false;
    };
    close(key);
    app.reconcile();
    true
}

/// Gives the shown page the keyboard.
pub fn focus(key: &str) {
    let view = read_tab(key, None, |t| t.view.clone());
    if let Some(v) = view {
        if let Some(w) = v.window() {
            w.makeFirstResponder(Some(&v));
        }
    }
}

/// Puts each shown browser's pane where the stage laid it out, over the
/// pane's glass, its shown page in it, and every other page off screen.
pub fn place(stage: &NSView, shown: &[(String, Rect)]) {
    let mtm = MainThreadMarker::from(stage);
    let keys: Vec<String> = WEBS.with(|w| w.borrow().keys().cloned().collect());
    for key in keys {
        let at = shown.iter().find(|(k, _)| *k == key).map(|(_, r)| *r);
        let (pane, views, active) = WEBS.with(|w| {
            let mut w = w.borrow_mut();
            let web = w.get_mut(&key).expect("listed above");
            if at.is_some() && web.pane.is_none() {
                web.pane = Some(WebPane::new(mtm, &key));
            }
            let views: Vec<Retained<WKWebView>> =
                web.tabs.iter().map(|t| t.view.clone()).collect();
            (web.pane.clone(), views, web.active)
        });
        let Some(pane) = pane else {
            views.iter().for_each(|v| park(mtm, v));
            continue;
        };
        let Some(r) = at else {
            pane.removeFromSuperview();
            views.iter().for_each(|v| park(mtm, v));
            continue;
        };
        if !unsafe { pane.superview() }.is_some_and(|p| std::ptr::eq(&*p, stage)) {
            stage.addSubview(&pane);
        }
        pane.setFrame(cg_rect(&r));
        for (i, v) in views.iter().enumerate() {
            if i == active {
                pane.hold(v);
            } else {
                park(mtm, v);
            }
        }
        pane.refresh();
    }
}

/// The browser's pane goes off the stage and its pages with it.
fn close(key: &str) {
    let gone = WEBS.with(|w| w.borrow_mut().remove(key));
    if let Some(web) = gone {
        if let Some(p) = &web.pane {
            p.removeFromSuperview();
        }
        for t in &web.tabs {
            unsafe { t.view.stopLoading() };
            t.view.removeFromSuperview();
        }
    }
}

/// Hands an agent's call to the page its session's tab shows, and answers
/// it, as Windows does in `drive.rs`.
pub fn call(app: &mut App, call: BrowserCall) {
    let reply = call.reply;
    // Its agent heard it fail while it waited behind something slow.
    if !reply.wanted() {
        return;
    }
    let Some(key) = app.project_of(&call.session) else {
        return reply.send(error("this session has no project in Horadric"));
    };
    let what = match Call::parse(&call.body) {
        Ok(c) => c,
        Err(e) => return reply.send(error(&e)),
    };
    let session = call.session.as_str();
    let live = live_sessions(app);
    let tab = |make: bool| agent_tab(&key, session, &live, make);
    match what {
        Call::Info => reply.send(info(&key, tab(false))),
        Call::Open(url) => {
            let had = is_open(&key).then(|| tab(false)).flatten();
            let Some(id) = open_for_agent(app, &key, session, url.as_deref()) else {
                return reply.send(closed());
            };
            if had != Some(id) || url.is_some() {
                answer_after_load(&key, id, reply);
            } else {
                reply.send(info(&key, Some(id)));
            }
        }
        Call::Navigate(url) => {
            let Some(id) = open_for_agent(app, &key, session, Some(&url)) else {
                return reply.send(closed());
            };
            answer_after_load(&key, id, reply);
        }
        Call::History(step) => {
            let Some(id) = tab(false) else {
                return reply.send(closed());
            };
            let (back, forward) = read_tab(&key, Some(id), |t| unsafe {
                (t.view.canGoBack(), t.view.canGoForward())
            })
            .unwrap_or_default();
            if let Err(e) = browse::can_step(step, back, forward) {
                return reply.send(error(&e));
            }
            go(&key, Some(id), step);
            answer_after_load(&key, id, reply);
        }
        Call::Close => {
            // Only its own tab: the others are the user's or another
            // agent's.
            let own = WEBS.with(|w| {
                w.borrow().get(&key).and_then(|web| {
                    (web.tabs.iter())
                        .find(|t| t.driver.as_deref() == Some(session))
                        .map(|t| t.id)
                })
            });
            let closed = own.is_some_and(|id| close_tab(&key, id));
            if is_open(&key) && tab_count(&key) == 0 {
                close(&key);
            }
            app.reconcile();
            reply.send(json!({ "closed": closed }));
        }
        Call::DevTools(method, params) => {
            let Some(id) = tab(false) else {
                return reply.send(closed());
            };
            run(&key, id, &method, &params, move |r| {
                reply.send(match r {
                    Ok(result) => json!({ "result": result }),
                    Err(e) => error(&e),
                })
            });
        }
    }
}

/// The sessions whose agents can still drive a page: a tab whose agent
/// has ended is free for another.
fn live_sessions(app: &App) -> HashSet<String> {
    let Ok(r) = app.registry.lock() else {
        return HashSet::new();
    };
    r.all()
        .filter(|s| s.phase != Phase::Ended)
        .map(|s| s.id.clone())
        .collect()
}

/// Opens the project's browser without the stage or the keyboard (the user
/// may be looking at another project) and gives `session` a tab of its
/// own in it, at `url` when given. Says which tab.
fn open_for_agent(app: &mut App, key: &str, session: &str, url: Option<&str>) -> Option<u64> {
    ensure(key);
    let live = live_sessions(app);
    // Without an address it only wants to look, which the user's page
    // allows: a tab of its own only when there is nothing to look at.
    let id = agent_tab(key, session, &live, url.is_some())
        .or_else(|| agent_tab(key, session, &live, true));
    if let (Some(id), Some(u)) = (id, url) {
        navigate(key, id, u);
    }
    app.reconcile();
    id
}

/// The tab `session`'s agent works in, as [`browse::pick_tab`] picks it,
/// opening one for it when `make` and it has none to take.
fn agent_tab(key: &str, session: &str, live: &HashSet<String>, make: bool) -> Option<u64> {
    let picked = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let Some(web) = w.get_mut(key) else {
            return if make { Err(()) } else { Ok(None) };
        };
        let drivers: Vec<Option<&str>> = (web.tabs.iter())
            .map(|t| t.driver.as_deref().filter(|d| live.contains(*d)))
            .collect();
        let shown_blank = (web.tabs.get(web.active))
            .is_some_and(|t| browse::is_blank(&t.url(), t.pending.as_deref()));
        match browse::pick_tab(&drivers, web.active, shown_blank, session, make) {
            Pick::At(i) => Ok(Some(web.tabs[i].id)),
            Pick::Claim(i) => {
                web.tabs[i].driver = Some(session.to_string());
                Ok(Some(web.tabs[i].id))
            }
            Pick::New => Err(()),
            Pick::Nothing => Ok(None),
        }
    });
    match picked {
        Ok(id) => id,
        Err(()) => {
            ensure(key);
            Some(add_tab(key, None, Some(session.to_string()), false))
        }
    }
}

/// Answers once the tab's load ends, with where it ended up.
fn answer_after_load(key: &str, id: u64, reply: Reply) {
    let k = key.to_string();
    after_load(key, id, move |ok| {
        let mut answer = info(&k, Some(id));
        if !ok {
            answer["warning"] = json!("the page did not finish loading");
        }
        reply.send(answer);
    });
}

/// Whether the project has a page, where the session's tab is, and
/// whether the user can see it.
fn info(key: &str, tab: Option<u64>) -> Value {
    let (title, url) = tab
        .and_then(|id| read_tab(key, Some(id), |t| (t.title(), t.url())))
        .unwrap_or_default();
    let place = tab.and_then(|id| {
        WEBS.with(|w| {
            let w = w.borrow();
            let web = w.get(key)?;
            let at = web.tabs.iter().position(|t| t.id == id)?;
            Some((at, web.tabs.len(), at == web.active))
        })
    });
    browse::info(
        is_open(key),
        is_shown(key),
        &project_name(key),
        place,
        &title,
        &url,
    )
}

/// Whether the project's pane is on the stage in a window that shows.
fn is_shown(key: &str) -> bool {
    WEBS.with(|w| {
        (w.borrow().get(key))
            .and_then(|web| web.pane.as_ref())
            .and_then(|p| p.window())
            .is_some_and(|win| win.isVisible())
    })
}

// The tabs.

/// Makes the project's browser when it has none, with no tab yet.
fn ensure(key: &str) {
    if is_open(key) {
        return;
    }
    if !WATCHING.with(|w| w.replace(true)) {
        watch();
    }
    let serial = SERIALS + NEXT.with(|n| n.get()) as usize;
    let console = Console::web(pane_id(key), serial, key.to_string());
    WEBS.with(|w| {
        w.borrow_mut().insert(
            key.to_string(),
            Web {
                tabs: Vec::new(),
                active: 0,
                console,
                pane: None,
            },
        )
    });
}

fn tab_count(key: &str) -> usize {
    WEBS.with(|w| w.borrow().get(key).map_or(0, |web| web.tabs.len()))
}

/// Adds a tab after the others, at `url` when given, driven by `driver`,
/// shown when `front` or when it is the only one. Says its id.
fn add_tab(key: &str, url: Option<&str>, driver: Option<String>, front: bool) -> u64 {
    let mtm = mtm();
    let id = NEXT.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    });
    let view = make_view(mtm);
    park(mtm, &view);
    WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let web = w.get_mut(key).expect("made by ensure");
        web.tabs.push(Tab {
            id,
            view,
            driver,
            pending: None,
            failed: false,
        });
        if front || web.tabs.len() == 1 {
            web.active = web.tabs.len() - 1;
        }
    });
    if let Some(u) = url {
        navigate(key, id, u);
    }
    retiled(key);
    id
}

fn make_view(mtm: MainThreadMarker) -> Retained<WKWebView> {
    unsafe {
        let config = WKWebViewConfiguration::new(mtm);
        config.setWebsiteDataStore(&WKWebsiteDataStore::defaultDataStore(mtm));
        // Without it WebKit names no browser, and some sites send a page
        // for an old one.
        config.setApplicationNameForUserAgent(Some(&NSString::from_str(
            "Version/18.0 Safari/605.1.15",
        )));
        let script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly(
            WKUserScript::alloc(mtm),
            &NSString::from_str(KEEP_CONSOLE),
            WKUserScriptInjectionTime::AtDocumentStart,
            false,
        );
        config.userContentController().addUserScript(&script);
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PARKED.0, PARKED.1));
        let view = WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &config);
        let watcher = WATCHER.with(|w| {
            w.borrow_mut()
                .get_or_insert_with(|| Watcher::new(mtm))
                .clone()
        });
        view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*watcher)));
        view.setUIDelegate(Some(ProtocolObject::from_ref(&*watcher)));
        view.setAllowsBackForwardNavigationGestures(true);
        view.setAllowsMagnification(true);
        // Safari's Web Inspector, where macOS has it for apps (13.3 on).
        if view.respondsToSelector(sel!(setInspectable:)) {
            view.setInspectable(true);
        }
        view
    }
}

/// Puts `view` in the window off screen, where it keeps its page.
fn park(mtm: MainThreadMarker, view: &WKWebView) {
    let window = PARK.with(|p| {
        p.borrow_mut()
            .get_or_insert_with(|| {
                let frame = NSRect::new(
                    NSPoint::new(-30_000.0, -30_000.0),
                    NSSize::new(PARKED.0, PARKED.1),
                );
                let w = unsafe {
                    NSWindow::initWithContentRect_styleMask_backing_defer(
                        NSWindow::alloc(mtm),
                        frame,
                        NSWindowStyleMask::Borderless,
                        NSBackingStoreType::Buffered,
                        false,
                    )
                };
                unsafe { w.setReleasedWhenClosed(false) };
                w.setIgnoresMouseEvents(true);
                w.setCollectionBehavior(
                    NSWindowCollectionBehavior::Transient
                        | NSWindowCollectionBehavior::IgnoresCycle,
                );
                // In the window list, off every screen: a page in a window
                // never shown is not drawn, so could not be pictured.
                w.orderBack(None);
                w
            })
            .clone()
    });
    let Some(content) = window.contentView() else {
        return;
    };
    if unsafe { view.superview() }.is_some_and(|p| std::ptr::eq(&*p, &*content)) {
        return;
    }
    view.removeFromSuperview();
    content.addSubview(view);
    view.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(PARKED.0, PARKED.1),
    ));
}

fn navigate(key: &str, id: u64, url: &str) {
    let view = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let t = w.get_mut(key)?.tabs.iter_mut().find(|t| t.id == id)?;
        t.pending = Some(url.to_string());
        t.failed = false;
        Some(t.view.clone())
    });
    let (Some(view), Some(u)) = (view, NSURL::URLWithString(&NSString::from_str(url))) else {
        return;
    };
    unsafe { view.loadRequest(&NSURLRequest::requestWithURL(&u)) };
}

fn go(key: &str, id: Option<u64>, step: Step) {
    let Some(view) = read_tab(key, id, |t| t.view.clone()) else {
        return;
    };
    unsafe {
        match step {
            Step::Back => drop(view.goBack()),
            Step::Forward => drop(view.goForward()),
            Step::Reload => drop(view.reload()),
        }
    }
}

/// Closes the tab, showing its neighbour as a browser does.
fn close_tab(key: &str, id: u64) -> bool {
    let gone = WEBS.with(|w| {
        let mut w = w.borrow_mut();
        let web = w.get_mut(key)?;
        let at = web.tabs.iter().position(|t| t.id == id)?;
        web.active = browse::after_close(web.active, at, web.tabs.len());
        Some(web.tabs.remove(at))
    });
    let Some(tab) = gone else {
        return false;
    };
    unsafe { tab.view.stopLoading() };
    tab.view.removeFromSuperview();
    retiled(key);
    true
}

/// The tab with this id, or the shown one, read.
fn read_tab<T>(key: &str, id: Option<u64>, f: impl FnOnce(&Tab) -> T) -> Option<T> {
    WEBS.with(|w| {
        let w = w.borrow();
        let web = w.get(key)?;
        let tab = match id {
            Some(id) => web.tabs.iter().find(|t| t.id == id)?,
            None => web.tabs.get(web.active)?,
        };
        Some(f(tab))
    })
}

/// The tabs changed: the shown pane holds the shown tab again.
fn retiled(key: &str) {
    let pane = WEBS.with(|w| w.borrow().get(key).and_then(|web| web.pane.clone()));
    let Some(pane) = pane else {
        return;
    };
    let (views, active) = WEBS.with(|w| {
        let w = w.borrow();
        let web = w.get(key).expect("has a pane");
        let views: Vec<Retained<WKWebView>> = web.tabs.iter().map(|t| t.view.clone()).collect();
        (views, web.active)
    });
    if unsafe { pane.superview() }.is_some() {
        let mtm = mtm();
        for (i, v) in views.iter().enumerate() {
            if i == active {
                pane.hold(v);
            } else {
                park(mtm, v);
            }
        }
    }
    pane.refresh();
}

/// Runs `f` once the tab's load ends: true when it ended well, false when
/// it failed or did not end in [`LOAD_WAIT`]. Looked at, rather than told,
/// so a load within the page, which WebKit says nothing of, ends too.
fn after_load(key: &str, id: u64, f: impl FnOnce(bool) + 'static) {
    let started = Instant::now();
    let f: Rc<RefCell<Option<Box<dyn FnOnce(bool)>>>> = Rc::new(RefCell::new(Some(Box::new(f))));
    look(key.to_string(), id, started, f);
}

fn look(key: String, id: u64, started: Instant, f: Rc<RefCell<Option<Box<dyn FnOnce(bool)>>>>) {
    queue::after_main(LOOK, move || {
        let state = read_tab(&key, Some(id), |t| {
            (unsafe { t.view.isLoading() }, t.failed, t.pending.is_some())
        });
        let done = match state {
            // The tab went.
            None => Some(false),
            Some((_, true, _)) => Some(false),
            // A load asked for and not begun yet still counts as loading.
            Some((loading, false, pending)) if !loading && !pending => Some(true),
            Some((loading, false, _)) if !loading && started.elapsed() > LOOK * 5 => Some(true),
            _ if started.elapsed() > LOAD_WAIT => Some(false),
            _ => None,
        };
        match done {
            Some(ok) => {
                if let Some(f) = f.borrow_mut().take() {
                    f(ok);
                }
            }
            None => look(key, id, started, f),
        }
    });
}

/// The tab showing `view`, by project and id.
fn tab_of(view: &WKWebView) -> Option<(String, u64)> {
    WEBS.with(|w| {
        w.borrow().iter().find_map(|(key, web)| {
            (web.tabs.iter())
                .find(|t| std::ptr::eq(&*t.view, view))
                .map(|t| (key.clone(), t.id))
        })
    })
}

/// What WebKit said of a tab's load.
fn heard(view: &WKWebView, failed: Option<bool>) {
    let Some((key, id)) = tab_of(view) else {
        return;
    };
    WEBS.with(|w| {
        if let Some(t) = (w.borrow_mut().get_mut(&key))
            .and_then(|web| web.tabs.iter_mut().find(|t| t.id == id))
        {
            t.pending = None;
            if let Some(f) = failed {
                t.failed = f;
            }
        }
    });
    let pane = WEBS.with(|w| w.borrow().get(&key).and_then(|web| web.pane.clone()));
    if let Some(p) = pane {
        p.refresh();
    }
}

/// A page asked for a new window, a link with a target or `window.open`:
/// it opens as a tab beside its opener, driven by the opener's agent and
/// shown when the opener was.
fn popup(view: &WKWebView, url: Option<String>) {
    let Some((key, id)) = tab_of(view) else {
        return;
    };
    let (driver, front) = WEBS.with(|w| {
        let w = w.borrow();
        let web = w.get(&key);
        let at = web.and_then(|web| web.tabs.iter().position(|t| t.id == id));
        (
            at.and_then(|i| web?.tabs[i].driver.clone()),
            at.is_some_and(|i| web.is_some_and(|web| web.active == i)),
        )
    });
    let url = url.filter(|u| !u.is_empty() && u != "about:blank");
    add_tab(&key, url.as_deref(), driver, front);
}

// The DevTools protocol, stood in for.

/// Runs the protocol call `method` in the tab and hands `done` its answer.
fn run(
    key: &str,
    id: u64,
    method: &str,
    params: &Value,
    done: impl FnOnce(Result<Value, String>) + 'static,
) {
    let Some(view) = read_tab(key, Some(id), |t| t.view.clone()) else {
        return done(Err("the tab is gone".into()));
    };
    match devtools::plan(method, params) {
        Err(e) => done(Err(e)),
        Ok(Plan::Script { body, fallback }) => {
            let evaluate = method == "Runtime.evaluate";
            let again = view.clone();
            script(&view, &body, move |r| match r {
                Err(e) if devtools::unparsed(&e) && fallback.is_some() => {
                    let fallback = fallback.unwrap_or_default();
                    script(&again, &fallback, move |r| done(answer(r, evaluate)));
                }
                r => done(answer(r, evaluate)),
            });
        }
        Ok(Plan::Picture { clip, beyond }) => picture(&view, clip, beyond, done),
    }
}

/// A script's answer as the protocol gives it: an evaluation that threw
/// answers with the exception, as Runtime.evaluate does.
fn answer(r: Result<String, String>, evaluate: bool) -> Result<Value, String> {
    match r {
        Ok(json) => Ok(serde_json::from_str(&json).unwrap_or(Value::Null)),
        Err(e) if evaluate => Ok(devtools::thrown(&e)),
        Err(e) => Err(e),
    }
}

/// Runs `body`, an async function's, in the page's own world, and hands
/// `done` the string it returns or what went wrong.
fn script(view: &WKWebView, body: &str, done: impl FnOnce(Result<String, String>) + 'static) {
    let mtm = mtm();
    let done = RefCell::new(Some(done));
    let block = RcBlock::new(move |result: *mut AnyObject, err: *mut NSError| {
        let Some(done) = done.borrow_mut().take() else {
            return;
        };
        // SAFETY: WebKit hands either a result or an error, alive for the
        // call.
        if let Some(e) = unsafe { err.as_ref() } {
            return done(Err(js_error(e)));
        }
        let text = unsafe { result.as_ref() }
            .and_then(|o| o.downcast_ref::<NSString>())
            .map(|s| s.to_string());
        done(Ok(text.unwrap_or_else(|| "null".into())));
    });
    unsafe {
        view.callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(
            &NSString::from_str(body),
            None,
            None,
            &WKContentWorld::pageWorld(mtm),
            Some(&block),
        );
    }
}

/// What the page threw, rather than WebKit's word for it.
fn js_error(e: &NSError) -> String {
    let key = NSString::from_str("WKJavaScriptExceptionMessage");
    let said = e
        .userInfo()
        .objectForKey(&key)
        .and_then(|o| o.downcast::<NSString>().ok())
        .map(|s| s.to_string());
    said.unwrap_or_else(|| e.localizedDescription().to_string())
}

/// A picture of the page as Page.captureScreenshot answers, `clip` in
/// page pixels. A clip out of view is taken with the page grown to hold
/// it, then put back.
fn picture(
    view: &Retained<WKWebView>,
    clip: Option<Clip>,
    beyond: bool,
    done: impl FnOnce(Result<Value, String>) + 'static,
) {
    let Some(clip) = clip else {
        return snapshot(view, None, done);
    };
    let v = view.clone();
    script(
        view,
        "return JSON.stringify([scrollX, scrollY, innerWidth, innerHeight]);",
        move |r| {
            let m: Vec<f64> = r
                .ok()
                .and_then(|j| serde_json::from_str(&j).ok())
                .unwrap_or_default();
            let &[sx, sy, vw, vh] = m.as_slice() else {
                return done(Err("the page did not say where it is".into()));
            };
            match devtools::shot(clip, (sx, sy), (vw, vh), TALLEST) {
                Shot::InView(r) => snapshot(&v, Some(r), done),
                Shot::Grow { .. } if !beyond => {
                    done(Err("that part of the page is out of view".into()))
                }
                Shot::Grow { grow, top } => grown(&v, clip, (sx, sy), grow, top, done),
            }
        },
    );
}

/// The picture of a clip out of view: the view grows to `grow` tall and
/// the page scrolls to `top`, then both go back.
fn grown(
    view: &Retained<WKWebView>,
    clip: Clip,
    was: (f64, f64),
    grow: f64,
    top: f64,
    done: impl FnOnce(Result<Value, String>) + 'static,
) {
    let frame = view.frame();
    view.setFrameSize(NSSize::new(frame.size.width, grow));
    let v = view.clone();
    let scroll = format!(
        "scrollTo({{ left: {}, top: {top}, behavior: 'instant' }}); \
         await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))); \
         return JSON.stringify([scrollX, scrollY]);",
        was.0
    );
    script(view, &scroll, move |r| {
        let at: Vec<f64> = r
            .ok()
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();
        let (sx, sy) = match at.as_slice() {
            &[x, y] => (x, y),
            _ => (was.0, top),
        };
        let rect = Clip {
            x: (clip.x - sx).max(0.0),
            y: (clip.y - sy).max(0.0),
            width: clip.width,
            height: clip.height.min(grow),
        };
        let back = v.clone();
        snapshot(&v, Some(rect), move |shot| {
            back.setFrameSize(frame.size);
            let put = format!(
                "scrollTo({{ left: {}, top: {}, behavior: 'instant' }}); return 'null';",
                was.0, was.1
            );
            script(&back, &put, |_| {});
            done(shot);
        });
    });
}

/// A PNG of `rect` of the view, or all it shows, as Page.captureScreenshot
/// answers: about one pixel to a CSS pixel, as WebView2 gives.
fn snapshot(
    view: &WKWebView,
    rect: Option<Clip>,
    done: impl FnOnce(Result<Value, String>) + 'static,
) {
    let mtm = mtm();
    let scale = view.window().map_or(2.0, |w| w.backingScaleFactor());
    let config = unsafe { WKSnapshotConfiguration::new(mtm) };
    let width = match rect {
        Some(r) => {
            unsafe {
                config.setRect(cg_rect(&Rect::new(
                    r.x as f32,
                    r.y as f32,
                    r.width as f32,
                    r.height as f32,
                )))
            };
            r.width
        }
        None => view.bounds().size.width,
    };
    unsafe {
        config.setSnapshotWidth(Some(&NSNumber::numberWithDouble(width / scale.max(1.0))));
        config.setAfterScreenUpdates(true);
    }
    let done = RefCell::new(Some(done));
    let block = RcBlock::new(move |image: *mut NSImage, err: *mut NSError| {
        let Some(done) = done.borrow_mut().take() else {
            return;
        };
        // SAFETY: WebKit hands either an image or an error, alive for the
        // call.
        let image = unsafe { image.as_ref() };
        let Some(image) = image else {
            let why = unsafe { err.as_ref() }
                .map(|e| e.localizedDescription().to_string())
                .unwrap_or_else(|| "the page could not be pictured".into());
            return done(Err(why));
        };
        done(png(image).map(|data| json!({ "data": data })))
    });
    unsafe { view.takeSnapshotWithConfiguration_completionHandler(Some(&config), &block) };
}

/// `image` as a PNG, in base64.
fn png(image: &NSImage) -> Result<String, String> {
    let tiff = image.TIFFRepresentation().ok_or("the picture has no bitmap")?;
    let rep = NSBitmapImageRep::imageRepWithData(&tiff).ok_or("the picture has no bitmap")?;
    let props = NSDictionary::new();
    let data = unsafe {
        NSBitmapImageRep::representationUsingType_properties(
            &rep,
            NSBitmapImageFileType::PNG,
            &props,
        )
    }
    .ok_or("the picture could not be made a PNG")?;
    Ok(horadric_core::release::base64_encode(&data.to_vec()))
}

// WebKit's delegate: loads and new windows.

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this does not
    // implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct Watcher;

    unsafe impl NSObjectProtocol for Watcher {}

    unsafe impl WKNavigationDelegate for Watcher {
        #[unsafe(method(webView:didCommitNavigation:))]
        fn did_commit(&self, view: &WKWebView, _nav: Option<&WKNavigation>) {
            heard(view, Some(false));
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, view: &WKWebView, _nav: Option<&WKNavigation>) {
            heard(view, None);
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn did_fail(&self, view: &WKWebView, _nav: Option<&WKNavigation>, _e: &NSError) {
            heard(view, Some(true));
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn did_fail_early(&self, view: &WKWebView, _nav: Option<&WKNavigation>, _e: &NSError) {
            heard(view, Some(true));
        }
    }

    unsafe impl WKUIDelegate for Watcher {
        #[unsafe(method(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn create(
            &self,
            view: &WKWebView,
            _config: &WKWebViewConfiguration,
            action: &WKNavigationAction,
            _features: &WKWindowFeatures,
        ) -> *mut WKWebView {
            let url = unsafe { action.request() }
                .URL()
                .and_then(|u| u.absoluteString())
                .map(|s| s.to_string());
            // After WebKit's call: a new tab lays the stage out again.
            let view = view.retain();
            queue::after_main(Duration::ZERO, move || popup(&view, url));
            std::ptr::null_mut()
        }
    }
);

impl Watcher {
    fn new(mtm: MainThreadMarker) -> Retained<Watcher> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

// The pane: a tab strip, an address bar, and the shown page.

pub struct PaneIvars {
    key: RefCell<String>,
    bar: RefCell<Option<Bar>>,
}

struct Bar {
    tabs: Vec<Retained<NSButton>>,
    add: Retained<NSButton>,
    shut: Retained<NSButton>,
    back: Retained<NSButton>,
    forward: Retained<NSButton>,
    reload: Retained<NSButton>,
    address: Retained<NSTextField>,
    /// What the address field last showed, so typing in it is not undone
    /// by a refresh while the page stays where it is.
    shown_url: String,
}

define_class!(
    // SAFETY: NSView has no subclassing requirements, and this view does
    // not implement Drop.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = PaneIvars]
    pub struct WebPane;

    impl WebPane {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            if let Some(p) = Painter::current() {
                let b = self.bounds();
                let all = Rect::new(0.0, 0.0, b.size.width as f32, b.size.height as f32);
                p.fill_rect(&all, theme::surface());
            }
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            let _: () = unsafe { msg_send![super(self), setFrameSize: size] };
            self.lay_out();
        }

        #[unsafe(method(pick:))]
        fn pick_pressed(&self, sender: &NSButton) {
            let key = self.key();
            let at = sender.tag() as usize;
            WEBS.with(|w| {
                if let Some(web) = w.borrow_mut().get_mut(&key) {
                    if at < web.tabs.len() {
                        web.active = at;
                    }
                }
            });
            retiled(&key);
            focus(&key);
        }

        #[unsafe(method(add:))]
        fn add_pressed(&self, _sender: &NSButton) {
            let key = self.key();
            add_tab(&key, None, None, true);
            if let Some(b) = self.ivars().bar.borrow().as_ref() {
                if let Some(w) = self.window() {
                    w.makeFirstResponder(Some(&b.address));
                }
            }
        }

        #[unsafe(method(shut:))]
        fn shut_pressed(&self, _sender: &NSButton) {
            let key = self.key();
            let Some(id) = read_tab(&key, None, |t| t.id) else {
                return;
            };
            close_tab(&key, id);
            if tab_count(&key) == 0 {
                let id = pane_id(&key);
                super::app::input(super::app::Input::PaneClose(id));
            }
        }

        #[unsafe(method(back:))]
        fn back_pressed(&self, _sender: &NSButton) {
            go(&self.key(), None, Step::Back);
        }

        #[unsafe(method(forward:))]
        fn forward_pressed(&self, _sender: &NSButton) {
            go(&self.key(), None, Step::Forward);
        }

        #[unsafe(method(reload:))]
        fn reload_pressed(&self, _sender: &NSButton) {
            go(&self.key(), None, Step::Reload);
        }

        #[unsafe(method(go:))]
        fn address_entered(&self, sender: &NSTextField) {
            let typed = sender.stringValue().to_string();
            let Some(url) = browse::address(&typed) else {
                return;
            };
            let key = self.key();
            if let Some(id) = read_tab(&key, None, |t| t.id) {
                navigate(&key, id, &url);
                focus(&key);
            }
        }
    }

    unsafe impl NSObjectProtocol for WebPane {}
);

impl WebPane {
    fn new(mtm: MainThreadMarker, key: &str) -> Retained<WebPane> {
        let this = Self::alloc(mtm).set_ivars(PaneIvars {
            key: RefCell::new(key.to_string()),
            bar: RefCell::new(None),
        });
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(400.0, 300.0));
        let pane: Retained<WebPane> = unsafe { msg_send![super(this), initWithFrame: frame] };
        pane.build(mtm);
        pane
    }

    fn key(&self) -> String {
        self.ivars().key.borrow().clone()
    }

    fn button(&self, mtm: MainThreadMarker, title: &str, action: Sel) -> Retained<NSButton> {
        let target: &AnyObject = self.as_ref();
        let b = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(title),
                Some(target),
                Some(action),
                mtm,
            )
        };
        b.setBezelStyle(NSBezelStyle::Push);
        self.addSubview(&b);
        b
    }

    fn build(&self, mtm: MainThreadMarker) {
        let back = self.button(mtm, "\u{2039}", sel!(back:));
        let forward = self.button(mtm, "\u{203A}", sel!(forward:));
        let reload = self.button(mtm, "\u{21BB}", sel!(reload:));
        let add = self.button(mtm, "+", sel!(add:));
        let shut = self.button(mtm, "\u{00D7}", sel!(shut:));
        let address = NSTextField::textFieldWithString(&NSString::from_str(""), mtm);
        address.setPlaceholderString(Some(&NSString::from_str("Search or type an address")));
        address.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        let target: &AnyObject = self.as_ref();
        unsafe {
            address.setTarget(Some(target));
            address.setAction(Some(sel!(go:)));
        }
        self.addSubview(&address);
        *self.ivars().bar.borrow_mut() = Some(Bar {
            tabs: Vec::new(),
            add,
            shut,
            back,
            forward,
            reload,
            address,
            shown_url: String::new(),
        });
        self.lay_out();
    }

    /// The tab strip on top, the address bar under it, the page below.
    fn lay_out(&self) {
        let bar = self.ivars().bar.borrow();
        let Some(b) = bar.as_ref() else {
            return;
        };
        let w = self.bounds().size.width;
        let row = |x: f64, y: f64, width: f64| {
            NSRect::new(NSPoint::new(x, y), NSSize::new(width.max(0.0), ROW_H - 2.0))
        };
        let mut x = 4.0;
        let room = (w - 8.0 - 2.0 * NAV_W).max(0.0);
        let each = if b.tabs.is_empty() {
            TAB_W
        } else {
            (room / b.tabs.len() as f64).min(TAB_W)
        };
        for t in &b.tabs {
            t.setFrame(row(x, 2.0, each - 2.0));
            x += each;
        }
        b.add.setFrame(row(x, 2.0, NAV_W));
        b.shut.setFrame(row(w - NAV_W - 4.0, 2.0, NAV_W));
        let y = ROW_H + 4.0;
        b.back.setFrame(row(4.0, y, NAV_W));
        b.forward.setFrame(row(4.0 + NAV_W, y, NAV_W));
        b.reload.setFrame(row(4.0 + 2.0 * NAV_W, y, NAV_W));
        let ax = 8.0 + 3.0 * NAV_W;
        b.address.setFrame(row(ax, y, w - ax - 4.0));
        drop(bar);
        let page = self.page_frame();
        for v in self.subviews().iter() {
            if v.isKindOfClass(WKWebView::class()) {
                v.setFrame(page);
            }
        }
    }

    fn page_frame(&self) -> NSRect {
        let b = self.bounds();
        NSRect::new(
            NSPoint::new(0.0, BAR_H),
            NSSize::new(b.size.width, (b.size.height - BAR_H).max(0.0)),
        )
    }

    /// Shows `view` as the page, in place of the one shown before.
    fn hold(&self, view: &WKWebView) {
        let me: &NSView = self;
        let page: &NSView = view;
        let mine = unsafe { view.superview() }.is_some_and(|p| std::ptr::eq(&*p, me));
        for v in self.subviews().iter() {
            if std::ptr::eq(&*v, page) {
                continue;
            }
            let any: &AnyObject = v.as_ref();
            if let Some(web) = any.downcast_ref::<WKWebView>() {
                park(mtm(), web);
            }
        }
        if !mine {
            view.removeFromSuperview();
            self.addSubview(view);
        }
        view.setFrame(self.page_frame());
    }

    /// The tab names, which one shows, and the shown page's address.
    fn refresh(&self) {
        let key = self.key();
        let (names, active, url, back, forward) = WEBS.with(|w| {
            let w = w.borrow();
            let Some(web) = w.get(&key) else {
                return (Vec::new(), 0, String::new(), false, false);
            };
            let names: Vec<String> = (web.tabs.iter())
                .map(|t| browse::tab_name(&t.title(), &t.url()))
                .collect();
            let shown = web.tabs.get(web.active);
            let url = shown.map(|t| t.url()).unwrap_or_default();
            let (back, forward) = shown.map_or((false, false), |t| unsafe {
                (t.view.canGoBack(), t.view.canGoForward())
            });
            (names, web.active, url, back, forward)
        });
        let mtm = MainThreadMarker::from(self);
        let mut relaid = false;
        {
            let mut bar = self.ivars().bar.borrow_mut();
            let Some(b) = bar.as_mut() else {
                return;
            };
            while b.tabs.len() > names.len() {
                if let Some(t) = b.tabs.pop() {
                    t.removeFromSuperview();
                }
                relaid = true;
            }
            while b.tabs.len() < names.len() {
                let target: &AnyObject = self.as_ref();
                let t = unsafe {
                    NSButton::buttonWithTitle_target_action(
                        &NSString::from_str(""),
                        Some(target),
                        Some(sel!(pick:)),
                        mtm,
                    )
                };
                t.setBezelStyle(NSBezelStyle::Push);
                t.setTag(b.tabs.len() as isize);
                self.addSubview(&t);
                b.tabs.push(t);
                relaid = true;
            }
            for (i, (t, name)) in b.tabs.iter().zip(&names).enumerate() {
                t.setTitle(&NSString::from_str(name));
                let font = if i == active {
                    NSFont::boldSystemFontOfSize(12.0)
                } else {
                    NSFont::systemFontOfSize(12.0)
                };
                t.setFont(Some(&font));
                t.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            }
            b.back.setEnabled(back);
            b.forward.setEnabled(forward);
            if b.shown_url != url {
                b.address.setStringValue(&NSString::from_str(&url));
                b.shown_url = url;
            }
        }
        if relaid {
            self.lay_out();
        }
        if let Some(stage) = unsafe { self.superview() } {
            stage.setNeedsDisplay(true);
        }
    }
}

/// Keeps the shown panes' bars up to date with what pages say after they
/// load: titles a script sets, a history step within the page.
fn watch() {
    queue::after_main(Duration::from_secs(1), || {
        let panes: Vec<Retained<WebPane>> = WEBS.with(|w| {
            (w.borrow().values())
                .filter_map(|web| web.pane.clone())
                .filter(|p| unsafe { p.superview() }.is_some())
                .collect()
        });
        for p in panes {
            p.refresh();
        }
        watch();
    });
}
