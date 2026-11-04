//! The handoffs one machine has under way, both ways, kept alike by a
//! Windows PC, a Linux node and the Mac: the sessions going out and where
//! each stands ([`Handoffs`]), what a receiver without the project said
//! about where it goes ([`Places`]), and landing what arrived
//! ([`arrive`]). The steps themselves are [`crate::handing`]'s; each
//! machine's layer only stops, packs, sends, adopts and shows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::Value;

use crate::handing::{
    self, bases_answer, bases_ask_placing, bases_placing, bases_refused, Arriving, Do, Landed,
    Outgoing, PlaceAsk,
};
use crate::handoff::{self, moved_line, moved_plain_line, split, Package, Record, RepoId};
use crate::placing::{self, Placement};
use crate::serving::Job;
use crate::worktree::Worktree;
use crate::Session;

/// The handoffs going out, and the bookkeeping of those that came in.
#[derive(Debug, Default)]
pub struct Handoffs {
    out: HashMap<String, Outgoing>,
    /// Bundles arriving, part by part.
    pub arriving: Arriving,
    /// The roster's number of each session handed here, and the sender's
    /// try it came by, to say `took` again to a sender whose link dropped
    /// before it heard it, and only about that try.
    took: HashMap<String, (u64, u64)>,
    /// Each outgoing session's try, new each time it starts, so a `took`
    /// from a try that failed never lets go of the next one.
    hands: HashMap<String, u64>,
    /// Each outgoing session's ask for commits, made once: the sender asks
    /// again and again while the receiver's human decides, and the ask
    /// reads the disk.
    asks: HashMap<String, Value>,
}

/// What came of packing a session for its receiver.
#[derive(Debug)]
pub enum Packed {
    /// Its handoff ended meanwhile: nothing goes.
    Gone,
    /// These parts go to the brother `to`.
    Send(String, Vec<Value>),
    /// It could not be packed: the handoff fails so.
    Fail(Outgoing, Do),
}

impl Handoffs {
    /// Hands session `id` to `to` once its turn ends, or at once with
    /// `now`. One already going keeps going where it goes.
    pub fn start(&mut self, id: &str, to: &str, now: bool) {
        if self.out.contains_key(id) {
            return;
        }
        self.out.insert(id.to_string(), Outgoing::new(id, to, now));
        self.hands.insert(id.to_string(), new_hand());
    }

    /// Which try at handing session `id` this is, while it goes: the
    /// record carries it, and the receiver says it back with `took`.
    pub fn hand_of(&self, id: &str) -> Option<u64> {
        self.out.get(id).and(self.hands.get(id).copied())
    }

    /// The handoff of session `id`, while it goes.
    pub fn going(&self, id: &str) -> Option<&Outgoing> {
        self.out.get(id)
    }

    /// Who session `id` goes to while its handoff is under way: the
    /// receiver owns it a moment before its `took` comes, and that is the
    /// handoff, not a takeover to fence.
    pub fn to(&self, id: &str) -> Option<String> {
        self.out.get(id).map(|o| o.to.clone())
    }

    /// The human clicked "Now".
    pub fn hurry(&mut self, id: &str) {
        if let Some(o) = self.out.get_mut(id) {
            o.hurry();
        }
    }

    /// The sessions going out.
    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.out.keys().cloned().collect();
        ids.sort();
        ids
    }

    pub fn is_empty(&self) -> bool {
        self.out.is_empty()
    }

    /// Moves session `id`'s handoff on a step at `at`: `phase` is the
    /// session's now, None once it is gone (its handoff goes with it),
    /// `linked` whether its receiver answers.
    pub fn tick(
        &mut self,
        id: &str,
        phase: Option<&crate::Phase>,
        linked: bool,
        at: u64,
    ) -> Option<(Outgoing, Do)> {
        let Some(phase) = phase else {
            self.drop(id);
            return None;
        };
        let o = self.out.get_mut(id)?;
        let step = o.tick(handing::between_turns(phase), linked, at);
        Some((o.clone(), step))
    }

    /// The ask for session `id`'s receiver's commits, made by `make` the
    /// first time.
    pub fn ask(&mut self, id: &str, make: impl FnOnce() -> Value) -> Value {
        let hand = self.hands.get(id).copied();
        self.asks
            .entry(id.to_string())
            .or_insert_with(|| {
                let mut ask = make();
                if let (Some(hand), Some(o)) = (hand, ask.as_object_mut()) {
                    o.insert("hand".into(), hand.into());
                }
                ask
            })
            .clone()
    }

    /// Session `id` went, or its handoff failed: it is no longer going.
    pub fn drop(&mut self, id: &str) {
        self.out.remove(id);
        self.asks.remove(id);
        self.hands.remove(id);
    }

    /// Session `id`'s receiver runs it: it goes from here, and nothing
    /// of it was handed here any more.
    pub fn let_go(&mut self, id: &str) {
        self.drop(id);
        self.took.remove(id);
    }

    /// Session `id` was packed at `at`, or could not be.
    pub fn packed(&mut self, id: &str, packed: Result<Vec<u8>, String>, at: u64) -> Packed {
        let Some(o) = self.out.get_mut(id) else {
            return Packed::Gone;
        };
        match packed {
            Ok(bytes) => {
                let parts = split(id, &bytes)
                    .iter()
                    .map(|p| serde_json::to_value(p).unwrap_or(Value::Null))
                    .collect();
                o.sent(at);
                Packed::Send(o.to.clone(), parts)
            }
            Err(e) => Packed::Fail(o.clone(), Do::Fail(format!("cannot pack it: {e}"))),
        }
    }

    /// What brother `who`, shown as `name`, answered at `at` about a
    /// handoff from here (see [`is_answer`]): the handoff it is about,
    /// moved on, and what to do next. None when no handoff from here to
    /// `who` is about that session.
    pub fn answer(
        &mut self,
        who: &str,
        name: &str,
        msg: &Value,
        at: u64,
    ) -> Option<(Outgoing, Do)> {
        let id = msg["id"].as_str()?;
        let hand = self.hands.get(id).copied();
        let o = self.out.get_mut(id).filter(|o| o.to == who)?;
        // A `took` of an earlier try, which failed and was recalled: the
        // session ran here since, and letting go would discard that.
        if msg["t"] == "took" && !same_hand(msg, hand) {
            return Some((o.clone(), Do::Wait));
        }
        let says = |none: &str| match msg["text"].as_str() {
            Some(why) => format!("{name} says {why}"),
            None => none.to_string(),
        };
        let step = match msg["t"].as_str() {
            Some("bases") => match handing::heard_bases(msg) {
                (known, true) => o.heard_bases(known),
                // Its human is saying where the project goes: asked again
                // until it is ready.
                _ if handing::heard_placing(msg) => {
                    o.heard_placing(at);
                    Do::Wait
                }
                (_, false) if matches!(o.stage, handing::Stage::Bases(_)) || o.placing() => {
                    Do::Fail(says(&format!("{name} has no checkout of this project")))
                }
                // The ask again after a drop, of a receiver that does not
                // run it (yet): the time limit decides.
                (_, false) => Do::Wait,
            },
            Some("took") => o.heard_took(handing::heard_took(msg)),
            _ => Do::Fail(says(&format!("{name} says no"))),
        };
        Some((o.clone(), step))
    }

    /// Session `id` landed here and runs, as the roster's number `n`.
    pub fn took(&mut self, id: &str, n: u64) {
        self.took_hand(id, n, 0);
    }

    /// Session `id` landed here and runs, as the roster's number `n`, by
    /// its sender's try `hand`.
    pub fn took_hand(&mut self, id: &str, n: u64, hand: u64) {
        self.took.insert(id.to_string(), (n, hand));
    }

    /// Session `id` handed here no longer runs here as handed (it was
    /// fenced, or its handoff recalled): no `took` of it is said again.
    pub fn forget_took(&mut self, id: &str) {
        self.took.remove(id);
    }

    /// The `took` to say again to a sender that asks for commits of a
    /// session that runs here already (`runs`): its link dropped before it
    /// heard it.
    pub fn took_again(&self, id: &str, runs: bool) -> Option<Value> {
        self.took_again_for(id, runs, &Value::Null)
    }

    /// [`Handoffs::took_again`] for the sender's `ask`: only when the ask
    /// is of the try that landed here. A sender that took its handoff back
    /// and tries again asks anew, and must not hear the old `took`.
    pub fn took_again_for(&self, id: &str, runs: bool, ask: &Value) -> Option<Value> {
        let &(n, hand) = self.took.get(id).filter(|_| runs)?;
        took_again_of(id, n, hand, ask)
    }
}

/// The `took` to say again for session `id`, run here as the roster's
/// number `n` by the sender's try `hand`, to the sender's `ask`: None when
/// the ask is of another try. A record that named no try (`hand` 0, from a
/// sender that did not tell) is matched to any ask, or the sender would
/// never hear `took` and would hand the session again while it runs here.
pub fn took_again_of(id: &str, n: u64, hand: u64, ask: &Value) -> Option<Value> {
    if hand != 0 && ask["hand"].as_u64().is_some_and(|a| a != hand) {
        return None;
    }
    Some(took_of(id, n, hand))
}

/// The `took` for session `id`, run here as the roster's number `n`, by
/// the sender's try `hand` (0 when it did not say).
pub fn took_of(id: &str, n: u64, hand: u64) -> Value {
    let mut took = handing::took_message(id, n);
    if hand != 0 {
        took["hand"] = hand.into();
    }
    took
}

/// Whether a `took` is of try `hand`. One that names no try comes from a
/// receiver that does not tell, and is believed as before.
fn same_hand(msg: &Value, hand: Option<u64>) -> bool {
    match msg["hand"].as_u64() {
        None => true,
        said => said == hand,
    }
}

/// A try's number, never 0 and never the same twice on this machine,
/// across restarts too.
fn new_hand() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos() as u64);
    let mut last = LAST.load(Ordering::Relaxed);
    loop {
        let next = now.max(last + 1);
        match LAST.compare_exchange(last, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(seen) => last = seen,
        }
    }
}

/// Whether `msg` answers a handoff from here: the receiver's commits, its
/// `took`, or why not (a refused bundle, a refused `handoff`).
pub fn is_answer(msg: &Value) -> bool {
    let about = |f: &str| msg["for"] == f;
    match msg["t"].as_str() {
        Some("bases" | "took") => true,
        Some("error") => about("bundle") || about("handoff"),
        _ => false,
    }
}

/// The folder a session's project stands in: the main tree of its own
/// worktree, else its folder.
pub fn project_top(s: &Session) -> String {
    s.worktree
        .as_ref()
        .map_or_else(|| s.cwd.clone(), |w| w.main.clone())
}

/// What any handoff or snapshot of session `s` says about it, sent from
/// the machine called `from`. None for one with no conversation to carry.
pub fn record_of(s: &Session, from: &str) -> Option<Record> {
    Some(Record {
        session: s.claude_session_id.clone()?,
        id: s.id.clone(),
        name: s.name.clone(),
        project: project_top(s),
        cwd: s.cwd.clone(),
        agent: "claude".into(),
        from: from.to_string(),
        ..Record::default()
    })
}

/// The model a session was started with, from its arguments.
pub fn model_of(args: &[String]) -> Option<String> {
    args.iter().enumerate().find_map(|(i, a)| {
        a.strip_prefix("--model=")
            .map(str::to_string)
            .or_else(|| (a == "--model").then(|| args.get(i + 1).cloned()).flatten())
    })
}

/// The ask for the commits of session `s`'s repository, sent by the
/// machine called `from`, with what a receiver's human needs to place the
/// project should it have none: the URL to clone, the folder's name, or
/// that it is a folder without git. Reads the disk.
pub fn bases_ask_of(s: &Session, from: &str) -> Value {
    let dir = Path::new(&s.cwd);
    let repo = handoff::repo_id(dir);
    let top = project_top(s);
    let place = PlaceAsk {
        url: repo
            .as_ref()
            .and_then(|_| placing::clone_url(&handoff::remote_urls(dir))),
        name: s.name.clone(),
        folder: Path::new(&top)
            .file_name()
            .map(|n| placing::folder_name(&n.to_string_lossy()))
            .unwrap_or_default(),
        plain: repo.is_none() && dir.is_dir(),
        from: from.to_string(),
    };
    bases_ask_placing(&s.id, &repo.unwrap_or_default(), &place)
}

/// The checkout of repository `repo` among `folders` (recent projects,
/// sessions' folders), each looked at once and only while it is there.
/// `fold` takes two spellings that differ only in case for one folder, as
/// Windows does.
pub fn checkout_among(folders: Vec<String>, repo: &RepoId, fold: bool) -> Option<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mine: Vec<(String, RepoId)> = folders
        .into_iter()
        .filter(|f| !f.is_empty())
        .filter(|f| seen.insert(if fold { f.to_lowercase() } else { f.clone() }))
        .filter(|f| Path::new(f).is_dir())
        .filter_map(|f| Some((f.clone(), handoff::repo_id(Path::new(&f))?)))
        .collect();
    handoff::find_project(repo, &mine).map(PathBuf::from)
}

/// Removes a handed off session's worktree and its branch, its work being
/// on its receiver now.
pub fn discard(w: &Worktree) {
    let main = Path::new(&w.main);
    let git = |args: &[&str]| handoff::git(main, args, &[]).is_ok();
    if git(&["worktree", "remove", "--force", &w.path]) {
        git(&["branch", "-D", &w.branch]);
    }
}

/// Where a project this machine has no checkout of goes, as its human (or,
/// on a node, the clone it started) said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// The human is asked, or the clone runs.
    Asking,
    /// It goes here: a checkout of the project, or for a plain copy a new
    /// or empty folder.
    Ready(PathBuf),
    /// Not here, and why.
    No(String),
}

/// Each arriving session's place, by its id, with when it was first
/// asked. Threads clone and fill it, so it holds its own lock.
#[derive(Debug, Default)]
pub struct Places(Mutex<Vec<(String, Place, u64)>>);

impl Places {
    pub const fn new() -> Self {
        Places(Mutex::new(Vec::new()))
    }

    fn with<R>(&self, f: impl FnOnce(&mut Vec<(String, Place, u64)>) -> R) -> R {
        f(&mut self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn get(&self, id: &str) -> Option<Place> {
        self.with(|p| p.iter().find(|(i, ..)| i == id).map(|(_, p, _)| p.clone()))
    }

    /// The folder session `id` goes into, once it is ready.
    pub fn ready(&self, id: &str) -> Option<PathBuf> {
        match self.get(id)? {
            Place::Ready(dir) => Some(dir),
            _ => None,
        }
    }

    pub fn set(&self, id: &str, place: Place, now: u64) {
        self.with(|p| match p.iter_mut().find(|(i, ..)| i == id) {
            Some(entry) => entry.1 = place,
            None => p.push((id.to_string(), place, now)),
        });
    }

    pub fn forget(&self, id: &str) {
        self.with(|p| p.retain(|(i, ..)| i != id));
    }

    /// Drops the places nobody came for in time at `now`: their senders
    /// gave up.
    pub fn expire(&self, now: u64) {
        self.with(|p| {
            p.retain(|(_, _, since)| now.saturating_sub(*since) < handing::PLACE_SECS + 120)
        });
    }

    fn asking(&self) -> usize {
        self.with(|p| p.iter().filter(|(_, p, _)| *p == Place::Asking).count())
    }

    /// The answer to a `bases` ask at `now` for session `id` of a project
    /// this machine has no checkout of (`plain` for a folder without git),
    /// and whether its human is to be asked (or its clone started) now:
    /// the first time it is, then the answer says it is deciding, where it
    /// goes once ready, or why not. As many at once as
    /// [`placing::MOST_PLACING`]: the sender asks again every few seconds,
    /// so one more waits its turn.
    pub fn answer(&self, id: &str, plain: bool, now: u64) -> (Job, bool) {
        let id = id.to_string();
        if id.is_empty() {
            return (Box::new(move || bases_answer(&id, None)), false);
        }
        match self.get(&id) {
            Some(Place::Ready(dir)) => (
                Box::new(move || {
                    let commits = if plain {
                        Vec::new()
                    } else {
                        handoff::known_commits(&dir)
                    };
                    bases_answer(&id, Some(commits))
                }),
                false,
            ),
            Some(Place::No(why)) => {
                self.forget(&id);
                (Box::new(move || bases_refused(&id, &why)), false)
            }
            Some(Place::Asking) => (Box::new(move || bases_placing(&id)), false),
            None if self.asking() >= placing::MOST_PLACING => {
                (Box::new(move || bases_placing(&id)), false)
            }
            None => {
                self.set(&id, Place::Asking, now);
                (Box::new(move || bases_placing(&id)), true)
            }
        }
    }
}

/// Where a session arrived: a worktree of a checkout, or a folder holding
/// a plain copy.
#[derive(Debug)]
pub enum Arrived {
    Git(Landed),
    Plain(PathBuf),
}

impl Arrived {
    /// Where it runs now.
    pub fn cwd(&self) -> &Path {
        match self {
            Arrived::Git(l) => &l.cwd,
            Arrived::Plain(dir) => dir,
        }
    }

    /// The line its next prompt starts with: it moved from `from` to `me`.
    pub fn first_line(&self, r: &Record, from: &str, me: &str) -> String {
        match self {
            Arrived::Git(l) => moved_line(from, me, &l.worktree.path, !r.interrupted),
            Arrived::Plain(dir) => {
                let folder = dir.to_string_lossy();
                moved_plain_line(from, me, &folder, !r.interrupted, &r.left)
            }
        }
    }
}

/// Lands `package`: a plain copy in the folder its human picked
/// (`placed`), else its work in a worktree of `placed` or of this
/// machine's `checkout` of the project, its transcript under `projects`
/// (Claude Code's folder, None when it was not found). Says why not in
/// words its sender is shown. Off the thread that serves, since git takes
/// as long as the work is large.
pub fn arrive(
    package: &Package,
    placed: Option<PathBuf>,
    checkout: Option<PathBuf>,
    projects: Option<PathBuf>,
) -> Result<Arrived, String> {
    let projects = projects.ok_or("Claude Code's folder was not found")?;
    if package.record.plain {
        let folder = placed.ok_or("no folder was picked for it here")?;
        return placing::land_plain(package, &folder, &projects)
            .map(Arrived::Plain)
            .map_err(|e| format!("cannot land it: {e}"));
    }
    let repo = placed
        .or(checkout)
        .ok_or("no checkout of this project here")?;
    handing::land(package, &repo, &projects)
        .map(Arrived::Git)
        .map_err(|e| format!("cannot land it: {e}"))
}

/// What a human said about where an arriving project goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    /// Clone this URL into this new folder.
    Clone(String, PathBuf),
    /// Put it in or under this folder.
    Folder(PathBuf),
    No,
}

/// What to ask a human about where an arriving project goes: a title, the
/// text, and the clone it offers (URL and new folder) beside picking a
/// folder, when there is one to offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub title: String,
    pub text: String,
    pub clone: Option<(String, String)>,
}

/// The folder name an arriving project gets: its clone's, else the
/// sender's.
pub fn name_for(ask: &PlaceAsk) -> String {
    ask.url
        .as_deref()
        .map(placing::clone_name)
        .unwrap_or_else(|| placing::folder_name(&ask.folder))
}

/// The question for a human on `here` ("PC", "Mac") about the project of
/// a session brother `who` hands it as `ask` says, its checkouts standing
/// in `parent`. `exists` says whether a folder is there.
pub fn question(
    ask: &PlaceAsk,
    who: &str,
    here: &str,
    parent: Option<&str>,
    exists: impl Fn(&str) -> bool,
) -> Question {
    let session = if ask.name.is_empty() {
        "a session".to_string()
    } else {
        format!("\u{201C}{}\u{201D}", ask.name)
    };
    let from = if ask.from.is_empty() { who } else { &ask.from };
    let title = format!("{from} hands {session} to this {here}");
    match (&ask.url, parent) {
        (Some(url), Some(parent)) if !ask.plain && placing::cloneable(url) => {
            let dest = placing::free_folder(parent, &name_for(ask), exists);
            let text = format!(
                "This {here} has no checkout of its project. Clone {url} into {dest}?\n\n\
                 Or pick a folder: one that holds the project already, or where it should go."
            );
            Question {
                title,
                text,
                clone: Some((url.clone(), dest)),
            }
        }
        _ => {
            let text = if ask.plain {
                "Its folder has no git, so it comes as a copy of the folder's files. \
                 Pick a folder to put it in: an empty one, or one it gets a new folder inside."
                    .to_string()
            } else {
                format!(
                    "This {here} has no checkout of its project, and the project has no remote \
                     to clone. Pick a folder: one that holds the project already, or where its \
                     history should go."
                )
            };
            Question {
                title,
                text,
                clone: None,
            }
        }
    }
}

/// Readies the place a human chose for the project of repository `repo`
/// that `ask` describes: clones it, takes the folder that holds it, or
/// makes one inside the folder picked. Takes as long as a clone does.
pub fn ready_place(ask: &PlaceAsk, repo: &RepoId, said: Said) -> Result<PathBuf, String> {
    let picked = match said {
        Said::Clone(url, dest) => {
            placing::clone_into(&url, &dest).map_err(|e| format!("the clone failed: {e}"))?;
            return Ok(dest);
        }
        Said::Folder(dir) => dir,
        Said::No => return Err("its human said no".into()),
    };
    let wanted = (!ask.plain).then_some(repo);
    let holds = placing::picked(&picked, wanted);
    let picked_text = picked.to_string_lossy();
    let dest = match placing::placement(&picked_text, holds, &name_for(ask), |f| {
        Path::new(f).exists()
    }) {
        Placement::Use(dir) => return Ok(PathBuf::from(dir)),
        Placement::Into(dir) => PathBuf::from(dir),
    };
    match (&ask.url, ask.plain) {
        (_, true) => std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?,
        (Some(url), false) => {
            placing::clone_into(url, &dest).map_err(|e| format!("the clone failed: {e}"))?
        }
        (None, false) => placing::init_at(&dest)?,
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handing::{bases_placing, took_message, Stage};
    use crate::Phase;
    use serde_json::json;

    fn run(job: Job) -> Value {
        job()
    }

    #[test]
    fn a_handoff_goes_through_its_answers_and_lets_go() {
        let mut h = Handoffs::default();
        h.start("s1", "b", false);
        h.start("s1", "other", true);
        assert_eq!(h.to("s1").as_deref(), Some("b"));
        let (o, step) = h.tick("s1", Some(&Phase::Idle), true, 10).unwrap();
        assert_eq!((o.stage, step), (Stage::Bases(10), Do::AskBases));
        // Only the brother it goes to is heard about it.
        let commits = json!({"t": "bases", "id": "s1", "commits": ["c1"], "found": true});
        assert!(h.answer("x", "X", &commits, 11).is_none());
        let (_, step) = h.answer("b", "B", &commits, 11).unwrap();
        assert_eq!(
            step,
            Do::Pack {
                known: vec!["c1".into()],
                clean: true
            }
        );
        match h.packed("s1", Ok(vec![1, 2, 3]), 12) {
            Packed::Send(to, parts) => assert_eq!((to.as_str(), parts.len()), ("b", 1)),
            other => panic!("{other:?}"),
        }
        let (_, step) = h.answer("b", "B", &took_message("s1", 4), 13).unwrap();
        assert_eq!(step, Do::LetGo(4));
        h.let_go("s1");
        assert!(h.is_empty());
        assert!(matches!(h.packed("s1", Ok(vec![]), 14), Packed::Gone));
    }

    #[test]
    fn a_session_that_went_drops_its_handoff_and_a_refusal_says_who() {
        let mut h = Handoffs::default();
        h.start("s1", "b", false);
        assert!(h.tick("s1", None, true, 1).is_none());
        assert!(h.is_empty());
        h.start("s2", "b", false);
        h.tick("s2", Some(&Phase::Done), true, 1);
        let no = handing::refused("s2", "no checkout");
        let (_, step) = h.answer("b", "Studio", &no, 2).unwrap();
        assert_eq!(step, Do::Fail("Studio says no checkout".into()));
        let none = json!({"t": "bases", "id": "s2", "commits": [], "found": false});
        let (_, step) = h.answer("b", "Studio", &none, 2).unwrap();
        assert_eq!(
            step,
            Do::Fail("Studio has no checkout of this project".into())
        );
        // Placing waits, and is asked again.
        let (_, step) = h.answer("b", "Studio", &bases_placing("s2"), 3).unwrap();
        assert_eq!(step, Do::Wait);
        assert!(h.going("s2").unwrap().placing());
    }

    #[test]
    fn the_ask_is_made_once_and_took_is_said_again_only_while_it_runs() {
        let mut h = Handoffs::default();
        let mut made = 0;
        for _ in 0..3 {
            h.ask("s1", || {
                made += 1;
                json!({"t": "bases"})
            });
        }
        assert_eq!(made, 1);
        h.took("s9", 7);
        assert_eq!(h.took_again("s9", true), Some(took_message("s9", 7)));
        assert_eq!(h.took_again("s9", false), None);
        assert!(is_answer(&json!({"t": "took"})));
        assert!(is_answer(&json!({"t": "error", "for": "handoff"})));
        assert!(!is_answer(&json!({"t": "error", "for": "attach"})));
    }

    #[test]
    fn a_took_of_an_earlier_try_never_lets_go_of_the_next() {
        // The sender: its first try fails, the session runs here again,
        // and a second try starts.
        let mut h = Handoffs::default();
        h.start("s1", "b", false);
        let first = h.hand_of("s1").unwrap();
        h.tick("s1", Some(&Phase::Idle), true, 10);
        let ask = h.ask("s1", || json!({"t": "bases", "id": "s1"}));
        assert_eq!(ask["hand"], first);
        h.drop("s1");
        h.start("s1", "b", false);
        let second = h.hand_of("s1").unwrap();
        assert_ne!(first, second);
        h.tick("s1", Some(&Phase::Idle), true, 20);
        let ask = h.ask("s1", || json!({"t": "bases", "id": "s1"}));
        assert_eq!(ask["hand"], second);

        // The receiver landed the first try late: asked about the second,
        // it says nothing, and its old `took` is not heard.
        let mut r = Handoffs::default();
        r.took_hand("s1", 4, first);
        assert_eq!(r.took_again_for("s1", true, &ask), None);
        let (_, step) = h.answer("b", "B", &took_of("s1", 4, first), 21).unwrap();
        assert_eq!(step, Do::Wait);
        assert!(h.going("s1").is_some());

        // A `took` of this try lets go; one naming no try is believed.
        let (_, step) = h.answer("b", "B", &took_of("s1", 4, second), 22).unwrap();
        assert_eq!(step, Do::LetGo(4));
        let (_, step) = h.answer("b", "B", &took_message("s1", 4), 22).unwrap();
        assert_eq!(step, Do::LetGo(4));

        // Asked about the try that landed, or by a sender that does not
        // tell, it says `took` again; once fenced, never.
        r.took_hand("s1", 5, second);
        let again = r.took_again_for("s1", true, &ask);
        assert_eq!(again, Some(took_of("s1", 5, second)));
        assert!(r.took_again("s1", true).is_some());
        r.forget_took("s1");
        assert_eq!(r.took_again_for("s1", true, &ask), None);
    }

    #[test]
    fn a_took_landed_without_a_try_is_said_again_to_any_ask() {
        // A sender whose record named no try, though its ask does: the
        // session runs here, so the sender must hear `took` or it hands
        // the session again.
        let mut r = Handoffs::default();
        r.took_hand("s1", 4, 0);
        let ask = json!({"t": "bases", "id": "s1", "hand": 77});
        assert_eq!(
            r.took_again_for("s1", true, &ask),
            Some(took_message("s1", 4))
        );
        assert_eq!(r.took_again_for("s1", false, &ask), None);
    }

    #[test]
    fn places_ask_once_then_answer_where_it_goes_or_why_not() {
        let p = Places::new();
        let (job, ask) = p.answer("s1", false, 100);
        assert!(ask);
        assert_eq!(run(job), bases_placing("s1"));
        let (_, ask) = p.answer("s1", false, 101);
        assert!(!ask, "asked once");
        // Only so many are asked at once.
        let (_, ask) = p.answer("s2", false, 101);
        assert!(ask);
        let (_, ask) = p.answer("s3", false, 101);
        assert!(!ask);
        p.set("s1", Place::No("its human said no".into()), 102);
        let (job, _) = p.answer("s1", false, 103);
        assert_eq!(run(job)["text"], "its human said no");
        assert_eq!(p.get("s1"), None, "a no is said once");
        let dir = std::env::temp_dir();
        p.set("s2", Place::Ready(dir.clone()), 104);
        assert_eq!(p.ready("s2"), Some(dir));
        let (job, _) = p.answer("s2", true, 105);
        assert_eq!(run(job)["found"], true);
        p.expire(101 + handing::PLACE_SECS + 120);
        assert_eq!(p.get("s2"), None);
    }

    #[test]
    fn a_sessions_model_is_read_either_way() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            model_of(&a(&["--model", "haiku"])).as_deref(),
            Some("haiku")
        );
        assert_eq!(model_of(&a(&["--model=opus"])).as_deref(), Some("opus"));
        assert_eq!(model_of(&a(&["--model"])), None);
    }

    #[test]
    fn the_question_offers_a_clone_only_with_somewhere_to_put_it() {
        let ask = PlaceAsk {
            url: Some("https://github.com/me/app.git".into()),
            name: "fix".into(),
            folder: "app".into(),
            plain: false,
            from: "Studio".into(),
        };
        let q = question(&ask, "id", "Mac", Some("/Users/me/code"), |_| false);
        assert_eq!(q.title, "Studio hands \u{201C}fix\u{201D} to this Mac");
        let (url, dest) = q.clone.unwrap();
        assert_eq!(url, "https://github.com/me/app.git");
        assert!(dest.ends_with("app"), "{dest}");
        let q = question(&ask, "id", "PC", None, |_| false);
        assert!(q.clone.is_none());
        assert!(q.text.starts_with("This PC has no checkout"));
        let plain = PlaceAsk { plain: true, ..ask };
        let q = question(&plain, "id", "PC", Some("/x"), |_| false);
        assert!(q.clone.is_none());
        assert!(q.text.contains("no git"));
    }

    #[test]
    fn a_plain_copy_needs_a_picked_folder_and_any_landing_claude_codes() {
        let package = Package {
            record: Record {
                plain: true,
                ..Record::default()
            },
            files: Vec::new(),
            work: Vec::new(),
        };
        let projects = Some(std::env::temp_dir());
        assert_eq!(
            arrive(&package, None, None, projects.clone()).unwrap_err(),
            "no folder was picked for it here"
        );
        assert_eq!(
            arrive(&package, None, None, None).unwrap_err(),
            "Claude Code's folder was not found"
        );
        let git = Package {
            record: Record::default(),
            ..package
        };
        assert_eq!(
            arrive(&git, None, None, projects).unwrap_err(),
            "no checkout of this project here"
        );
    }
}
