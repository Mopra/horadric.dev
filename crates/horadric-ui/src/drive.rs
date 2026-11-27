//! The app's side of an agent driving its project's browser pane, through
//! `horadric mcp`. A call names the session, and the session names the
//! project, so an agent only ever reaches its own project's page.
//!
//! An agent opens and closes the page as the user's Browser does, but never
//! takes the stage or the keyboard: the page goes into its project's grid,
//! and shows when the stage shows that project. Everything else a page can
//! do goes over the DevTools protocol, which WebView2 lets the app call on
//! one page without the debugging port.
//!
//! Each session's agent keeps to a tab of its own, so the user showing
//! another tab, or another session opening one, never moves it.

use std::collections::HashSet;

use horadric_core::vault::identity::Resolved;
use horadric_core::Phase;
use horadric_hooks::listener::{BrowserCall, Reply};
use horadric_hooks::open_url;
use serde_json::{json, Value};

use super::{folder_key, App, Kind, WEB};
use crate::browse::{self, closed, error, Call};
use crate::companion::link;
use crate::companion::takeover::{self, Asker};
use crate::console::Console;
use crate::hotkey::Action;
use crate::quests::{Runner as _, SessionSpec, Workplace};
use crate::vault::{fill, identity, run};
use crate::web;
use crate::window::project_name;

impl App {
    /// Whether a call comes from a session the app started itself (a
    /// terminal or a Folio it runs) and shows that session's key.
    fn keyed(&self, session: &str, body: &Value) -> bool {
        let ours = self.consoles.contains_key(session) || self.folios.contains_key(session);
        ours && run::key_ok(session, body["key"].as_str().unwrap_or(""))
    }

    pub(super) fn browser_call(&mut self, call: BrowserCall) {
        // A desktop program is no project's, so these ask nothing of it.
        // They do show the session's key like every other call, before
        // anything else: without it any local program could put questions
        // on the human's screen under a session's name, write the
        // history, hold a program against the sessions or keep bypass
        // alive.
        if call
            .body
            .get("op")
            .and_then(Value::as_str)
            .is_some_and(|op| op.starts_with("desktop."))
        {
            if !self.keyed(&call.session, &call.body) {
                return call.reply.send(error(
                    "Horadric takes desktop calls only from a session it runs, with that \
                     session's key. A session begun before Horadric kept keys gets them once it \
                     is closed and resumed in Horadric; a session started outside Horadric gets \
                     them only when resumed inside Horadric.",
                ));
            }
            return self.takeover_call(call);
        }
        // The trail and the companion are no project's either. The key
        // matters most here: the trail holds everything that was on the
        // human's screen, and no other local program may ask for it under
        // a session's name.
        if let Some(op) = (call.body.get("op").and_then(Value::as_str))
            .filter(|op| op.starts_with("trail.") || op.starts_with("companion."))
        {
            if !self.keyed(&call.session, &call.body) {
                return call.reply.send(error(
                    "Horadric answers for the companion only to a session it runs, with that \
                     session's key.",
                ));
            }
            let op = op.to_string();
            return self.companion_call(&op, call);
        }
        // The agent's desk is no project's either. The key keeps any
        // other local program from opening it under a session's name.
        if let Some(op) =
            (call.body.get("op").and_then(Value::as_str)).filter(|op| op.starts_with("desk."))
        {
            if !self.keyed(&call.session, &call.body) {
                return call.reply.send(error(
                    "Horadric opens the agent's desk only for a session it runs, with that \
                     session's key.",
                ));
            }
            return call.reply.send(crate::desk::window::agent_call(op));
        }
        let reply = call.reply;
        // Its agent heard it fail while it waited behind something slow.
        if !reply.wanted() {
            return;
        }
        let Some(key) = self.project_of(&call.session) else {
            return reply.send(error("this session has no project in Horadric"));
        };
        if call.body["op"] == open_url::OPEN_OP {
            return self.open_url_call(&key, &call.session, &call.body, reply);
        }
        // The session id comes in a header any local program can set, and
        // a hook can make up a session the registry then adopts, so a call
        // counts only from a session the app started itself (a terminal or
        // a Folio it runs) that shows its key. `horadric mcp` sends the key
        // with every call.
        if !self.keyed(&call.session, &call.body) {
            return reply.send(error(
                "Horadric takes browser pane and Forge calls only from a session it runs, with \
                 that session's key. A session begun before Horadric kept keys gets them once \
                 it is closed and resumed in Horadric; a session started outside Horadric \
                 (horadric run, or a terminal of its own) gets them only when resumed inside \
                 Horadric.",
            ));
        }
        let Some(reply) = self.forge_call(&key, &call.body, reply) else {
            return;
        };
        // After the key check, so only a session Horadric runs can put the
        // remote desktop question to the human.
        let Some(reply) = crate::remote_gate::call(&key, &call.session, &call.body, reply) else {
            return;
        };
        let session = call.session.as_str();
        let what = match Call::parse(&call.body) {
            Ok(c) => c,
            Err(e) => return reply.send(error(&e)),
        };
        // Its own tab, whichever tab the user shows: see `web::pick_tab`.
        let live = self.live_sessions();
        let tab = |make: bool| web::agent_tab(&key, session, |s| live.contains(s), make);
        // A navigation while a login is typed in would carry the typing to
        // another page.
        if what != Call::Info && tab(false).is_some_and(fill::busy) {
            return reply.send(error(fill::BUSY));
        }
        match what {
            Call::Info => reply.send(info(&key, tab(false))),
            Call::Open(url) => {
                let had = web::is_open(&key).then(|| tab(false)).flatten();
                let Some(id) = self.open_web_for_agent(&key, session, url.as_deref()) else {
                    return reply.send(closed());
                };
                if had != Some(id) || url.is_some() {
                    answer_after_load(&key, id, reply);
                } else {
                    let k = key.clone();
                    web::with_view(&key, id, move |_| reply.send(info(&k, Some(id))));
                }
            }
            Call::Navigate(url) => {
                let Some(id) = self.open_web_for_agent(&key, session, Some(&url)) else {
                    return reply.send(closed());
                };
                answer_after_load(&key, id, reply);
            }
            Call::History(step) => {
                let Some(id) = tab(false) else {
                    return reply.send(closed());
                };
                let (back, forward) = web::history_of(&key, Some(id));
                if let Err(e) = browse::can_step(step, back, forward) {
                    return reply.send(error(&e));
                }
                web::go_in(&key, Some(id), step);
                answer_after_load(&key, id, reply);
            }
            Call::Close => {
                // Only its own tab: the others are the user's or another
                // agent's.
                let own = web::drivers(&key)
                    .iter()
                    .any(|d| d.as_deref() == Some(session));
                let id = own.then(|| tab(false)).flatten();
                let closed = id.is_some_and(|id| web::close_tab(&key, id));
                if !web::is_open(&key) && self.webs.contains_key(&key) {
                    self.close_web(&key);
                }
                reply.send(json!({ "closed": closed }));
            }
            Call::DevTools(method, params) => {
                let Some(id) = tab(false) else {
                    return reply.send(closed());
                };
                // A tab still being made holds the call until it is, which
                // may be after its agent gave up on it.
                let who = fill::Caller {
                    project: key.clone(),
                    identity: self.identity_of(&key),
                };
                web::with_view(&key, id, move |_| {
                    if !reply.wanted() {
                        return;
                    }
                    fill::devtools(who, id, method, params, reply);
                });
            }
            Call::FillLogin {
                name,
                key: secret_key,
                user,
                password,
                code,
            } => {
                let Some(id) = tab(false) else {
                    return reply.send(closed());
                };
                let req = fill::Request {
                    project: key.clone(),
                    identity: self.identity_of(&key),
                    session: session.to_string(),
                    key: secret_key,
                    name,
                    user,
                    password,
                    code,
                };
                web::with_view(&key, id, move |_| {
                    if reply.wanted() {
                        fill::fill(req, id, reply);
                    }
                });
            }
        }
    }

    /// A call about the companion from `horadric mcp`: `trail.recall`
    /// asks what the human was doing, `companion.offer` hands the human a
    /// text to type. The caller has shown the session's key already.
    fn companion_call(&mut self, op: &str, call: BrowserCall) {
        let id = call.session.as_str();
        match op {
            "trail.recall" => {
                // Only a session the human sits at. Warriv, a reviewer, an
                // errand and a quest's session work unwatched, where a page
                // they read could talk them into fetching the trail. So
                // does the companion's own session, which has the minutes
                // it needs in its prompt.
                let asker = if self.companions_own(id) {
                    Asker::Companion
                } else if horadric_core::warriv::is_warriv(id)
                    || horadric_core::runeword::is_errand(id)
                    || self.holds_quest(id)
                {
                    Asker::Unwatched
                } else {
                    Asker::Human
                };
                let who = format!("{} ({id})", self.name_of(id));
                takeover::recall(who, asker, &call.body, call.reply);
            }
            "companion.offer" => {
                if !self.companions_own(id) {
                    return call.reply.send(error(
                        "Only the companion's own session, started by a take over, offers the \
                         human a text.",
                    ));
                }
                let Some(text) = call.body["text"].as_str().filter(|t| !t.trim().is_empty())
                else {
                    return call.reply.send(error("say what to offer: text"));
                };
                if let Err(why) = takeover::offer(text) {
                    return call.reply.send(error(&why));
                }
                let i = Action::CompanionType.index();
                // The key is held only while the companion is on.
                let how = match (link::is_on(), self.keys_live[i]) {
                    (false, _) => "Turn the companion on in Settings to type it".to_string(),
                    (true, true) => {
                        format!("{} types it where your cursor is", self.keys[i].name())
                    }
                    (true, false) => "Set a key to type it in Settings, under Keys".to_string(),
                };
                self.toasts.show(
                    Kind::Info,
                    "The companion has a text",
                    &format!("{}\n{how}", takeover::offer_shown(text)),
                );
                call.reply.send(json!({ "text": takeover::OFFER_TAKEN }));
            }
            _ => call.reply.send(error("unknown companion call")),
        }
    }

    /// Whether session `id` is the companion's own, started by a take
    /// over: its project is the companion's folder.
    fn companions_own(&self, id: &str) -> bool {
        let folder = takeover::folder().map(|d| folder_key(&d.to_string_lossy()));
        folder.is_some() && self.project_of(id) == folder
    }

    /// Starts a session for each take over that has its sentence, in the
    /// companion's own folder, with the trail in its first prompt. Never
    /// shown on the stage and never given the keyboard: the human is in
    /// another program and stays there.
    pub(super) fn companion_start(&mut self) {
        for ready in takeover::ready() {
            let started = takeover::folder()
                .ok_or_else(|| "it has no folder to work in".to_string())
                .and_then(|dir| {
                    // A folder nobody was asked about would stop the
                    // session at Claude Code's question, unseen.
                    if !crate::accounts::trusted(&dir) {
                        if let Err(e) = crate::accounts::trust(&dir) {
                            eprintln!("horadric: cannot trust {}: {e}", dir.display());
                        }
                    }
                    self.start_session(SessionSpec {
                        name: Some("Companion".into()),
                        prompt: Some(ready.prompt),
                        place: Workplace::Here,
                        show: false,
                        quiet: true,
                        ..SessionSpec::new(dir, self.chat_sessions)
                    })
                });
            match started {
                Ok(_) => self.toasts.show(
                    Kind::Info,
                    "The companion took it over",
                    &ready.instruction,
                ),
                Err(e) => self
                    .toasts
                    .show(Kind::Failed, "The companion could not take it over", &e),
            }
        }
    }

    /// The key that types the companion's offer where the caret is, in
    /// whatever program has the keyboard.
    pub(super) fn companion_type(&mut self) {
        match takeover::take_offer() {
            // On a thread: it waits for the chord's modifiers to be let go.
            Some(text) => {
                std::thread::spawn(move || crate::dictate::anywhere::type_text(&text));
            }
            None => self
                .toasts
                .show(Kind::Info, "The companion has nothing to type", ""),
        }
    }

    /// The pause key.
    pub(super) fn companion_pause(&mut self) {
        if !link::is_on() {
            return self.toasts.show(
                Kind::Info,
                "The companion is off",
                "Turn it on in Settings, under General",
            );
        }
        let title = match link::toggle_pause() {
            true => "Companion paused",
            false => "Companion watching",
        };
        // A pause is kept, so a reload does not undo it.
        self.save();
        self.toasts.show(Kind::Info, title, "");
    }

    /// `horadric open-url` from a terminal of the project `key`, a
    /// session's or a plain shell's: the link opens in a tab of its own on
    /// the stage, with the keyboard, since it is nearly always a sign in
    /// the human just started and finishes with a click there. Its key is
    /// the pane's, not the session's secret key, so a plain shell can hold
    /// one without being able to drive the page or run secrets.
    fn open_url_call(&mut self, key: &str, pane: &str, body: &Value, reply: Reply) {
        let ours = self.consoles.contains_key(pane) || self.folios.contains_key(pane);
        let given = body["key"].as_str().unwrap_or("");
        if !ours || !run::key_ok(&open_url::key_name(pane), given) {
            return reply.send(error("not a terminal Horadric runs, or not its key"));
        }
        let Some(url) = body["url"].as_str().and_then(open_url::web_url) else {
            return reply.send(error("only http and https addresses open in the pane"));
        };
        self.open_web_tab(key, url);
        reply.send(json!({ "opened": true }));
    }

    /// The identity the project's sessions work as, if it has one.
    fn identity_of(&self, key: &str) -> Option<String> {
        match self.project_dir(key).map(|d| identity::of_project(&d)) {
            Some(Resolved::Identity(slug)) => Some(slug),
            _ => None,
        }
    }

    /// The sessions whose agents can still drive a page: a tab whose agent
    /// has ended is free for another.
    fn live_sessions(&self) -> HashSet<String> {
        let Ok(r) = self.shared.registry.lock() else {
            return HashSet::new();
        };
        r.all()
            .filter(|s| s.phase != Phase::Ended)
            .map(|s| s.id.clone())
            .collect()
    }

    /// Opens the project's browser pane, without the stage or the keyboard
    /// (the user may be looking at another project), and gives `session`
    /// a tab of its own in it, at `url` when given. Says which tab.
    fn open_web_for_agent(&mut self, key: &str, session: &str, url: Option<&str>) -> Option<u64> {
        if !self.webs.contains_key(key) {
            let serial = self.next_serial;
            self.next_serial += 1;
            let page = Console::web(format!("{WEB}{key}"), serial, key.to_string());
            self.webs.insert(key.to_string(), page);
        }
        let live = self.live_sessions();
        // Without an address it only wants to look, which the user's page
        // allows: a tab of its own only when there is nothing to look at.
        let id = web::agent_tab(key, session, |s| live.contains(s), url.is_some())
            .or_else(|| web::agent_tab(key, session, |s| live.contains(s), true));
        if let (Some(id), Some(u)) = (id, url) {
            web::navigate(key, id, u);
        }
        if self.stage.as_ref().map(|s| s.project()).as_deref() == Some(key) {
            self.sync_stage();
        }
        id
    }
}

/// Answers once the tab's navigation ends, with where it ended up.
fn answer_after_load(key: &str, id: u64, reply: Reply) {
    let k = key.to_string();
    web::after_load(key, id, move |ok| {
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
        .and_then(|id| web::label_of(key, Some(id)))
        .unwrap_or_default();
    let place = tab.and_then(|id| web::place_of(key, id));
    browse::info(
        web::is_open(key),
        web::is_shown(key),
        &project_name(key),
        place,
        &title,
        &url,
    )
}
