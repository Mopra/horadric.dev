//! The Mac as a brother (see docs/plan/the-brotherhood.md): it signs in
//! with WorkOS's device code flow, links to the relay as a Windows PC
//! does, lets a brother in on the six digit code its human sees, and
//! serves its own sessions to its brothers through the one
//! `horadric_core::serving::Server`, so a PC shows the Mac's tiles and
//! panes as it shows a Linux node's, and a project's files read only
//! (`tree`, `read`, `changes`). Its client of each brother, which shows
//! the brother's tiles and panes here, is `far.rs`.
//!
//! The link, its threads and its sign in are `horadric_link::relay` and
//! `horadric_link::signin`. What the link hears comes to the main thread
//! as an [`app::Input::Kin`], since the sessions it serves live there.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use horadric_core::brothers::{Asking, Event, Human, Listener};
use horadric_core::joining::Listed;
use horadric_core::portal::{b64url, Kind};
use horadric_core::serving::{afar_files_error, answers, Job, Running, Serve, Server, LOOK_EVERY};
use horadric_core::standing::Standing;
use horadric_core::{rostering, Phase};
use horadric_link::relay::{self, Link, LinkCrypto, Setup};
use horadric_link::signin::{Shown, Workos};
use serde_json::{json, Value};

use crate::console::GridSize;
use crate::portal;
use crate::project::{folder_key, project_key, project_name};

use super::app::{self, App, Input};
use super::menu::Item;
use super::{dialog, queue};

/// How often the server looks for what changed while something streams,
/// so typing on a PC echoes without a lag.
const STREAM_EVERY: Duration = Duration::from_millis(20);

/// Menu tags of the Brothers submenu. A listed brother's starts at
/// [`JOIN_TAG`].
pub(super) const SIGN_IN_TAG: usize = 300;
pub(super) const SIGN_OUT_TAG: usize = 301;
pub(super) const JOIN_TAG: usize = 310;
/// The most brothers the menu lists, as the relay lets one human have 16
/// machines connected.
const JOIN_MAX: usize = 32;

/// Something the link, a job or the sign in says, for the main thread.
pub enum Note {
    Up {
        ch: u32,
        id: String,
        name: String,
    },
    Asked {
        ch: u32,
        id: String,
        msg: Value,
    },
    Down {
        ch: u32,
    },
    /// A brother's answer to what the Mac's client of it asked.
    Heard {
        ch: u32,
        msg: Value,
    },
    /// Brother `id` was cast out.
    Gone {
        id: String,
    },
    /// What a brother's pane here asked, for that brother.
    FarAsk(String, crate::console::FarAsk),
    /// A job's answer for channel `ch`.
    Answer {
        ch: u32,
        msg: Value,
    },
    /// A question for the human, answered through the brothers.
    Ask {
        token: u64,
        text: String,
        then: Then,
    },
    /// Something to tell the human that needs no answer.
    Tell(String),
    /// Signing in: the link and the code to see there.
    Device(Shown),
    SignedIn(Result<String, String>),
    /// The brothers or the relay changed: the menu lists them anew.
    Changed,
}

/// What a yes or no to a question goes to.
#[derive(Clone, Copy)]
pub enum Then {
    Join,
    Leave,
    Contested,
    Readmit,
}

/// The link, once started. It outlives every reload of the app's state.
static LINK: OnceLock<Link> = OnceLock::new();
static WORKOS: OnceLock<Arc<Workos>> = OnceLock::new();

fn tell_app(note: Note) {
    queue::post(move || app::input(Input::Kin(note)));
}

/// What the Mac keeps of its brothers on the main thread.
#[derive(Default)]
pub(super) struct Kin {
    server: Server,
    /// The brother on each linked relay channel, by id.
    linked: HashMap<u32, String>,
    /// The brothers the menu lists, in its order, so a tag finds one.
    listed: Vec<Listed>,
    /// Whether the relay was reached when the menu was last built.
    up: bool,
    /// Signing in runs now.
    signing_in: bool,
    /// The pump runs.
    pumping: bool,
    /// The Mac as a client of each brother.
    pub(super) far: super::far::Far,
}

/// The state folder, where `brothers.json` and the sign in live.
fn dir() -> Option<PathBuf> {
    horadric_hooks::state_dir()
}

/// What the Mac calls itself to its brothers: `HORADRIC_PC_NAME`, as a
/// PC's, else the name the Mac's Sharing settings give it.
pub(super) fn mac_name() -> String {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        std::env::var("HORADRIC_PC_NAME")
            .ok()
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .or_else(computer_name)
            .unwrap_or_else(|| "Mac".into())
    })
    .clone()
}

fn computer_name() -> Option<String> {
    let out = std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !name.is_empty()).then_some(name)
}

fn workos() -> Option<Arc<Workos>> {
    let dir = dir()?;
    Some(Arc::clone(WORKOS.get_or_init(|| {
        Arc::new(Workos::new(dir, horadric_hooks::dev()))
    })))
}

/// Starts the relay link, which waits until the Mac is signed in. Once:
/// the link lives as long as the process.
pub(super) fn start() {
    if LINK.get().is_some() {
        return;
    }
    let (Some(dir), Some(sign_in)) = (dir(), workos()) else {
        return;
    };
    let setup = Setup {
        dir,
        name: mac_name(),
        sign_in,
        human: Box::new(Alerts),
        listener: Box::new(ToApp),
        // The Mac pairs phones of its own through no relay link yet.
        phones: None,
        log: |line| eprintln!("horadric brothers: {line}"),
    };
    match Link::start(setup) {
        Ok(link) => {
            let _ = LINK.set(link);
        }
        Err(e) => eprintln!("horadric brothers: cannot start the relay link: {e}"),
    }
}

fn link() -> Option<&'static Link> {
    LINK.get()
}

/// Hands what linked channels do to the main thread. Called with the
/// brothers held, so it only posts.
struct ToApp;

impl Listener for ToApp {
    fn event(&self, e: Event) {
        let note = match e {
            Event::Up { ch, id, name } => Note::Up { ch, id, name },
            Event::Asked { ch, id, msg } => Note::Asked { ch, id, msg },
            Event::Down { ch } => Note::Down { ch },
            Event::Heard { ch, msg } => Note::Heard { ch, msg },
            Event::Gone { id } => Note::Gone { id },
            // The Mac hands off and keeps nothing yet, so whose a session
            // is now changes nothing it shows; the menu lists brothers anew.
            Event::Moved { .. } => Note::Changed,
        };
        tell_app(note);
    }
}

/// The human, asked in AppKit alerts on the main thread. The brothers call
/// it with their lock held, so each question is posted and answered later.
struct Alerts;

impl Alerts {
    fn ask(&self, token: u64, text: String, then: Then) {
        tell_app(Note::Ask { token, text, then });
    }
}

impl Human for Alerts {
    fn let_in(&self, a: Asking) {
        self.ask(
            a.token,
            horadric_core::brothers::let_in_text(&a),
            Then::Join,
        );
    }

    fn confirm(&self, a: Asking) {
        self.ask(
            a.token,
            horadric_core::brothers::confirm_text(&a),
            Then::Join,
        );
    }

    fn tell(&self, text: &str) {
        tell_app(Note::Tell(text.to_string()));
    }

    /// A word in passing, not an alert: who let whom in.
    fn note(&self, text: &str) {
        dialog::notify("Horadric brothers", text);
        tell_app(Note::Changed);
    }

    fn cast_out_self(&self, token: u64, by: &str) {
        let text = format!(
            "{by} cast this Mac out of the brotherhood: it hears its brothers' rosters, but acts \
             on none of them and none acts on it.\n\n\
             Leave the brotherhood? Leaving lets every brother and this Mac's keys go: it comes \
             back only when a brother asks it to join again. Stay if you think {by} was stolen: \
             this Mac keeps its keys, and is a brother again the moment humans on brothers of \
             two lines cast {by} out."
        );
        self.ask(token, text, Then::Leave);
    }

    fn contested(&self, token: u64, by: &str, name: &str) {
        let text = format!(
            "{by} and {name} cast each other out, so neither acts on any brother \
             until a human decides.\n\n\
             Is {name} no longer yours, stolen or lost? Yes casts {name} out from \
             this Mac too, which decides it. Say no unless you know: {by} may be the \
             stolen one, and you are asked about it as well."
        );
        self.ask(token, text, Then::Contested);
    }

    fn readmit(&self, token: u64, name: &str, by: &str) {
        let text = format!(
            "{by} let {name} in again with a new key, which this Mac has not taken: \
             the key it holds for {name} is out, or {name} was cast out here.\n\n\
             Take the new key? Say yes only if you set {name} up anew yourself, \
             or know who did: a stolen brother could say this of any. Otherwise \
             {name} stays as it is here."
        );
        self.ask(token, text, Then::Readmit);
    }
}

fn micros(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A new connection's `s`, random, since relay frames can be recorded and
/// replayed.
fn fresh_s() -> String {
    let mut s = [0u8; 16];
    horadric_link::random(&mut s);
    b64url(&s)
}

/// Sends what the server answered to the brothers it is for, and runs the
/// jobs on threads of their own.
fn send_out(out: horadric_core::serving::Out) {
    let Some(link) = link() else {
        return;
    };
    for (ch, msg) in &out.msgs {
        link.reply(*ch, msg);
    }
    for (ch, job) in out.later {
        run_job(ch, job);
    }
}

/// Sends `msg` to the brother on channel `ch`, sealed.
pub(super) fn reply(ch: u32, msg: &Value) {
    if let Some(link) = link() {
        link.reply(ch, msg);
    }
}

fn run_job(ch: u32, job: Job) {
    std::thread::spawn(move || tell_app(Note::Answer { ch, msg: job() }));
}

/// Takes one note on the main thread, with the app.
pub(super) fn heard(app: &mut App, note: Note) {
    match note {
        Note::Up { ch, id, name } => {
            let mut kin = std::mem::take(&mut app.kin);
            kin.server.open(ch);
            kin.linked.insert(ch, id.clone());
            app.kin = kin;
            eprintln!("horadric brothers: linked to {name}");
            super::far::up(app, ch, &id, &name);
            menu_changed(app);
            pump(app);
        }
        Note::Asked { ch, id, mut msg } => {
            // Only a brother's key opens this channel, whatever its hello
            // says it is.
            if msg["t"] == "hello" {
                msg["kind"] = json!("brother");
            }
            let now = SystemTime::now();
            let back = app.kin.server.heard(ch, &id, &msg, micros(now), fresh_s);
            if let Some(link) = link() {
                for m in back {
                    link.reply(ch, &m);
                }
            }
            let mut kin = std::mem::take(&mut app.kin);
            let out = kin.server.answer(&mut Here(&mut *app), unix_now());
            app.kin = kin;
            send_out(out);
        }
        Note::Down { ch } => {
            app.kin.server.close(ch);
            app.kin.linked.remove(&ch);
            super::far::down(app, ch);
            menu_changed(app);
        }
        Note::Heard { ch, msg } => super::far::heard(app, ch, &msg),
        Note::Gone { id } => {
            super::far::gone(app, &id);
            menu_changed(app);
        }
        Note::FarAsk(id, ask) => super::far::ask(app, &id, ask),
        // A lore catch up is several messages, and a job with nothing to
        // say answers null, which is no message.
        Note::Answer { ch, msg } => {
            if let Some(link) = link() {
                for m in answers(msg) {
                    link.reply(ch, &m);
                }
            }
        }
        Note::Ask { token, text, then } => ask(token, &text, then),
        Note::Tell(text) => dialog::info("Horadric brothers", &text),
        Note::Device(shown) => {
            let _ = std::process::Command::new("/usr/bin/open")
                .arg(&shown.link)
                .stdin(std::process::Stdio::null())
                .spawn();
            dialog::info(
                "Sign in to the brotherhood",
                &format!(
                    "Your browser opens WorkOS's sign in. Check that it shows the code {}, \
                     then confirm it there.\n\nIf no browser opened, go to {}",
                    shown.code, shown.link
                ),
            );
        }
        Note::SignedIn(done) => {
            app.kin.signing_in = false;
            match done {
                Ok(email) => {
                    if let Some(link) = link() {
                        link.renew();
                    }
                    dialog::notify("Horadric brothers", &format!("signed in as {email}"));
                }
                Err(e) => dialog::error("Horadric could not sign in", &e),
            }
            menu_changed(app);
        }
        Note::Changed => menu_changed(app),
    }
}

/// Asks the human, then gives the answer to the brothers. A question void
/// by now takes the answer and does nothing with it.
fn ask(token: u64, text: &str, then: Then) {
    let (yes, no) = match then {
        Then::Join => ("Let In", "No"),
        Then::Leave => ("Leave", "Stay"),
        Then::Contested => ("Cast Out", "No"),
        Then::Readmit => ("Take the Key", "No"),
    };
    let said = dialog::choose("Horadric brothers", text, yes, no);
    let Some(link) = link() else {
        return;
    };
    link.with(|b| match then {
        Then::Join => b.answer_for(token, said),
        Then::Leave => b.leave(token, said),
        Then::Contested => b.cast_out_contested(token, said),
        Then::Readmit => b.readmit(token, said),
    });
}

/// Keeps the server looking at what changed: faster while a brother
/// watches a pane stream, so its typing echoes at once. Once started it
/// runs for as long as a brother is linked.
fn pump(app: &mut App) {
    if app.kin.pumping {
        return;
    }
    app.kin.pumping = true;
    queue::after_main(LOOK_EVERY, || app::input(Input::KinTick));
}

/// One look at what changed, from the pump.
pub(super) fn tick(app: &mut App) {
    app.kin.pumping = false;
    if app.kin.linked.is_empty() {
        return;
    }
    let mut kin = std::mem::take(&mut app.kin);
    let out = kin.server.tick(&mut Here(&mut *app));
    let wait = if kin.server.streaming() {
        STREAM_EVERY
    } else {
        LOOK_EVERY
    };
    kin.pumping = true;
    app.kin = kin;
    send_out(out);
    queue::after_main(wait, || app::input(Input::KinTick));
}

/// The listed brothers changed, or the link did: they are read again for
/// the menu, which is rebuilt only when what it shows changed.
fn menu_changed(app: &mut App) {
    let (up, listed) = read_listed();
    app.kin.up = up;
    app.kin.listed = listed;
    app.menus();
}

fn read_listed() -> (bool, Vec<Listed>) {
    let up = link().is_some_and(Link::up);
    let listed = link()
        .and_then(Link::listed)
        .unwrap_or_default()
        .into_iter()
        .take(JOIN_MAX)
        .collect();
    (up, listed)
}

/// From the app's tick: the relay's list of brothers comes and changes
/// with no event of its own, so the menu is built again when it did.
pub(super) fn refresh(app: &mut App) {
    super::far::tick(app);
    let (up, listed) = read_listed();
    if up != app.kin.up || listed != app.kin.listed {
        app.kin.up = up;
        app.kin.listed = listed;
        app.menus();
    }
}

/// The Brothers submenu: who is signed in, each brother the relay lists,
/// a click on one not joined yet asking to join it.
pub(super) fn menu(app: &App) -> Item {
    let mut items = Vec::new();
    match workos().and_then(|w| w.user()) {
        Some(user) => {
            items.push(Item::disabled(format!("Signed in as {}", user.email)));
            items.push(Item::pick("Sign Out", SIGN_OUT_TAG));
        }
        None if app.kin.signing_in => items.push(Item::disabled("Signing in\u{2026}")),
        None => items.push(Item::pick("Sign In to Find Brothers\u{2026}", SIGN_IN_TAG)),
    }
    if app.kin.up {
        items.push(Item::Separator);
        if app.kin.listed.is_empty() {
            items.push(Item::disabled("No other machine is signed in"));
        }
        for (i, b) in app.kin.listed.iter().enumerate() {
            let label = b.label();
            if b.joined || b.old {
                items.push(Item::disabled(label));
            } else {
                items.push(Item::pick(format!("Join {label}\u{2026}"), JOIN_TAG + i));
            }
        }
    }
    Item::Sub("Brothers".into(), items)
}

/// A pick from the Brothers submenu.
pub(super) fn picked(app: &mut App, tag: usize) -> bool {
    match tag {
        SIGN_IN_TAG => sign_in(app),
        SIGN_OUT_TAG => {
            if let Some(w) = workos() {
                if let Err(e) = w.sign_out() {
                    dialog::error("Horadric could not sign out", &e);
                }
            }
            if let Some(link) = link() {
                link.renew();
            }
            menu_changed(app);
        }
        t if (JOIN_TAG..JOIN_TAG + JOIN_MAX).contains(&t) => {
            let Some(b) = app.kin.listed.get(t - JOIN_TAG).cloned() else {
                return true;
            };
            if let Some(link) = link() {
                link.with(|bs| bs.ask(&b.id));
            }
        }
        _ => return false,
    }
    true
}

fn sign_in(app: &mut App) {
    let Some(w) = workos() else {
        dialog::error("Horadric could not sign in", "cannot find the state folder");
        return;
    };
    if app.kin.signing_in {
        return;
    }
    app.kin.signing_in = true;
    app.menus();
    std::thread::Builder::new()
        .name("horadric-signin".into())
        .spawn(move || {
            let done = w
                .device(|shown| tell_app(Note::Device(shown)))
                .map(|u| u.email);
            tell_app(Note::SignedIn(done));
        })
        .ok();
}

/// Why a brother may not read a project's files.
const NOT_SHOWN: &str = "That project is not on the Mac's tiles.";

/// The folder of a tile project of the Mac's, as `projects` spelled it:
/// a brother reads the files of those and of nothing else.
fn project_dir(app: &App, project: &str) -> Option<PathBuf> {
    app.clusters
        .iter()
        .filter(|c| horadric_core::afar::split_key(&c.key).is_none() && !crate::town::is(&c.key))
        .filter_map(|c| app.project_dir(&c.key))
        .find(|dir| dir.to_string_lossy() == project)
}

/// The Mac's sessions as the server sees them.
struct Here<'a>(&'a mut App);

impl Serve for Here<'_> {
    fn name(&self) -> String {
        mac_name()
    }

    fn sessions(&self) -> Value {
        let list: Vec<Value> = self
            .0
            .registry
            .lock()
            .map(|r| {
                r.all()
                    .filter(|s| portal::served(s))
                    .map(|s| {
                        let paused = self.0.paused.contains_key(&s.id);
                        portal::session_entry(s, &project_name(&project_key(s)), paused)
                    })
                    .collect()
            })
            .unwrap_or_default();
        json!({"t": "sessions", "list": list})
    }

    fn projects(&self) -> Value {
        let projects: Vec<(String, String)> = self
            .0
            .clusters
            .iter()
            .filter(|c| horadric_core::afar::split_key(&c.key).is_none())
            .filter_map(|c| {
                let dir = self.0.project_dir(&c.key)?;
                Some((dir.to_string_lossy().into_owned(), project_name(&c.key)))
            })
            .collect();
        portal::projects_message(&projects)
    }

    /// No folders beyond the tiles' projects are offered: a brother does
    /// not get to pick any folder on the Mac.
    fn folders(&self) -> Value {
        json!({"t": "folders", "roots": []})
    }

    fn waiting(&self) -> HashSet<String> {
        self.0
            .registry
            .lock()
            .map(|r| {
                r.all()
                    .filter(|s| portal::served(s))
                    .filter(|s| matches!(s.phase, Phase::Waiting(_)))
                    .map(|s| s.id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Starts a session in one of the tiles' projects, and only those.
    fn start(&mut self, project: &str) -> Result<String, String> {
        let key = folder_key(project);
        if !self.0.clusters.iter().any(|c| c.key == key) {
            return Err(format!("{project} is not one of the Mac's projects."));
        }
        let dir = self
            .0
            .project_dir(&key)
            .ok_or_else(|| format!("{project} is not there now."))?;
        self.0.start_afar(dir)
    }

    fn resume(&mut self, id: &str) -> Result<(), String> {
        self.0.resume(id, false)
    }

    fn create(&mut self, _root: &str, _name: &str) -> Result<(String, String), String> {
        Err("New projects cannot be made on the Mac from afar yet.".to_string())
    }

    fn running(&self, id: &str) -> Running {
        if let Some(c) = self.0.consoles.get(id) {
            Running::Here(Arc::as_ptr(c) as usize as u64)
        } else if self.0.paused.contains_key(id) {
            Running::Paused
        } else {
            Running::Elsewhere
        }
    }

    fn input(&mut self, id: &str, data: &str) {
        if let Some(c) = self.0.consoles.get(id) {
            c.note_typed_from_afar();
            c.write(data.as_bytes().to_vec());
        }
    }

    fn screen(&self, id: &str) -> Option<Value> {
        let console = self.0.consoles.get(id)?;
        let s = console.screen.lock().ok()?;
        let screen = portal::screen_text(&s.term);
        drop(s);
        let size = console.size();
        Some(portal::screen_message(
            id,
            size.cols as usize,
            size.rows as usize,
            &screen,
        ))
    }

    fn size(&self, id: &str) -> Option<(u16, u16)> {
        let size = self.0.consoles.get(id)?.size();
        Some((size.cols, size.rows))
    }

    fn output_since(&self, id: &str, at: u64) -> Option<(Vec<u8>, u64)> {
        self.0.consoles.get(id)?.raw_since(at)
    }

    fn output_replay(&self, id: &str) -> (u64, Vec<u8>) {
        self.0
            .consoles
            .get(id)
            .map_or((0, Vec::new()), |c| c.raw_replay())
    }

    fn typed_here(&self, id: &str) -> Option<u64> {
        self.0.consoles.get(id)?.typed_here_at().map(micros)
    }

    fn hold_size(&mut self, id: &str, size: Option<(u16, u16)>) {
        let Some(c) = self.0.consoles.get(id) else {
            return;
        };
        match size {
            Some((cols, rows)) => c.hold_size(GridSize { cols, rows }),
            None => c.release_size(),
        }
    }

    /// A brother may act when it is joined and the human did not switch
    /// it to watching only.
    fn control(&self, who: &str, _kind: Kind) -> bool {
        let Some(dir) = dir() else {
            return false;
        };
        let joined = link().is_some_and(|l| l.store().get(who).is_some());
        joined && Standing::load(&dir).controls(who)
    }

    fn roster(&self) -> Vec<Value> {
        let (Some(dir), Some(link)) = (dir(), link()) else {
            return Vec::new();
        };
        let Some(me) = relay::me(&dir) else {
            return Vec::new();
        };
        let sees: Vec<String> = link
            .with(|b| b.linked().into_iter().map(|(_, id)| id).collect())
            .unwrap_or_default();
        rostering::served(&dir, &me, &mac_name(), &LinkCrypto, &sees)
            .map(|r| rostering::messages(&r))
            .unwrap_or_default()
    }

    fn state(&self) -> &'static str {
        "awake"
    }

    fn rename(&mut self, id: &str, name: &str) -> Result<(), String> {
        {
            let mut r = self.0.registry.lock().map_err(|e| e.to_string())?;
            let s = r
                .get_mut(id)
                .filter(|s| portal::served(s))
                .ok_or("That session is gone.")?;
            s.rename(Some(name.trim()).filter(|n| !n.is_empty()));
        }
        self.0.reconcile();
        Ok(())
    }

    fn changes(&self, id: &str, path: Option<&str>) -> Job {
        let session = self.0.registry.lock().ok().and_then(|r| {
            r.get(id)
                .filter(|s| portal::served(s))
                .map(|s| (PathBuf::from(&s.cwd), s.worktree.clone()))
        });
        let (id, path) = (id.to_string(), path.map(str::to_string));
        Box::new(move || match session {
            Some((cwd, tree)) => portal::changes_reply(&id, &cwd, tree.as_ref(), path.as_deref()),
            None => json!({"t": "error", "text": "That session is not on the Mac."}),
        })
    }

    fn tree(&self, project: &str) -> Job {
        let dir = project_dir(&*self.0, project);
        let project = project.to_string();
        Box::new(move || match dir {
            Some(dir) => horadric_core::afar_files::tree(&project, &dir),
            None => afar_files_error(&project, "", NOT_SHOWN),
        })
    }

    fn read(&self, project: &str, path: &str) -> Job {
        let dir = project_dir(&*self.0, project);
        let (project, path) = (project.to_string(), path.to_string());
        Box::new(move || match dir {
            Some(dir) => horadric_core::afar_files::read(&project, &dir, &path),
            None => afar_files_error(&project, &path, NOT_SHOWN),
        })
    }

    fn stop(&mut self, id: &str) -> Result<(), String> {
        let known = self.0.consoles.contains_key(id)
            || self.0.paused.contains_key(id)
            || self.0.registry.lock().is_ok_and(|r| r.get(id).is_some());
        if !known {
            return Err("That session is gone.".to_string());
        }
        self.0.end(id);
        Ok(())
    }
}
