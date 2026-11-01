//! The lore shared with linked brothers, as the serving side does it: the
//! `have` a hello tells, the `entries` that answer a brother's, and the
//! entries a brother sends taken in. The log and how it meets the lore and
//! the store are [`crate::share`]; this is when to use it. The Windows
//! app and a Linux node both serve with it, so they share alike.
//!
//! Exporting reads the store's text, which is tens of MB, so it runs on a
//! thread, and only when something it reads changed since the last time
//! that brother was sent anything: the mark of the lore file, the log and
//! the store's newest month.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use horadric_core::handoff::{self, RepoId};
use horadric_core::serving::Job;
use horadric_hooks::lore::Place;
use serde_json::{json, Value};

use crate::brethren::{self, Entry, Have};
use crate::share::{self, Shared};
use crate::store::Store;

/// The most packed bytes one `entries` message carries, so a first catch
/// up of a long lore goes as several, each well under the relay's 1 MB a
/// frame once in base64 and sealed.
pub const MOST: usize = 384 * 1024;

/// Where a project's shared lore lives: its memory folder and its key.
pub struct Project {
    pub memory: PathBuf,
    pub place: Place,
    /// This brother's id.
    pub me: String,
    /// What each brother's copy holds once the last export to it went,
    /// and the [`Project::mark`] then, by [`Theirs::token`]. Written by
    /// the export's thread, so the next look starts from what it sent.
    sent: Mutex<HashMap<String, (Have, String)>>,
}

impl Project {
    /// The project `dir` is in, None outside git or without a state folder.
    pub fn of(dir: &Path) -> Option<Project> {
        let state = horadric_hooks::state_dir()?;
        let place = Place::of(dir).ok()?;
        Some(Project {
            memory: state.join("memory"),
            me: share::brother(&state).ok()?,
            place,
            sent: Mutex::new(HashMap::new()),
        })
    }

    /// What this copy holds, by writing brother.
    pub fn have(&self) -> Have {
        Shared::open(&self.memory, self.place.key(), &self.me)
            .map(|s| s.have())
            .unwrap_or_default()
    }

    /// What changes when anything an export reads does: the lore file,
    /// the log and the store's month files, by size and time.
    pub fn mark(&self) -> String {
        let meta = |p: &Path| {
            std::fs::metadata(p)
                .ok()
                .map(|m| {
                    let at = m
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_nanos());
                    format!("{}@{at}", m.len())
                })
                .unwrap_or_default()
        };
        let log = self.memory.join("brethren").join(format!(
            "{}l",
            horadric_core::lore::file_name(self.place.key())
        ));
        let mut months: Vec<String> = std::fs::read_dir(&self.memory)
            .map(|d| {
                d.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                    .map(|p| meta(&p))
                    .collect()
            })
            .unwrap_or_default();
        months.sort();
        format!(
            "{}|{}|{}",
            meta(self.place.notes_store()),
            meta(&log),
            months.join(",")
        )
    }

    /// What the last export to the brother of `token` left it holding.
    pub fn sent(&self, token: &str) -> Option<(Have, String)> {
        let sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
        sent.get(token).cloned()
    }

    /// The `entries` messages with what a brother holding `theirs` lacks,
    /// after stamping what this brother wrote since, and what it holds
    /// then kept by `token`. Null when it lacks nothing or the lore cannot
    /// be read.
    pub fn export(&self, project: &Value, theirs: &Have, token: &str) -> Value {
        self.export_as(project, theirs, token, false)
    }

    /// [`Project::export`], for a teammate's machine as [`for_team`] cuts
    /// it when `team`.
    fn export_as(&self, project: &Value, theirs: &Have, token: &str, team: bool) -> Value {
        let run = || -> Result<Value, String> {
            let mut shared = Shared::open(&self.memory, self.place.key(), &self.me)
                .map_err(|e| e.to_string())?;
            let lore = self.place.read()?;
            let mut entries = shared
                .stamp_and_export(&lore, theirs, usize::MAX)
                .map_err(|e| e.to_string())?;
            if team {
                entries = for_team(entries, &self.me);
            }
            let held = reached(theirs, &entries);
            let mut sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
            sent.insert(token.to_string(), (held, self.mark()));
            drop(sent);
            if entries.is_empty() {
                return Ok(Value::Null);
            }
            let msgs: Vec<Value> = batches(&entries, MOST)
                .into_iter()
                .map(|batch| brethren::entries_message(project, batch))
                .collect();
            Ok(Value::Array(msgs))
        };
        run().unwrap_or(Value::Null)
    }

    /// Takes in the entries a brother sent, `b64` as `entries` carries
    /// it, inside the lore's lock.
    pub fn import(&self, b64: &str, now: u64) -> Result<(), String> {
        self.take_in(brethren::entries_of(&json!({"b64": b64})), now)
            .map(|_| ())
    }

    /// [`Project::import`] of what a teammate's machine sent, cut by
    /// [`from_team`] against what this copy already holds.
    pub fn import_team(&self, b64: &str, now: u64) -> Result<(), String> {
        let shared =
            Shared::open(&self.memory, self.place.key(), &self.me).map_err(|e| e.to_string())?;
        let talkers = talkers(shared.log());
        drop(shared);
        let entries = from_team(brethren::entries_of(&json!({"b64": b64})), &self.me, &talkers);
        self.take_in(entries, now).map(|_| ())
    }

    /// Takes in what the copy of this project's shared lore in the memory
    /// folder `memory` holds and this one lacks: another machine's, read
    /// from its backup, while that machine is silent. Gives how many
    /// entries went in. See [`Shared::export_whole`] for what waits for
    /// the brother itself.
    pub fn catch_up_from(&self, memory: &Path, now: u64) -> Result<usize, String> {
        let name = horadric_core::lore::file_name(self.place.key());
        if !memory.join("brethren").join(format!("{name}l")).is_file() {
            return Ok(0);
        }
        let theirs = Shared::open(memory, self.place.key(), &self.me).map_err(|e| e.to_string())?;
        let store = Store::text_since(memory, crate::store::Month { year: 0, month: 1 })
            .map_err(|e| e.to_string())?;
        self.take_in(theirs.export_whole(&self.have(), &store), now)
    }

    /// Takes `entries` in, inside the lore's lock. Gives how many were
    /// new here.
    fn take_in(&self, entries: Vec<Entry>, now: u64) -> Result<usize, String> {
        if entries.is_empty() {
            return Ok(0);
        }
        let mut store = Store::open(&self.memory).map_err(|e| e.to_string())?;
        let mut shared =
            Shared::open(&self.memory, self.place.key(), &self.me).map_err(|e| e.to_string())?;
        let fresh = self.place.change(|lore| {
            shared
                .import(entries, lore, &mut store, now)
                .map_err(|e| e.to_string())
        })?;
        Ok(fresh.len())
    }
}

/// The projects a brother shares lore for, each with its repository, as
/// its serving side found them at the last brother's hello: what the
/// `Serve` hooks `lore_projects`, `lore_export` and `lore_import` answer
/// from.
#[derive(Default)]
pub struct Lores(Vec<(RepoId, Arc<Project>)>);

impl Lores {
    /// The projects of `dirs` that are in git, one per repository: two
    /// checkouts of one repository share one lore.
    pub fn of(dirs: impl IntoIterator<Item = PathBuf>) -> Lores {
        let found = dirs.into_iter().filter_map(|dir| {
            let repo = handoff::repo_id(&dir)?;
            Some((repo, dir))
        });
        Lores(
            one_each(found)
                .into_iter()
                .filter_map(|(repo, dir)| Some((repo, Arc::new(Project::of(&dir)?))))
                .collect(),
        )
    }

    pub fn projects(&self) -> impl Iterator<Item = &Arc<Project>> {
        self.0.iter().map(|(_, p)| p)
    }

    /// What a hello tells of each: its repository and what this copy
    /// holds.
    pub fn told(&self) -> Vec<(Value, Value)> {
        self.0
            .iter()
            .map(|(repo, p)| (json!(repo), json!(p.have())))
            .collect()
    }

    /// This brother's project with the repository a brother names.
    pub fn find(&self, project: &Value) -> Option<Arc<Project>> {
        let wanted: RepoId = serde_json::from_value(project.clone()).ok()?;
        let mine: Vec<(String, RepoId)> = self
            .0
            .iter()
            .enumerate()
            .map(|(i, (repo, _))| (i.to_string(), repo.clone()))
            .collect();
        let i: usize = handoff::find_project(&wanted, &mine)?.parse().ok()?;
        Some(self.0[i].1.clone())
    }

    /// What `lore_export` answers: the job that sends a brother holding
    /// `theirs` what it lacks, when anything changed since the last one.
    /// `theirs` is kept by the server and updated here.
    pub fn export(&self, project: &Value, theirs: &mut Value) -> Option<Job> {
        self.export_as(project, theirs, |_| true, false)
    }

    /// [`Lores::export`] for a teammate's machine: only this brother's own
    /// notes and quest summaries ([`for_team`]), and only of a project
    /// whose lore key `shared` says is switched on for teammates.
    pub fn export_team(
        &self,
        project: &Value,
        theirs: &mut Value,
        shared: impl Fn(&str) -> bool,
    ) -> Option<Job> {
        self.export_as(project, theirs, shared, true)
    }

    fn export_as(
        &self,
        project: &Value,
        theirs: &mut Value,
        shared: impl Fn(&str) -> bool,
        team: bool,
    ) -> Option<Job> {
        let ours = self.find(project)?;
        if !shared(ours.place.key()) {
            return None;
        }
        let mut t = Theirs::of(theirs);
        t.learn(ours.sent(&t.token));
        let due = t.due(&ours.mark());
        *theirs = t.to_json();
        if !due {
            return None;
        }
        let project = project.clone();
        Some(Box::new(move || {
            ours.export_as(&project, &t.have, &t.token, team)
        }))
    }

    /// [`Lores::told`] of only the projects `shared` says are switched on
    /// for teammates, for asking a teammate's machine.
    pub fn told_team(&self, shared: impl Fn(&str) -> bool) -> Vec<(Value, Value)> {
        self.0
            .iter()
            .filter(|(_, p)| shared(p.place.key()))
            .map(|(repo, p)| (json!(repo), json!(p.have())))
            .collect()
    }

    /// [`Lores::import`] of what a teammate's machine sent, for a project
    /// `shared` says is switched on for teammates.
    pub fn import_team(
        &self,
        project: &Value,
        b64: &str,
        now: u64,
        shared: impl Fn(&str) -> bool,
    ) {
        let Some(ours) = self.find(project).filter(|p| shared(p.place.key())) else {
            return;
        };
        let b64 = b64.to_string();
        std::thread::spawn(move || {
            if let Err(e) = ours.import_team(&b64, now) {
                if std::env::var_os("HORADRIC_DEBUG").is_some() {
                    eprintln!("horadric: a teammate's lore did not go in: {e}");
                }
            }
        });
    }

    /// What `lore_import` does: takes in what a brother sent, on a thread,
    /// since it reads and writes the store.
    pub fn import(&self, project: &Value, b64: &str, now: u64) {
        let Some(ours) = self.find(project) else {
            return;
        };
        let b64 = b64.to_string();
        std::thread::spawn(move || {
            if let Err(e) = ours.import(&b64, now) {
                if std::env::var_os("HORADRIC_DEBUG").is_some() {
                    eprintln!("horadric: a brother's lore did not go in: {e}");
                }
            }
        });
    }
}

/// The first of each key, in order.
fn one_each<K: PartialEq, T>(all: impl IntoIterator<Item = (K, T)>) -> Vec<(K, T)> {
    let mut out: Vec<(K, T)> = Vec::new();
    for (k, t) in all {
        if !out.iter().any(|(seen, _)| seen == &k) {
            out.push((k, t));
        }
    }
    out
}

/// What a teammate's machine is sent of `entries`: only those brother
/// `me` wrote itself, so nothing another brother wrote reaches a teammate
/// through this one. A passage of dialogue goes as an empty entry, its
/// number without its text, so the teammate's count still moves past it
/// (a count stops at the first number it lacks). A note goes without the
/// passages it came from and replacing only `me`'s own notes, and a
/// forgetting of another brother's entry goes empty too.
pub fn for_team(entries: Vec<Entry>, me: &str) -> Vec<Entry> {
    entries
        .into_iter()
        .filter(|e| e.by == me)
        .map(|e| team_cut(e, me))
        .collect()
}

/// What this machine takes of `entries` a teammate's machine sent: only
/// entries of a writer that is neither this brother `me` nor one of
/// `talkers`, the writers whose dialogue this copy holds, which are the
/// human's own brothers (a teammate's dialogue never comes, so its writer
/// never becomes one). So a teammate cannot take a number of the human's
/// own writers and block what they send, nor forget or replace what they
/// wrote. What is taken is cut as [`for_team`] cuts what is sent: no
/// dialogue, notes replacing and forgettings naming only their own
/// writer's entries.
pub fn from_team(entries: Vec<Entry>, me: &str, talkers: &BTreeSet<String>) -> Vec<Entry> {
    entries
        .into_iter()
        .filter(|e| e.by != me && !talkers.contains(&e.by))
        .map(|e| {
            let by = e.by.clone();
            team_cut(e, &by)
        })
        .collect()
}

/// An entry as it crosses between teammates, `own` the writer whose
/// entries it may name.
fn team_cut(mut e: Entry, own: &str) -> Entry {
    e.said = None;
    e.kept = None;
    // A kind a newer Horadric added is not known to be meant for others.
    e.rest.clear();
    if let Some(note) = e.note.as_mut() {
        note.sources.clear();
        note.replaces.retain(|s| s.by == own);
    }
    if e.forget.as_ref().is_some_and(|f| f.of.by != own) {
        e.forget = None;
    }
    e
}

/// The writers whose dialogue `log` holds.
pub fn talkers(log: &brethren::Log) -> BTreeSet<String> {
    log.entries()
        .filter(|e| e.said_fingerprint().is_some())
        .map(|e| e.by.clone())
        .collect()
}

/// What a brother holding `have` holds once it has `entries` too: each
/// writer's number at the highest either names.
pub fn reached(have: &Have, entries: &[Entry]) -> Have {
    let mut out = have.clone();
    for e in entries {
        let n = out.entry(e.by.clone()).or_insert(0);
        *n = (*n).max(e.n);
    }
    out
}

/// `entries` cut into runs whose packed size stays at most `most`, in
/// order, an entry bigger than that alone in its run.
pub fn batches(entries: &[Entry], most: usize) -> Vec<&[Entry]> {
    let mut out = Vec::new();
    let (mut start, mut size) = (0, 0);
    for (i, e) in entries.iter().enumerate() {
        let len = brethren::pack([e]).len();
        if i > start && size + len > most {
            out.push(&entries[start..i]);
            (start, size) = (i, 0);
        }
        size += len;
    }
    if start < entries.len() {
        out.push(&entries[start..]);
    }
    out
}

/// What the serving side keeps of a brother's copy of one project: what
/// it holds, as it said and as was sent it since, the [`Project::mark`]
/// at the last send, and the token the export's thread files what it sent
/// under.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Theirs {
    pub have: Have,
    pub mark: Option<String>,
    pub token: String,
}

impl Theirs {
    /// From what the server holds: the `have` a `lore` message carried at
    /// first, this struct's own JSON after.
    pub fn of(v: &Value) -> Theirs {
        if let Some(token) = v.get("token").and_then(Value::as_str) {
            return Theirs {
                have: serde_json::from_value(v["have"].clone()).unwrap_or_default(),
                mark: v["mark"].as_str().map(str::to_string),
                token: token.to_string(),
            };
        }
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Theirs {
            have: serde_json::from_value(v.clone()).unwrap_or_default(),
            mark: None,
            token: NEXT.fetch_add(1, Ordering::Relaxed).to_string(),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({"have": self.have, "mark": self.mark, "token": self.token})
    }

    /// Takes in what the last export's thread found it sent.
    pub fn learn(&mut self, sent: Option<(Have, String)>) {
        if let Some((have, mark)) = sent {
            for (by, n) in have {
                let m = self.have.entry(by).or_insert(0);
                *m = (*m).max(n);
            }
            self.mark = Some(mark);
        }
    }

    /// Whether anything an export reads changed since the last one.
    pub fn due(&mut self, mark: &str) -> bool {
        if self.mark.as_deref() == Some(mark) {
            return false;
        }
        self.mark = Some(mark.to_string());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn have(pairs: &[(&str, u64)]) -> Have {
        pairs.iter().map(|(b, n)| (b.to_string(), *n)).collect()
    }

    fn entry(v: Value) -> Entry {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_teammate_gets_only_this_brothers_notes_and_dialogue_as_bare_numbers() {
        let stamp = |by: &str, n: u64| json!({"by": by, "n": n});
        let sent = for_team(
            vec![
                entry(json!({"by": "me", "n": 1, "said": {"text": "secret", "time": 1}})),
                entry(json!({"by": "me", "n": 2, "note": {"text": "a note", "at": 2,
                    "sources": [stamp("me", 1)], "replaces": [stamp("me", 0), stamp("desk", 4)]}})),
                entry(json!({"by": "me", "n": 3, "forget": {"of": stamp("desk", 4), "at": 3}})),
                entry(json!({"by": "me", "n": 4, "forget": {"of": stamp("me", 2), "at": 4}})),
                entry(json!({"by": "me", "n": 5, "newer": "kind"})),
                entry(json!({"by": "desk", "n": 1, "note": {"text": "desk's", "at": 1}})),
            ],
            "me",
        );
        assert_eq!(sent.iter().map(|e| e.n).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
        assert!(sent.iter().all(|e| e.by == "me" && e.said.is_none() && e.rest.is_empty()));
        let note = sent[1].note.as_ref().unwrap();
        assert!(note.sources.is_empty());
        assert_eq!(note.replaces.len(), 1);
        assert!(sent[2].forget.is_none() && sent[3].forget.is_some());
    }

    #[test]
    fn a_teammate_cannot_write_as_the_humans_own_brothers() {
        let talkers: BTreeSet<String> = ["desk".to_string()].into();
        let taken = from_team(
            vec![
                entry(json!({"by": "me", "n": 9, "note": {"text": "as me", "at": 1}})),
                entry(json!({"by": "desk", "n": 9, "note": {"text": "as desk", "at": 1}})),
                entry(json!({"by": "ann", "n": 1, "said": {"text": "hers", "time": 1}})),
                entry(json!({"by": "ann", "n": 2, "forget": {"of": {"by": "desk", "n": 1}, "at": 2}})),
                entry(json!({"by": "ann", "n": 3, "note": {"text": "hers", "at": 3}})),
            ],
            "me",
            &talkers,
        );
        assert_eq!(taken.iter().map(|e| e.n).collect::<Vec<_>>(), [1, 2, 3]);
        assert!(taken.iter().all(|e| e.by == "ann" && e.said.is_none() && e.forget.is_none()));
        assert!(taken[2].note.is_some());
    }

    #[test]
    fn a_long_catch_up_goes_in_runs_that_fit() {
        let entries = brethren::unpack(
            &(1..=5)
                .map(|n| {
                    format!(
                        "{{\"by\":\"a\",\"n\":{n},\"pad\":\"{}\"}}
",
                        "x".repeat(100)
                    )
                })
                .collect::<String>()
                .into_bytes(),
        );
        assert_eq!(entries.len(), 5, "the test's entries must parse");
        let one = brethren::pack([&entries[0]]).len();
        let runs = batches(&entries, one * 2);
        assert_eq!(runs.iter().map(|r| r.len()).collect::<Vec<_>>(), [2, 2, 1]);
        assert_eq!(batches(&entries, 1).len(), 5);
        assert_eq!(batches(&entries, usize::MAX).len(), 1);
        assert!(batches(&[], 10).is_empty());
    }

    #[test]
    fn a_brothers_have_is_read_first_raw_then_as_kept() {
        let raw = Theirs::of(&json!({"a": 3, "b": 1}));
        assert_eq!(raw.have, have(&[("a", 3), ("b", 1)]));
        assert_eq!(raw.mark, None);
        assert_ne!(raw.token, Theirs::of(&json!({})).token);
        assert_eq!(Theirs::of(&raw.to_json()), raw);
        assert_eq!(Theirs::of(&Value::Null).have, Have::new());
    }

    #[test]
    fn nothing_goes_again_until_the_mark_moves_past_what_the_last_send_saw() {
        let mut t = Theirs::of(&json!({"a": 3}));
        assert!(t.due("m1"));
        assert!(!t.due("m1"));
        // The export stamped, which moved the mark, and sent up to me 5.
        t.learn(Some((have(&[("a", 2), ("me", 5)]), "m2".into())));
        assert_eq!(t.have, have(&[("a", 3), ("me", 5)]));
        assert!(!t.due("m2"));
        t.learn(None);
        assert!(t.due("m3"));
    }

    #[test]
    fn two_checkouts_of_one_repository_share_the_first() {
        let all = [("a", 1), ("b", 2), ("a", 3), ("c", 4), ("b", 5)];
        assert_eq!(one_each(all), [("a", 1), ("b", 2), ("c", 4)]);
        assert!(one_each(Vec::<(u8, u8)>::new()).is_empty());
    }

    #[test]
    fn a_project_no_lore_holds_is_found_nowhere() {
        let lores = Lores::default();
        assert!(lores.told().is_empty());
        assert!(lores
            .find(&json!({"remotes": ["x"], "roots": []}))
            .is_none());
        assert!(lores.export(&json!("not a repo"), &mut json!({})).is_none());
    }

    #[test]
    fn what_was_sent_raises_each_writers_number() {
        let entries = brethren::unpack(
            b"{\"by\":\"me\",\"n\":4}\n{\"by\":\"me\",\"n\":5}\n{\"by\":\"b\",\"n\":1}\n",
        );
        assert_eq!(entries.len(), 3);
        assert_eq!(
            reached(&have(&[("a", 3), ("me", 3)]), &entries),
            have(&[("a", 3), ("b", 1), ("me", 5)])
        );
    }
}
