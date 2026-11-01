//! The Mac as a client of its brothers: their sessions as tiles in
//! clusters of their own, and their panes on the stage, drawn from the raw
//! output each brother streams (protocol v3, `docs/PORTAL.md`), as
//! `crates/horadric-ui/src/far.rs` does on Windows. What the messages mean
//! is `horadric_core::afar`, the registry kept to a brother's list is
//! `horadric_core::afar_tiles`; this is the glue.
//!
//! A brother is reached only over its brother channel on the relay link
//! (`brothers.rs`), which carries both what it asks of the Mac and its
//! answers to what the Mac asks of it. Its rosters are taken in by
//! `horadric_core::brothers` before anything reaches here.
//!
//! A brother's sessions are in the registry, so they light and count as
//! the Mac's own do, but their panes are never in the app's `consoles`:
//! nothing may stop, save or resume them as if they ran on the Mac.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::SystemTime;

use horadric_core::afar::{self, Client, Fit, Heard, Streams};
use horadric_core::afar_tiles;
use horadric_core::brotherhood::{self, Said};
use horadric_core::Phase;
use serde_json::{json, Value};

use crate::console::{Console, FarAsk, GridSize, Note as ConsoleNote};
use crate::project::project_key;

use super::app::{self, App, Input};
use super::brothers::{self, Note};
use super::{dialog, queue};

/// Where the serials of brothers' panes start, so they never meet the
/// app's own, which count up from 1.
const SERIAL_BASE: usize = 1 << 40;

/// One brother, kept while its link is down so its tiles stay.
struct Peer {
    /// What it is called, as shown. Its relay id is what it is kept by.
    name: String,
    /// Its brother channel, while linked.
    ch: Option<u32>,
    client: Client,
    streams: Streams,
    /// Its projects, (path, name), for starting one there.
    projects: Vec<(String, String)>,
    control: bool,
    heard: brotherhood::Heard,
    /// Its sessions as last listed, so tiles dimmed while it slept get
    /// their own lines back when it wakes without relinking.
    list: Vec<afar::Entry>,
}

#[derive(Default)]
pub(super) struct Far {
    /// The brothers by relay id, never by name: two may share one.
    peers: HashMap<String, Peer>,
    /// Which brother each channel is to.
    by_ch: HashMap<u32, String>,
    /// Panes of brothers' sessions, by id here.
    consoles: HashMap<String, Arc<Console>>,
    next_serial: usize,
    /// Sessions to bring onto the stage once they run: started or resumed
    /// from here.
    showing: HashSet<String>,
    /// Tiles that went a moment ago, for a handed off session to come back
    /// into where it stood.
    places: afar_tiles::Places,
}

impl Far {
    pub(super) fn console(&self, id: &str) -> Option<&Arc<Console>> {
        self.consoles.get(id)
    }

    pub(super) fn console_by_serial(&self, serial: usize) -> Option<&Arc<Console>> {
        self.consoles.values().find(|c| c.serial == serial)
    }
}

fn far(app: &mut App) -> &mut Far {
    &mut app.kin.far
}

/// Seconds local time is ahead of UTC now, for the time a silent
/// brother's tiles say they last heard it.
pub(super) fn utc_offset(now: u64) -> i64 {
    let t = now as libc::time_t;
    // SAFETY: `tm` is plain data that localtime_r fills in.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return 0;
    }
    tm.tm_gmtoff as i64
}

/// Sends `msg` to brother `id`, stamped as every message after hello is.
fn send(app: &mut App, id: &str, msg: Value) {
    let Some(p) = far(app).peers.get_mut(id) else {
        return;
    };
    let Some(ch) = p.ch else {
        return;
    };
    let msg = p.client.stamp(msg);
    brothers::reply(ch, &msg);
}

/// Brother `id` linked on channel `ch`: the Mac says hello to it as its
/// client, beside serving what it asks.
pub(super) fn up(app: &mut App, ch: u32, id: &str, name: &str) {
    let name = if name.is_empty() { "brother" } else { name };
    crate::project::name_brother(id, name);
    let f = far(app);
    f.by_ch.insert(ch, id.to_string());
    let p = f.peers.entry(id.to_string()).or_insert_with(|| Peer {
        name: name.to_string(),
        ch: None,
        client: Client::default(),
        streams: Streams::default(),
        projects: Vec::new(),
        control: false,
        heard: brotherhood::Heard {
            said: Said::Awake,
            last: app::unix_now(),
            linked: false,
        },
        list: Vec::new(),
    });
    p.name = name.to_string();
    p.ch = Some(ch);
    p.client = Client::default();
    p.streams.clear();
    let hello = p.client.hello();
    brothers::reply(ch, &hello);
}

/// Channel `ch` went: its brother's tiles stay, dimmed, until it links
/// again.
pub(super) fn down(app: &mut App, ch: u32) {
    let Some(id) = far(app).by_ch.remove(&ch) else {
        return;
    };
    let Some(p) = far(app).peers.get_mut(&id) else {
        return;
    };
    if p.ch != Some(ch) {
        return;
    }
    p.ch = None;
    p.heard.linked = false;
    p.client = Client::default();
    p.streams.clear();
    if lines(app) {
        app.reconcile();
    }
}

/// Brother `id` was cast out: its tiles and panes go, and nothing of it
/// is shown again.
pub(super) fn gone(app: &mut App, id: &str) {
    let f = far(app);
    f.peers.remove(id);
    f.by_ch.retain(|_, b| b != id);
    let gone = app
        .registry
        .lock()
        .map(|mut r| afar_tiles::drop_brother(&mut r, id))
        .unwrap_or_default();
    for local in gone {
        far(app).consoles.remove(&local);
    }
    crate::project::unname_brother(id);
    app.reconcile();
}

/// Sets each brother's tiles to how it is reached now. True when any
/// changed.
fn lines(app: &mut App) -> bool {
    let now = app::unix_now();
    let offset = utc_offset(now);
    let lines: HashMap<String, Option<String>> = far(app)
        .peers
        .iter_mut()
        .map(|(id, p)| {
            if p.heard.linked {
                p.heard.last = now;
            }
            let line = brotherhood::tile_line(&p.name, p.heard.presence(now), offset);
            (id.clone(), line)
        })
        .collect();
    app.registry
        .lock()
        .map(|mut r| afar_tiles::set_reach(&mut r, &lines))
        .unwrap_or(false)
}

/// From the app's tick: a brother that went quiet dims its tiles with
/// when it was last heard.
pub(super) fn tick(app: &mut App) {
    if !far(app).peers.is_empty() && lines(app) {
        app.reconcile();
    }
}

/// An answer brother on channel `ch` sent to what the Mac asked of it.
pub(super) fn heard(app: &mut App, ch: u32, msg: &Value) {
    let Some(id) = far(app).by_ch.get(&ch).cloned() else {
        return;
    };
    if let Some(p) = far(app).peers.get_mut(&id) {
        p.heard.last = app::unix_now();
    }
    if super::handing::heard(app, &id, msg) {
        return;
    }
    let changed = match afar::hear(msg) {
        Heard::Hello { control, v, .. } => hello(app, &id, msg, control, v),
        Heard::Sessions(list) => {
            if let Some(p) = far(app).peers.get_mut(&id) {
                p.list = list.clone();
            }
            sessions(app, &id, &list)
        }
        Heard::Projects(list) => {
            if let Some(p) = far(app).peers.get_mut(&id) {
                p.projects = list;
            }
            false
        }
        Heard::Out {
            id: remote,
            seq,
            bytes,
            reset,
        } => {
            out(app, &id, &remote, seq, &bytes, reset);
            false
        }
        Heard::Size {
            id: remote,
            cols,
            rows,
        } => {
            let local = afar::local_id(&id, &remote);
            if let Some(c) = far(app).consoles.get(&local).cloned() {
                c.far_size(GridSize { cols, rows });
                drawn(c.serial);
            }
            false
        }
        Heard::Started { id: remote, .. } => {
            far(app).showing.insert(afar::local_id(&id, &remote));
            false
        }
        Heard::Error { id: of, text } => {
            let name = crate::project::brother_name(&id);
            eprintln!(
                "horadric: {name} says {text}{}",
                of.map(|i| format!(" of {i}")).unwrap_or_default()
            );
            false
        }
        Heard::State(said) => {
            let Some(p) = far(app).peers.get_mut(&id) else {
                return;
            };
            p.heard.said = said;
            let list = p.list.clone();
            let listed = sessions(app, &id, &list);
            lines(app) || listed
        }
        // A roster may say a session another lists is not its own any
        // more: every brother's tiles are looked at again.
        Heard::Other if msg["t"] == "roster" => {
            let lists: Vec<(String, Vec<afar::Entry>)> = far(app)
                .peers
                .iter()
                .map(|(k, p)| (k.clone(), p.list.clone()))
                .collect();
            lists
                .into_iter()
                .fold(false, |changed, (k, l)| sessions(app, &k, &l) || changed)
        }
        Heard::Usage(told) => {
            usage(app, told);
            false
        }
        // The Mac keeps no lore yet.
        _ => false,
    };
    if changed {
        app.reconcile();
    }
    reveal_started(app);
}

/// Takes in the limits a brother told: a reading of the account Claude
/// Code uses on the Mac (or of any while none is known) merges into what
/// the usage window shows, since a limit is the account's and work on a
/// brother runs it down too. Another account's is not the Mac's to show.
fn usage(app: &mut App, told: Vec<horadric_core::usage::Told>) {
    let here = super::usage::account();
    let now = app::unix_now();
    for t in told {
        if t.agent != horadric_core::Agent::Claude {
            continue;
        }
        let mine = match (&here, &t.email) {
            (None, _) => true,
            (Some(a), Some(e)) => a == e || a.starts_with(&format!("{e}, ")),
            (Some(_), None) => false,
        };
        if !mine {
            continue;
        }
        if let Ok(mut u) = app.usage.lock() {
            *u = Some(
                u.as_ref()
                    .map_or(t.usage.clone(), |h| h.merge(&t.usage, now)),
            );
        }
    }
}

/// The answer to hello: the brother serves this link, and panes open
/// before it dropped take their streams again.
fn hello(app: &mut App, id: &str, msg: &Value, control: bool, v: u64) -> bool {
    if v < afar::VERSION {
        eprintln!(
            "horadric: {} needs a newer Horadric to be a brother",
            crate::project::brother_name(id)
        );
        return false;
    }
    let Some(p) = far(app).peers.get_mut(id) else {
        return false;
    };
    p.client.heard_hello(msg);
    p.control = control;
    p.heard.linked = true;
    let attach: Vec<Value> = far(app)
        .consoles
        .values()
        .filter_map(|c| {
            let remote = afar::remote_id(&c.id, id)?;
            let size = c.far_asked().unwrap_or_else(|| c.size());
            Some(json!({"t": "attach", "id": remote, "cols": size.cols, "rows": size.rows}))
        })
        .collect();
    for a in attach {
        send(app, id, a);
    }
    lines(app)
}

/// Keeps the registry to what brother `id` lists, what the roster says
/// another owns now left out. Panes of sessions gone or stopped there go.
fn sessions(app: &mut App, id: &str, list: &[afar::Entry]) -> bool {
    let (roster, held) = horadric_hooks::state_dir()
        .map(|d| {
            let held = horadric_core::joining::Store::load(&d).is_some_and(|s| s.get(id).is_some());
            (horadric_core::mesh::load(&d), held)
        })
        .unwrap_or_default();
    let list = afar::shown_list(list, id, &roster, held);
    let name = far(app)
        .peers
        .get(id)
        .map_or_else(|| crate::project::brother_name(id), |p| p.name.clone());
    let now = SystemTime::now();
    let places = &mut app.kin.far.places;
    let (changed, mut gone) = app
        .registry
        .lock()
        .map(|mut r| afar_tiles::hold_placed(&mut r, id, &name, &list, now, places, project_key))
        .unwrap_or_default();
    // A pane whose session stopped running there has nothing more to show
    // until it runs again.
    gone.extend(
        list.iter()
            .filter(|e| e.paused)
            .map(|e| afar::local_id(id, &e.id)),
    );
    let mut panes = false;
    for local in gone {
        if far(app).consoles.remove(&local).is_some() {
            if let (Some(p), Some(remote)) =
                (far(app).peers.get_mut(id), afar::remote_id(&local, id))
            {
                p.streams.forget(remote);
            }
            panes = true;
        }
    }
    changed || panes
}

/// A brother's output for one of its sessions, drawn in its pane here.
fn out(app: &mut App, id: &str, remote: &str, seq: u64, bytes: &[u8], reset: bool) {
    let local = afar::local_id(id, remote);
    let Some(c) = far(app).consoles.get(&local).cloned() else {
        return;
    };
    let fit = match far(app).peers.get_mut(id) {
        Some(p) => p.streams.fit(remote, seq, bytes.len(), reset),
        None => return,
    };
    match fit {
        Fit::Reset => c.far_output(bytes, true),
        Fit::Next => c.far_output(bytes, false),
        Fit::Old => {}
        Fit::Gap => {
            let size = c.far_asked().unwrap_or_else(|| c.size());
            let again = json!({"t": "attach", "id": remote, "cols": size.cols, "rows": size.rows});
            send(app, id, again);
        }
    }
    drawn(c.serial);
}

/// The pane with this serial has something new to draw.
fn drawn(serial: usize) {
    app::input(Input::Console(ConsoleNote::Output, serial));
}

/// Sessions started or resumed from here come onto the stage once they
/// run there.
fn reveal_started(app: &mut App) {
    if far(app).showing.is_empty() {
        return;
    }
    let showing: Vec<String> = far(app).showing.iter().cloned().collect();
    let running: Vec<(String, String)> = {
        let Ok(r) = app.registry.lock() else {
            return;
        };
        showing
            .into_iter()
            .filter_map(|id| {
                let s = r.get(&id).filter(|s| s.phase != Phase::Paused)?;
                Some((id, project_key(s)))
            })
            .collect()
    };
    for (id, key) in running {
        far(app).showing.remove(&id);
        if reveal(app, &id) {
            app.show_project(&key, Some(&id));
        }
    }
}

/// The brother and its project of session `id` here, if it is a
/// brother's.
fn brother_of(app: &App, id: &str) -> Option<(String, String, bool)> {
    let r = app.registry.lock().ok()?;
    let s = r.get(id)?;
    let b = s.brother.as_ref()?;
    Some((b.id.clone(), project_key(s), s.phase == Phase::Paused))
}

/// A click on a brother's tile: its pane is made and its stream asked for,
/// so the stage shows it; a paused one is resumed there first. False when
/// the session is not a brother's.
pub(super) fn reveal(app: &mut App, id: &str) -> bool {
    let Some((brother, _, paused)) = brother_of(app, id) else {
        return false;
    };
    let Some(remote) = afar::remote_id(id, &brother).map(str::to_string) else {
        return true;
    };
    if far(app).consoles.contains_key(id) {
        return true;
    }
    let up = far(app)
        .peers
        .get(&brother)
        .is_some_and(|p| p.ch.is_some() && p.client.ready());
    if !up {
        return true;
    }
    if paused {
        far(app).showing.insert(id.to_string());
        send(app, &brother, json!({"t": "resume", "id": remote}));
        return true;
    }
    let f = far(app);
    let serial = SERIAL_BASE + f.next_serial;
    f.next_serial += 1;
    let local = id.to_string();
    let console = Console::far(
        id.to_string(),
        serial,
        Box::new(move |ask| {
            let local = local.clone();
            queue::post(move || app::input(Input::Kin(Note::FarAsk(local, ask))));
        }),
    );
    let size = console.size();
    f.consoles.insert(id.to_string(), console);
    if let Some(p) = f.peers.get_mut(&brother) {
        p.streams.forget(&remote);
    }
    send(
        app,
        &brother,
        json!({"t": "attach", "id": remote, "cols": size.cols, "rows": size.rows}),
    );
    true
}

/// What a brother's pane asked: typing or its size, for the brother.
pub(super) fn ask(app: &mut App, id: &str, ask: FarAsk) {
    let Some((brother, _, _)) = brother_of(app, id) else {
        return;
    };
    let Some(remote) = afar::remote_id(id, &brother).map(str::to_string) else {
        return;
    };
    // A brother that lets the Mac only watch gets no typing from it.
    let control = far(app).peers.get(&brother).is_some_and(|p| p.control);
    let msg = match ask {
        FarAsk::Input(_) if !control => return,
        FarAsk::Input(bytes) => {
            json!({"t": "input", "id": remote, "data": String::from_utf8_lossy(&bytes)})
        }
        FarAsk::Resize(size) => {
            json!({"t": "resize", "id": remote, "cols": size.cols, "rows": size.rows})
        }
    };
    send(app, &brother, msg);
}

/// A new session on a brother's project, started there. False when the
/// cluster is not a brother's.
pub(super) fn start(app: &mut App, key: &str) -> bool {
    let Some((brother, project)) = afar::split_key(key) else {
        return false;
    };
    let path = far(app)
        .peers
        .get(brother)
        .and_then(|p| afar::project_path(&p.projects, project))
        .map(str::to_string);
    match path {
        Some(path) => {
            let brother = brother.to_string();
            send(app, &brother, json!({"t": "start", "project": path}));
        }
        None => dialog::error(
            "Horadric could not start a session there",
            &format!(
                "{} lists no one folder for {project}.",
                crate::project::brother_name(brother)
            ),
        ),
    }
    true
}

/// The pane of a brother's session closed here: its stream stops, and
/// the session runs on there. False when the session is not a brother's.
pub(super) fn close(app: &mut App, id: &str) -> bool {
    let Some((brother, _, _)) = brother_of(app, id) else {
        return false;
    };
    if far(app).consoles.remove(id).is_some() {
        if let Some(remote) = afar::remote_id(id, &brother).map(str::to_string) {
            if let Some(p) = far(app).peers.get_mut(&brother) {
                p.streams.forget(&remote);
            }
            send(app, &brother, json!({"t": "detach", "id": remote}));
        }
    }
    app.reconcile();
    true
}

/// Whether session `id` here is a brother's: the Mac never ends one as
/// its own.
pub(super) fn is_brothers(app: &App, id: &str) -> bool {
    brother_of(app, id).is_some()
}

/// Ends a brother's session there, once the human says so. False when the
/// session is not a brother's.
pub(super) fn stop(app: &mut App, id: &str) -> bool {
    let Some((brother, _, _)) = brother_of(app, id) else {
        return false;
    };
    let Some(remote) = afar::remote_id(id, &brother).map(str::to_string) else {
        return true;
    };
    let name = crate::project::brother_name(&brother);
    let ok = dialog::confirm(
        &format!("End this session on {name}?"),
        "Its agent is stopped there.",
        "End session",
    );
    if ok {
        send(app, &brother, json!({"t": "stop", "id": remote}));
    }
    true
}

/// The brothers a session can be handed to now, by relay id: linked,
/// answering, and letting the Mac act. In the order of their names.
pub(super) fn takers(app: &App) -> Vec<String> {
    let mut ids: Vec<(&str, &String)> = app
        .kin
        .far
        .peers
        .iter()
        .filter(|(_, p)| p.ch.is_some() && p.client.ready() && p.control)
        .map(|(id, p)| (p.name.as_str(), id))
        .collect();
    ids.sort();
    ids.into_iter().map(|(_, id)| id.clone()).collect()
}

/// A brother by its relay id or, when only one goes by it, its name.
pub(super) fn id_of(app: &App, to: &str) -> Option<String> {
    let peers = &app.kin.far.peers;
    if peers.contains_key(to) {
        return Some(to.to_string());
    }
    let mut named = peers.iter().filter(|(_, p)| p.name == to);
    let (id, _) = named.next()?;
    named.next().is_none().then(|| id.clone())
}

/// The brother that owns tile `id`, for a Claude Code session of theirs
/// with a conversation to carry on.
pub(super) fn owner_of(app: &App, id: &str) -> Option<String> {
    let r = app.registry.lock().ok()?;
    let s = r.get(id)?;
    let b = s.brother.as_ref()?;
    (!s.shell && s.prompted).then(|| b.id.clone())
}

/// Sends `msg` to brother `id` on its channel, while linked.
pub(super) fn tell(app: &mut App, id: &str, msg: Value) {
    send(app, id, msg);
}

/// The Mac's own session `id` goes because it was handed to a brother:
/// its tile's place is kept for it to come back as that brother's.
pub(super) fn left(app: &mut App, id: &str) {
    let now = SystemTime::now();
    let places = &mut app.kin.far.places;
    if let Ok(mut r) = app.registry.lock() {
        if let Some(home) = r.get(id).map(project_key) {
            places.leave(&mut r, id, &home, now);
        }
    }
}

/// Names a brother's session there; the name comes back in its list.
/// False when the session is not a brother's.
pub(super) fn rename(app: &mut App, id: &str, name: &str) -> bool {
    let Some((brother, _, _)) = brother_of(app, id) else {
        return false;
    };
    if let Some(remote) = afar::remote_id(id, &brother).map(str::to_string) {
        send(
            app,
            &brother,
            json!({"t": "rename", "id": remote, "name": name}),
        );
    }
    true
}
