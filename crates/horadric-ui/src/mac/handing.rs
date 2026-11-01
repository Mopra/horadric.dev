//! The Mac's side of handing a session to a brother and taking one in, as
//! Windows' `handing.rs` and the node's `node_handing.rs` do: what the
//! three share is `horadric_core::handoffs` (the handoffs under way, the
//! place a project goes, landing) and `horadric_core::sworn` (the signed
//! word on owners, by docs/plan/brotherhood-ownership.md); this is the
//! glue to the Mac's tiles, consoles and alerts.
//!
//! As sender it waits for the turn to end (or stops it with Esc for
//! "Now"), stops the agent, asks the receiver for its commits, packs the
//! session on a thread, sends it in parts on the brother channel, and lets
//! go once the receiver says `took`: the tile goes, its place kept for the
//! session to come back into as the receiver's (`far::left`). As receiver
//! it joins the parts, asks its human where a project it has no checkout
//! of goes, lands the session on a thread, and adopts it on the main
//! thread, its next prompt told it moved.
//!
//! Everything but packing, landing and cloning runs on the main thread;
//! threads come back through [`Note::Run`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

use horadric_core::agent::Agent;
use horadric_core::handing::{self, bases_answer, refused, Do, Outgoing, PlaceAsk};
use horadric_core::handoff::{Record, RepoId};
use horadric_core::handoffs::{
    self, arrive, model_of, record_of, Arrived, Handoffs, Packed, Place, Places, Said,
};
use horadric_core::placing;
use horadric_core::serving::Job;
use horadric_core::sworn::Sworn;
use horadric_core::SavedSession;
use horadric_hooks::listener::RecallCall;
use horadric_link::relay::LinkCrypto;
use serde_json::{json, Value};

use super::app::{self, App, Input};
use super::brothers::{self, Note};
use super::menu::Item;
use super::{dialog, far, queue};
use crate::project::brother_name;

/// Where a tile menu's handoff ids start: "Now", then a brother each.
pub(super) const HANDOFF: usize = 2000;
/// Where a brother's tile's handoff ids start: a brother, or this Mac.
pub(super) const HANDOFF_FAR: usize = 2200;
/// How long a landed session gets to start on the main thread before its
/// sender is told it did not.
const ADOPT_WAIT: Duration = Duration::from_secs(60);

#[derive(Default)]
struct State {
    hand: Handoffs,
    /// The line each adopted session's next prompt starts with, by id.
    first: HashMap<String, String>,
    /// Brothers' sessions this Mac asked their owner to hand on, by the
    /// tile's id, to say why when the owner refuses.
    asked: HashMap<String, String>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

/// Each arriving session's place: threads clone and fill it.
static PLACING: Places = Places::new();

/// The Mac's word on whose its sessions are, None before it has a relay id.
pub(super) fn sworn() -> Option<Sworn<'static>> {
    let dir = horadric_hooks::state_dir()?;
    let me = horadric_link::relay::me(&dir)?;
    Sworn::new(dir, me, &LinkCrypto)
}

/// Runs `f` with the app on the main thread, from a thread.
fn on_main(f: impl FnOnce(&mut App) + Send + 'static) {
    queue::post(move || app::input(Input::Kin(Note::Run(Box::new(f)))));
}

/// Why session `id` cannot be handed off, if it cannot: only a Claude Code
/// session of the Mac with a conversation to resume, running in a pane or
/// paused, can.
fn handable(app: &App, id: &str) -> Result<(), String> {
    let r = app.registry.lock().map_err(|_| "busy")?;
    let s = r.get(id).ok_or("no such session")?;
    if s.brother.is_some() {
        return Err("it runs on a brother".into());
    }
    if s.shell || s.ssh.is_some() || s.background.is_some() {
        return Err("only a Claude Code session can be handed off".into());
    }
    if !s.prompted || s.claude_session_id.is_none() {
        return Err("it has no conversation to resume yet".into());
    }
    if !app.consoles.contains_key(id) && !app.paused.contains_key(id) {
        return Err("it does not run in a pane of this Mac".into());
    }
    Ok(())
}

/// The tile menu's handoff lines for session `id`: where it is going and
/// "Now", the brothers it can go to, or for a brother's tile, the brothers
/// (and this Mac) its owner can hand it to.
pub(super) fn menu(app: &App, id: &str) -> Vec<Item> {
    if let Some(o) = with(|s| s.hand.going(id).cloned()) {
        let mut items = vec![Item::disabled(shown_line(&o))];
        if !o.now && !o.stopped() {
            items.push(Item::pick("Now", HANDOFF));
        }
        return vec![Item::Sub(
            format!("Hand Off to {}", brother_name(&o.to)),
            items,
        )];
    }
    let takers = far::takers(app);
    if let Some(owner) = far::owner_of(app, id) {
        if !takers.contains(&owner) {
            return Vec::new();
        }
        let items = far_targets(&owner, &takers)
            .into_iter()
            .take(99)
            .enumerate()
            .map(|(i, (_, label))| Item::pick(label, HANDOFF_FAR + i))
            .collect();
        return vec![Item::Sub("Hand Off to".into(), items)];
    }
    if handable(app, id).is_err() || takers.is_empty() {
        return Vec::new();
    }
    let items = takers
        .iter()
        .take(99)
        .enumerate()
        .map(|(i, b)| Item::pick(brother_name(b), HANDOFF + 1 + i))
        .collect();
    vec![Item::Sub("Hand Off to".into(), items)]
}

/// Who a brother's session can be handed to, asked of its `owner`: the
/// other brothers linked here, by relay id, and this Mac.
fn far_targets(owner: &str, takers: &[String]) -> Vec<(String, String)> {
    let me = brothers::me_id().unwrap_or_default();
    let mut out: Vec<(String, String)> = takers
        .iter()
        .filter(|t| *t != owner && **t != me)
        .map(|t| (t.clone(), brother_name(t)))
        .collect();
    if !me.is_empty() {
        out.push((me, format!("This Mac ({})", brothers::mac_name())));
    }
    out
}

/// Acts on a pick from [`menu`]; false when it was not one of its tags.
pub(super) fn picked(app: &mut App, id: &str, tag: usize) -> bool {
    if let Some(i) = tag.checked_sub(HANDOFF_FAR).filter(|i| *i < 99) {
        let Some(owner) = far::owner_of(app, id) else {
            return true;
        };
        if let Some((to, _)) = far_targets(&owner, &far::takers(app)).get(i) {
            let Some(remote) = horadric_core::afar::remote_id(id, &owner).map(str::to_string)
            else {
                return true;
            };
            with(|s| s.asked.insert(id.to_string(), to.clone()));
            far::tell(app, &owner, json!({"t": "handoff", "id": remote, "to": to}));
            dialog::notify(
                "Handing off",
                &format!(
                    "Asked {} to hand it to {}.",
                    brother_name(&owner),
                    brother_name(to)
                ),
            );
        }
        return true;
    }
    if tag == HANDOFF {
        with(|s| s.hand.hurry(id));
        tick(app);
        return true;
    }
    let Some(i) = tag.checked_sub(HANDOFF + 1).filter(|i| *i < 99) else {
        return false;
    };
    if let Some(to) = far::takers(app).get(i).cloned() {
        if let Err(e) = handoff_to(app, id, &to, false) {
            dialog::error("Horadric could not hand it off", &e);
        }
    }
    true
}

/// Hands session `id` to the brother `to` (its relay id or its name) once
/// its turn ends, or at once with `now`. What a brother's `handoff` asks
/// too.
pub(super) fn handoff_to(app: &mut App, id: &str, to: &str, now: bool) -> Result<(), String> {
    handable(app, id)?;
    let to = far::id_of(app, to).ok_or_else(|| format!("{to} is not a brother of this Mac"))?;
    if !far::takers(app).contains(&to) {
        return Err(format!(
            "{} is not a brother this Mac can hand sessions to",
            brother_name(&to)
        ));
    }
    with(|s| s.hand.start(id, &to, now));
    tick(app);
    Ok(())
}

/// A handoff's line as shown: the brother by its name, not its id.
fn shown_line(o: &Outgoing) -> String {
    o.line().replace(&o.to, &brother_name(&o.to))
}

fn set_line(app: &mut App, id: &str, line: &str) {
    let changed = app.registry.lock().is_ok_and(|mut r| {
        r.get_mut(id).is_some_and(|s| {
            let changed = s.last_line != line;
            s.last_line = line.to_string();
            changed
        })
    });
    if let Some(p) = app.paused.get_mut(id) {
        p.last_line = line.to_string();
    }
    if changed {
        app.reconcile();
    }
}

/// Moves each handoff on a step. From the app's tick, and whenever an
/// answer or a thread moves one.
pub(super) fn tick(app: &mut App) {
    PLACING.expire(app::unix_now());
    let takers = far::takers(app);
    let now = app::unix_now();
    for id in with(|s| s.hand.ids()) {
        let phase = app
            .registry
            .lock()
            .ok()
            .and_then(|r| r.get(&id).map(|s| s.phase.clone()));
        let linked = with(|s| s.hand.to(&id)).is_some_and(|to| takers.contains(&to));
        let Some((o, step)) = with(|s| s.hand.tick(&id, phase.as_ref(), linked, now)) else {
            continue;
        };
        set_line(app, &id, &shown_line(&o));
        act(app, &id, &o, step);
    }
}

fn act(app: &mut App, id: &str, o: &Outgoing, step: Do) {
    match step {
        Do::Wait => {}
        // Esc stops a turn in Claude Code, as the human would.
        Do::Interrupt => {
            if let Some(c) = app.consoles.get(id) {
                c.write(b"\x1b".to_vec());
            }
        }
        Do::AskBases => {
            // The agent stops first, so nothing changes the tree or the
            // transcript while they travel. Killed, its exit pauses it.
            if let Some(c) = app.consoles.get(id) {
                if c.exit_code().is_none() {
                    c.kill();
                }
            }
            let me = brothers::mac_name();
            let session = app.registry.lock().ok().and_then(|r| r.get(id).cloned());
            let ask = with(|s| {
                s.hand.ask(id, || match &session {
                    Some(s) => handoffs::bases_ask_of(s, &me),
                    None => handing::bases_ask(id, &RepoId::default()),
                })
            });
            far::tell(app, &o.to, ask);
        }
        Do::Pack { known, clean } => pack(app, id, &o.to, known, clean),
        Do::LetGo(n) => let_go(app, id, &o.to, n),
        Do::Fail(why) => {
            with(|s| s.hand.drop(id));
            if let Some(w) = sworn() {
                w.recall(id, app::unix_now());
            }
            set_line(app, id, &format!("not handed off: {why}"));
            dialog::notify("Handoff failed", &why);
        }
    }
}

/// Packs session `id` on a thread for its receiver `to`, which has
/// `known`. The record carries the Mac's signed word that it hands the
/// session to `to`, so `to` owns it on that word as soon as it runs.
fn pack(app: &mut App, id: &str, to: &str, known: Vec<String>, clean: bool) {
    let args = app
        .consoles
        .get(id)
        .map(|c| c.args.clone())
        .or_else(|| app.paused.get(id).map(|p| p.args.clone()))
        .unwrap_or_default();
    let now = app::unix_now();
    let w = sworn();
    let n = w.as_ref().map_or(1, |w| w.number(id, now));
    let said = w.map(|w| w.handing(id, to, n, now)).unwrap_or_default();
    let record = app.registry.lock().ok().and_then(|r| {
        Some(Record {
            model: model_of(&args),
            interrupted: !clean,
            n,
            said,
            ..record_of(r.get(id)?, &brothers::mac_name())?
        })
    });
    let id = id.to_string();
    std::thread::spawn(move || {
        let packed = match (record, horadric_hooks::transcript::projects()) {
            (Some(mut record), Some(projects)) => {
                let cwd = PathBuf::from(&record.cwd);
                handing::pack_session(&mut record, &cwd, &projects, &known)
            }
            (None, _) => Err("the session has gone".into()),
            (_, None) => Err("Claude Code's folder was not found".into()),
        };
        on_main(move |app| packed_back(app, &id, packed));
    });
}

fn packed_back(app: &mut App, id: &str, packed: Result<Vec<u8>, String>) {
    match with(|s| s.hand.packed(id, packed, app::unix_now())) {
        Packed::Gone => {}
        Packed::Send(to, parts) => {
            for msg in parts {
                far::tell(app, &to, msg);
            }
        }
        Packed::Fail(o, step) => act(app, id, &o, step),
    }
}

/// The receiver runs the session: the tile goes, its place kept for the
/// session to come back as the receiver's, and its worktree with it,
/// since the work is on the receiver now. What it changed in a folder of
/// the human's own stays there.
fn let_go(app: &mut App, id: &str, to: &str, n: u64) {
    with(|s| s.hand.let_go(id));
    if let Some(w) = sworn() {
        w.hand(id, to, n);
    }
    let (tree, cwd, session) = match app.registry.lock() {
        Ok(mut r) => match r.get_mut(id) {
            Some(s) => (
                s.worktree.take(),
                s.cwd.clone(),
                s.claude_session_id.clone(),
            ),
            None => (None, String::new(), None),
        },
        Err(_) => return,
    };
    if let Some(session) = session {
        handing::forget_ref(Path::new(&cwd), &session);
    }
    far::left(app, id);
    app.forget(id);
    if let Some(w) = tree {
        std::thread::spawn(move || handoffs::discard(&w));
    }
    app.reconcile();
    dialog::notify(
        "Handed off",
        &format!("It runs on {} now.", brother_name(to)),
    );
}

/// Hears what brother `id` answered about a handoff from here, or about
/// one of its sessions this Mac asked it to hand on. False for anything
/// else.
pub(super) fn heard(app: &mut App, id: &str, msg: &Value) -> bool {
    if !handoffs::is_answer(msg) {
        return false;
    }
    let name = brother_name(id);
    if msg["t"] == "error" && msg["for"] == "handoff" {
        let remote = msg["id"].as_str().unwrap_or("");
        let tile = horadric_core::afar::local_id(id, remote);
        if let Some(to) = with(|s| s.asked.remove(&tile)) {
            let why = msg["text"].as_str().unwrap_or("no");
            dialog::notify(
                "Not handed off",
                &format!("{name} did not hand it to {}: {why}", brother_name(&to)),
            );
            return true;
        }
    }
    let Some((o, step)) = with(|s| s.hand.answer(id, &name, msg, app::unix_now())) else {
        return msg["t"] != "error";
    };
    act(app, &o.id.clone(), &o, step);
    true
}

/// This Mac's checkout of repository `repo`: a recent project or a
/// session's folder whose repository is the same one.
fn checkout_of(app: &App, repo: &RepoId) -> Option<PathBuf> {
    let mut folders = app.recent.clone();
    if let Ok(r) = app.registry.lock() {
        folders.extend(
            r.all()
                .filter(|s| s.brother.is_none())
                .map(handoffs::project_top),
        );
    }
    handoffs::checkout_among(folders, repo, false)
}

/// A brother's `bases` ask or `bundle` part from `who`: a job answering
/// it, or None while parts are still coming.
pub(super) fn serve(app: &mut App, who: &str, msg: &Value) -> Option<Job> {
    let id = msg["id"].as_str().unwrap_or("").to_string();
    if msg["t"] == "bases" {
        // Handed here already: the sender asks again because its link
        // dropped before it heard `took`.
        let runs = app
            .registry
            .lock()
            .is_ok_and(|r| r.get(&id).is_some_and(|s| s.brother.is_none()));
        if let Some(took) = with(|s| s.hand.took_again(&id, runs)) {
            return Some(Box::new(move || took));
        }
        let repo: RepoId = serde_json::from_value(msg["repo"].clone()).unwrap_or_default();
        let place = handing::heard_place(msg);
        let plain = place.as_ref().is_some_and(|p| p.plain);
        let checkout = if plain { None } else { checkout_of(app, &repo) };
        if let (None, Some(place)) = (&checkout, place) {
            let (job, ask_now) = PLACING.answer(&id, place.plain, app::unix_now());
            if ask_now {
                let who = who.to_string();
                on_main(move |app| ask_place(app, &id, &who, place, repo));
            }
            return Some(job);
        }
        return Some(Box::new(move || {
            bases_answer(
                &id,
                checkout.map(|c| horadric_core::handoff::known_commits(&c)),
            )
        }));
    }
    let package = match with(|s| s.hand.arriving.take(who, msg, app::unix_now())) {
        Ok(None) => return None,
        Ok(Some(p)) => p,
        Err(e) => return Some(Box::new(move || refused(&id, &e))),
    };
    let record = package.record.clone();
    let id = record.id.clone();
    // Never run a handoff its sender took back, or that another brother
    // already runs: landed late, it would fence the one running it.
    let n = record.n.saturating_add(1);
    if let Err(why) = sworn().map_or(Ok(()), |w| w.may_arrive(&id, &record.said, n)) {
        return Some(Box::new(move || refused(&id, &why)));
    }
    let taken = app.registry.lock().is_ok_and(|r| r.get(&id).is_some());
    let placed = PLACING.ready(&id);
    let checkout = if record.plain || placed.is_some() {
        None
    } else {
        checkout_of(app, &record.repo)
    };
    let projects = horadric_hooks::transcript::projects();
    let from = brother_name(who);
    Some(Box::new(move || {
        if taken || id.is_empty() {
            return refused(&id, "a session of that id is here already");
        }
        let arrived = match arrive(&package, placed, checkout, projects) {
            Ok(a) => a,
            Err(e) => return refused(&id, &e),
        };
        let (done, told) = mpsc::channel();
        on_main(move |app| {
            let _ = done.send(adopt(app, &record, arrived, &from));
        });
        match told.recv_timeout(ADOPT_WAIT) {
            Ok(Ok(())) => {
                PLACING.forget(&id);
                handing::took_message(&id, n)
            }
            Ok(Err(e)) => refused(&id, &format!("cannot resume it: {e}")),
            Err(_) => refused(&id, "it did not start in time"),
        }
    }))
}

/// Takes in a handed off session that landed: a tile under its own id
/// where it arrived, resumed with its conversation, its next prompt told
/// it moved from `from`. The roster hears it runs here on its sender's
/// word.
fn adopt(app: &mut App, record: &Record, arrived: Arrived, from: &str) -> Result<(), String> {
    let id = record.id.clone();
    let first = arrived.first_line(record, from, &brothers::mac_name());
    let worktree = match &arrived {
        Arrived::Git(l) => Some(l.worktree.clone()),
        Arrived::Plain(_) => None,
    };
    let cwd = arrived.cwd().to_path_buf();
    // Nobody may be at the Mac to answer Claude Code's trust prompt for a
    // folder it has never seen.
    if let Some(home) = horadric_hooks::home() {
        horadric_core::trust::trust_folder(Path::new(&home), &cwd);
    }
    let saved = SavedSession {
        id: id.clone(),
        name: record.name.clone(),
        title: None,
        renamed: false,
        cwd: cwd.to_string_lossy().into_owned(),
        agent: Agent::Claude,
        args: record
            .model
            .iter()
            .flat_map(|m| ["--model".to_string(), m.clone()])
            .collect(),
        claude_session_id: Some(record.session.clone()),
        prompted: true,
        last_line: format!("moved here from {from}"),
        running: false,
        shell: false,
        ssh: None,
        worktree,
        loot: Default::default(),
        runeword: None,
        chat: false,
    };
    if let Ok(mut r) = app.registry.lock() {
        r.add(saved.to_session(SystemTime::now()));
    }
    app.paused.insert(id.clone(), saved);
    with(|s| s.first.insert(id.clone(), first));
    app.reconcile();
    if let Err(e) = app.resume(&id, false) {
        with(|s| s.first.remove(&id));
        app.forget(&id);
        app.reconcile();
        return Err(e);
    }
    let n = record.n.saturating_add(1);
    with(|s| s.hand.took(&id, n));
    let believed = sworn().is_some_and(|w| w.arrived(&id, n, &record.said, app::unix_now()));
    if !believed {
        eprintln!("horadric: {id} runs here, but its sender's word was not believed");
    }
    dialog::notify(
        "Handed here",
        &format!("{} moved here from {from}.", record.name),
    );
    Ok(())
}

/// Asks the human where the project of session `id` that brother `who`
/// hands here goes: clone it beside the other checkouts, or pick a folder.
/// Then readies it on a thread, which the sender's next ask hears.
fn ask_place(app: &mut App, id: &str, who: &str, ask: PlaceAsk, repo: RepoId) {
    let recent: Vec<String> = app
        .recent
        .iter()
        .filter(|r| Path::new(r).is_dir())
        .cloned()
        .collect();
    let home = horadric_hooks::home();
    let parent = placing::checkouts_parent(&recent, home.as_deref(), |p| Path::new(p).is_dir());
    let q = handoffs::question(&ask, &brother_name(who), "Mac", parent.as_deref(), |f| {
        Path::new(f).exists()
    });
    let start = parent.map(PathBuf::from);
    let pick = |start: Option<&Path>| dialog::pick_place(start).map_or(Said::No, Said::Folder);
    let said = match &q.clone {
        Some((url, dest)) => {
            match dialog::buttons(
                &q.title,
                &q.text,
                &["Clone", "Pick a Folder\u{2026}", "Refuse"],
            ) {
                Some(0) => Said::Clone(url.clone(), PathBuf::from(dest)),
                Some(1) => pick(start.as_deref()),
                _ => Said::No,
            }
        }
        None => match dialog::buttons(&q.title, &q.text, &["Pick a Folder\u{2026}", "Refuse"]) {
            Some(0) => pick(start.as_deref()),
            _ => Said::No,
        },
    };
    if said == Said::No {
        PLACING.set(id, Place::No("its human said no".into()), app::unix_now());
        return;
    }
    let id = id.to_string();
    std::thread::spawn(move || {
        let place = match handoffs::ready_place(&ask, &repo, said) {
            Ok(dir) => Place::Ready(dir),
            Err(e) => Place::No(e),
        };
        PLACING.set(&id, place, app::unix_now());
    });
}

/// Answers the prompt hook of a session that just moved here with the
/// line it is told first, or gives the call back.
pub(super) fn recall(call: RecallCall) -> Option<RecallCall> {
    if call.body["prompt"] != json!(true) {
        return Some(call);
    }
    let session = call.body["session"].as_str().unwrap_or("").to_string();
    match with(|s| s.first.remove(&session)) {
        Some(line) => {
            call.reply.send(json!({ "text": line }));
            None
        }
        None => Some(call),
    }
}

/// Who session `id` is being handed to while its handoff is under way:
/// that the roster says the receiver owns it is the handoff, not a
/// takeover to fence.
pub(super) fn going_to(id: &str) -> Option<String> {
    with(|s| s.hand.to(id))
}
