//! A node's side of handing a session to a Windows PC and taking one in
//! (see docs/plan/the-brotherhood.md, step 11). The steps and the disk
//! work are `horadric_core::handing`'s, as on Windows; this is the glue to
//! the node's hosts and its one link per viewer.
//!
//! A Windows PC reaches a node as a viewer over `ssh`, so both ways go on
//! that one link: the node serves the PC's `bases` ask and `bundle` parts
//! as any brother does, and sends its own asks to the viewer whose hello
//! named the PC, hearing the answers through the server.
//!
//! What the node lands goes into a worktree of its own checkout of the
//! project, found by the repository's identity among the folders it knows
//! and those in the home folder. Without one, it clones the project beside
//! its other checkouts, since nobody is at a node to ask where; a project
//! with no remote it refuses.
//!
//! Each side names the other as it is known there: the PC by the name its
//! hello gave, this node by the name the PC's hello says it has there
//! ("vps"), else its host name.

use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use horadric_core::handing::{
    asks_receiver, bases_answer, refused, Do, Landed, Outgoing, PlaceAsk,
};
use horadric_core::handoff::{self, Record, RepoId};
use horadric_core::handoffs::{self, arrive, model_of, record_of, Handoffs, Packed, Place, Places};
use horadric_core::placing;
use horadric_core::saved::SavedSession;
use horadric_core::serving::Job;
use horadric_core::worktree::Worktree;
use horadric_core::{Agent, Phase};
use serde_json::Value;

use super::{lock, machine_name, node_state, relay, resume, trust, wire, Event, Machine, Shared};

/// The handoffs on their way, and what goes out to viewers.
#[derive(Default)]
pub(super) struct Handing {
    go: Handoffs,
    /// Projects cloned for handoffs here, by session: the sender asks
    /// again every few seconds and hears where each stands.
    places: Arc<Places>,
    /// The sender's try each session handed here came by, beside the
    /// node's `took`, so it is said again only to an ask of that try.
    hands: Arc<Mutex<HashMap<String, u64>>>,
    /// Messages for viewers, by the name each gave in its hello, sent by
    /// the serving thread after each round.
    pub(super) outbox: Vec<(String, Value)>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Machine {
    fn linked(&self, to: &str) -> bool {
        self.names.values().any(|n| n == to)
    }

    /// Who session `id` is being handed to while its handoff is under way:
    /// the receiver owns it a moment before its `took` comes, and that is
    /// the handoff, not a takeover to fence.
    pub(super) fn handing_out_to(&self, id: &str) -> Option<String> {
        self.hand.go.to(id)
    }

    /// Whether `who` names this node: its relay id or its name.
    fn is_me(&self, who: &str) -> bool {
        who == machine_name() || relay::me().is_some_and(|me| me == who)
    }

    /// This node as the viewer named `who` calls it.
    fn called_by(&self, who: &str) -> String {
        let ch = self
            .names
            .iter()
            .filter(|(_, n)| *n == who)
            .map(|(ch, _)| *ch)
            .max();
        let heard = ch.and_then(|ch| self.called.get(&ch));
        wire::known_as(heard.map(String::as_str), &machine_name())
    }

    /// Hands session `id` to the viewer named `to` once its turn ends, as
    /// a `handoff` from that viewer asks.
    pub(super) fn handoff_to(&mut self, id: &str, to: &str) -> Result<(), String> {
        // A relay brother is asked for by its name or its id, and goes by
        // its id here.
        let to = self.kin.id_of(to).unwrap_or_else(|| to.to_string());
        let to = to.as_str();
        // A silent brother's session this node keeps, asked to come here:
        // a takeover.
        if lock(&self.node).registry.get(id).is_none() {
            if let Some(from) = self.keeping_holder(id) {
                if !self.is_me(to) {
                    return Err("only this node can take it over from here".into());
                }
                return self.keeping_take_over(&from, id);
            }
        }
        {
            let n = lock(&self.node);
            let s = n.registry.get(id).ok_or("no such session here")?;
            if !s.prompted || s.claude_session_id.is_none() {
                return Err("it has no conversation to resume yet".into());
            }
            if !n.hosted.contains_key(id) && s.phase != Phase::Paused {
                return Err("it is not run by this node".into());
            }
        }
        if !self.linked(to) {
            return Err(format!("{to} is not linked to this node"));
        }
        self.hand.go.start(id, to, false);
        self.handing_tick();
        Ok(())
    }

    /// Moves each handoff on a step. Called after every round of serving.
    pub(super) fn handing_tick(&mut self) {
        let at = now();
        for id in self.hand.go.ids() {
            let phase = lock(&self.node).registry.get(&id).map(|s| s.phase.clone());
            let linked = self.hand.go.to(&id).is_some_and(|to| self.linked(&to));
            let Some((o, step)) = self.hand.go.tick(&id, phase.as_ref(), linked, at) else {
                continue;
            };
            self.line(&id, &o.line());
            self.handing_do(&id, &o, step);
        }
    }

    /// What the session's tile says while it goes.
    fn line(&self, id: &str, line: &str) {
        if let Some(s) = lock(&self.node).registry.get_mut(id) {
            s.last_line = line.to_string();
        }
    }

    fn handing_do(&mut self, id: &str, o: &Outgoing, step: Do) {
        match step {
            Do::Wait => {}
            // Esc stops a turn in Claude Code, as the human would.
            Do::Interrupt => {
                if let Some(h) = lock(&self.node).hosted.get(id) {
                    h.remote.write(b"\x1b".to_vec());
                }
            }
            Do::AskBases => {
                // The agent stops first, so nothing changes the tree or the
                // transcript while they travel.
                let session = {
                    let n = lock(&self.node);
                    if let Some(h) = n.hosted.get(id) {
                        h.remote.kill();
                    }
                    n.registry.get(id).cloned()
                };
                let from = self.called_by(&o.to);
                let ask = self.hand.go.ask(id, || match &session {
                    Some(s) => handoffs::bases_ask_of(s, &from),
                    None => horadric_core::handing::bases_ask(id, &RepoId::default()),
                });
                self.hand.outbox.push((o.to.clone(), ask));
            }
            Do::Pack { known, clean } => self.pack(id, known, clean),
            Do::LetGo(n) => self.let_go(id, &o.to, n),
            Do::Fail(why) => {
                self.hand.go.drop(id);
                self.keeping_recall(id);
                self.line(id, &format!("not handed off: {why}"));
                eprintln!("horadric node: handing {id} off failed: {why}");
            }
        }
    }

    /// Packs session `id` on a thread for its receiver, which has `known`.
    fn pack(&mut self, id: &str, known: Vec<String>, clean: bool) {
        let to = self.hand.go.to(id).unwrap_or_default();
        let from = self.called_by(&to);
        let number = self.keeping_number(id);
        let said = self.keeping_handing(id, &to, number);
        let hand = self.hand.go.hand_of(id).unwrap_or(0);
        let record = {
            let n = lock(&self.node);
            n.registry.get(id).and_then(|s| {
                Some(Record {
                    model: model_of(n.args.get(id).map_or(&[][..], Vec::as_slice)),
                    interrupted: !clean,
                    n: number,
                    hand,
                    said: said.clone(),
                    ..record_of(s, &from)?
                })
            })
        };
        let events = self.events.clone();
        let id = id.to_string();
        std::thread::spawn(move || {
            let packed = match (record, horadric_hooks::transcript::projects()) {
                (Some(mut record), Some(projects)) => {
                    let cwd = PathBuf::from(&record.cwd);
                    horadric_core::handing::pack_session(&mut record, &cwd, &projects, &known)
                }
                (None, _) => Err("the session has gone".into()),
                (_, None) => Err("Claude Code's folder was not found".into()),
            };
            let _ = events.send(Event::Packed(id, packed));
        });
    }

    /// A packed session came back from its thread: its parts go out.
    pub(super) fn packed(&mut self, id: &str, packed: Result<Vec<u8>, String>) {
        match self.hand.go.packed(id, packed, now()) {
            Packed::Gone => {}
            Packed::Send(to, parts) => {
                for msg in parts {
                    self.hand.outbox.push((to.clone(), msg));
                }
            }
            Packed::Fail(o, step) => self.handing_do(id, &o, step),
        }
    }

    /// The receiver runs it: the session, its console and its worktree go
    /// from here.
    fn let_go(&mut self, id: &str, to: &str, n: u64) {
        self.hand.go.let_go(id);
        self.keeping_handed(id, Some(to), n, &[]);
        let gone = {
            let mut n = lock(&self.node);
            if let Some(h) = n.hosted.remove(id) {
                h.remote.kill();
            }
            n.args.remove(id);
            n.dirty = true;
            n.registry.remove(id)
        };
        let Some(s) = gone else {
            return;
        };
        if let Some(session) = &s.claude_session_id {
            horadric_core::handing::forget_ref(Path::new(&s.cwd), session);
        }
        if let Some(w) = s.worktree {
            // Off this thread: git may take a while over a big tree.
            std::thread::spawn(move || discard(&w));
        }
        eprintln!("horadric node: {id} runs on {} now", self.shown(to));
    }

    /// Hears what a viewer answered about a handoff from here: its
    /// commits, `took` or a refusal (the server's `handing_answer`).
    pub(super) fn heard_answer(&mut self, who: &str, msg: &Value) {
        let name = self.shown(who);
        let Some((o, step)) = self.hand.go.answer(who, &name, msg, now()) else {
            return;
        };
        self.handing_do(&o.id, &o, step);
    }

    /// A viewer's `bases` or `bundle` (the server's `handing`): asks of
    /// this node as receiver are answered by a job; the `bases` answer to
    /// this node's own ask moves its handoff on.
    pub(super) fn serve_handing(&mut self, who: &str, msg: &Value) -> Option<Job> {
        if !asks_receiver(msg) {
            self.heard_answer(who, msg);
            return None;
        }
        let id = msg["id"].as_str().unwrap_or("").to_string();
        if msg["t"] == "bases" {
            // Handed here already: the sender asks again because its link
            // dropped before it heard `took`.
            let ran = {
                let n = lock(&self.node);
                n.took
                    .get(&id)
                    .copied()
                    .filter(|_| n.registry.get(&id).is_some())
            };
            let hand = self.hand.hands.lock().map_or(0, |h| h.get(&id).copied().unwrap_or(0));
            if let Some(took) = ran.and_then(|n| handoffs::took_again_of(&id, n, hand, msg)) {
                return Some(Box::new(move || took));
            }
            let repo: RepoId = serde_json::from_value(msg["repo"].clone()).unwrap_or_default();
            let checkout = checkout_of(&self.node, &repo);
            if let (None, Some(ask)) = (&checkout, horadric_core::handing::heard_place(msg)) {
                return Some(self.placing_answer(&id, ask));
            }
            return Some(Box::new(move || {
                bases_answer(&id, checkout.map(|c| handoff::known_commits(&c)))
            }));
        }
        let package = match self.hand.go.arriving.take(who, msg, now()) {
            Ok(None) => return None,
            Ok(Some(p)) => p,
            Err(e) => return Some(Box::new(move || refused(&id, &e))),
        };
        // Never run a handoff its sender took back, or that another brother
        // already runs: landed late, it would fence the one running it.
        let r = &package.record;
        if let Err(why) = self.keeping_may_arrive(&r.id, &r.said, r.n.saturating_add(1)) {
            return Some(Box::new(move || refused(&id, &why)));
        }
        let node = Arc::clone(&self.node);
        let places = Arc::clone(&self.hand.places);
        let hands = Arc::clone(&self.hand.hands);
        // Named as each side knows the other, for the moved line.
        let from = wire::known_as(Some(&self.shown(who)), &package.record.from);
        let me = self.called_by(who);
        Some(Box::new(move || {
            let record = package.record.clone();
            let id = record.id.clone();
            if id.is_empty() || lock(&node).registry.get(&id).is_some() {
                return refused(&id, "a session of that id is here already");
            }
            let placed = places.ready(&id);
            let checkout = if placed.is_some() {
                None
            } else {
                checkout_of(&node, &record.repo)
            };
            let projects = horadric_hooks::transcript::projects();
            // Nobody at a node picks a folder for a plain copy.
            let landed = match arrive(&package, placed, checkout, projects) {
                Ok(handoffs::Arrived::Git(l)) => l,
                Ok(handoffs::Arrived::Plain(_)) => {
                    return refused(&id, "a node takes no plain copy");
                }
                Err(e) => return refused(&id, &e),
            };
            let first = handoffs::Arrived::Git(landed.clone()).first_line(&record, &from, &me);
            let shown = format!("moved here from {from}");
            match adopt(&node, &record, landed, first, &shown) {
                Ok(()) => {
                    places.forget(&id);
                    let n = record.n.saturating_add(1);
                    lock(&node).took.insert(id.clone(), n);
                    if let Ok(mut h) = hands.lock() {
                        h.insert(id.clone(), record.hand);
                    }
                    super::keeping::landed(&id, n, &record.said);
                    handoffs::took_of(&id, n, record.hand)
                }
                Err(e) => refused(&id, &format!("cannot resume it: {e}")),
            }
        }))
    }

    /// The answer to a `bases` ask for session `id` of a project this node
    /// has no checkout of: the first time, a clone beside its other
    /// checkouts starts (or a refusal saying why it cannot), then it is
    /// cloning, ready with its commits, or why it failed.
    fn placing_answer(&mut self, id: &str, ask: PlaceAsk) -> Job {
        let (job, start) = self.hand.places.answer(id, ask.plain, now());
        if !start {
            return job;
        }
        let recent: Vec<String> = lock(&self.node)
            .recent
            .iter()
            .filter(|r| Path::new(r).is_dir())
            .cloned()
            .collect();
        let home = horadric_hooks::home();
        let parent = placing::checkouts_parent(&recent, home.as_deref(), |p| Path::new(p).is_dir());
        let chosen = placing::unattended(&ask, parent.as_deref(), |f| Path::new(f).exists());
        let (url, dest) = match chosen {
            Ok(c) => c,
            Err(why) => {
                // Said at the sender's next ask, as a clone that failed is.
                self.hand.places.set(id, Place::No(why), now());
                return job;
            }
        };
        let places = Arc::clone(&self.hand.places);
        let key = id.to_string();
        // Off the serving thread: a clone takes as long as the repository
        // is large, and the sender asks again meanwhile.
        std::thread::spawn(move || {
            let dest = PathBuf::from(dest);
            let place = match placing::clone_into(&url, &dest) {
                Ok(()) => {
                    eprintln!("horadric node: cloned {url} into {}", dest.display());
                    Place::Ready(dest)
                }
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&dest);
                    Place::No(format!("the clone of {url} failed: {e}"))
                }
            };
            places.set(&key, place, now());
        });
        job
    }
}

/// Removes a session's worktree and its branch, its work being elsewhere
/// now.
pub(super) fn discard(w: &Worktree) {
    let main = Path::new(&w.main);
    if git(main, &["worktree", "remove", "--force", &w.path]) {
        git(main, &["branch", "-D", &w.branch]);
    }
}

/// Runs git in `dir`, true when it succeeded.
fn git(dir: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// This node's checkout of the repository `repo`: a folder a session works
/// or worked in, or one in the home folder or a folder below it.
pub(super) fn checkout_of(node: &Shared, repo: &RepoId) -> Option<PathBuf> {
    let mut folders: Vec<String> = {
        let n = lock(node);
        n.registry
            .all()
            .map(|s| {
                s.worktree
                    .as_ref()
                    .map_or_else(|| s.cwd.clone(), |w| w.main.clone())
            })
            .chain(n.recent.iter().cloned())
            .collect()
    };
    if let Some(home) = horadric_hooks::home() {
        folders.extend(repos_below(Path::new(&home), 2));
    }
    handoffs::checkout_among(folders, repo, false)
}

/// Git checkouts in `dir` and up to `depth` folders below it, hidden
/// folders left out.
fn repos_below(dir: &Path, depth: usize) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let hidden = e.file_name().to_string_lossy().starts_with('.');
        if hidden || !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let path = e.path();
        if path.join(".git").exists() {
            out.push(path.to_string_lossy().into_owned());
        } else if depth > 1 {
            out.extend(repos_below(&path, depth - 1));
        }
    }
    out
}

/// Takes in a handed off or taken over session that has landed: under its
/// own id in its new worktree, resumed with its conversation, its next
/// prompt opened by `first`, its tile reading `shown`.
pub(super) fn adopt(
    node: &Shared,
    record: &Record,
    landed: Landed,
    first: String,
    shown: &str,
) -> Result<(), String> {
    let id = record.id.clone();
    let args: Vec<String> = record
        .model
        .iter()
        .flat_map(|m| ["--model".to_string(), m.clone()])
        .collect();
    let cwd = landed.cwd.to_string_lossy().into_owned();
    trust(&landed.cwd);
    let saved = SavedSession {
        id: id.clone(),
        name: record.name.clone(),
        title: None,
        renamed: false,
        cwd: cwd.clone(),
        agent: Agent::Claude,
        args: args.clone(),
        claude_session_id: Some(record.session.clone()),
        prompted: true,
        last_line: shown.to_string(),
        running: false,
        shell: false,
        ssh: None,
        worktree: Some(landed.worktree.clone()),
        loot: Default::default(),
        runeword: None,
        chat: false,
    };
    {
        let mut n = lock(node);
        n.registry.add(saved.to_session(SystemTime::now()));
        n.args.insert(id.clone(), args);
        n.first.insert(id.clone(), first);
        n.recent = node_state::with_recent(&n.recent, &landed.worktree.main);
        n.dirty = true;
    }
    if let Err(e) = resume(node, &id) {
        // The sender keeps the session, so nothing of it stays here to
        // refuse the next try with.
        {
            let mut n = lock(node);
            n.first.remove(&id);
            n.args.remove(&id);
            n.registry.remove(&id);
            n.dirty = true;
        }
        discard(&landed.worktree);
        return Err(e);
    }
    Ok(())
}
