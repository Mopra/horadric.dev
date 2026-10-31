//! A brother's project files and what its sessions changed, read only, as
//! a local project shows them: its files tile, a file on the stage, and
//! each worktree session's `+12 −3` on its tile with its changed files in
//! the tile menu. Read from the brother over its link (`tree`, `read`,
//! `changes`, `diff`), never from a disk here. What the messages mean is
//! `horadric_core::afar_files`; this is the glue.
//!
//! Nothing polls. A brother is asked again only after its sessions there
//! moved on (`afar_files::Due`), as a local worktree is counted again only
//! after its agent did something.
//!
//! What was read is written under the state folder only so the stage's
//! file view, which reads a file from disk, can show it. The copy is
//! overwritten at each read and never written back.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::Instant;

use horadric_core::afar::{self, Entry};
use horadric_core::afar_files::{self, Due};
use horadric_core::diff::Diff;
use serde_json::{json, Value};

use super::App;
use crate::files::Tree;

/// Where the copies of brothers' files are kept, in the state folder.
const COPIES: &str = "afar-files";

/// What this PC asked a brother to read, to show on the stage when it comes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Read {
    /// A file of a project: the brother, the project's path there, the file.
    File(String, String, String),
    /// One file's diff in a session: the brother, the session's id there,
    /// the file.
    Diff(String, String, String),
}

/// What is asked of brothers about files, and when to ask again.
#[derive(Default)]
pub(super) struct FarFiles {
    /// By cluster key.
    trees: HashMap<String, Due>,
    /// By the session's id here.
    counts: HashMap<String, Due>,
    /// Reads asked for, and the cluster each shows in. True to bring it
    /// onto the stage, false only to refresh a view already there.
    reads: HashMap<Read, (String, bool)>,
}

/// How a session stands there, as one number: it moved on when this did.
fn stir(e: &Entry) -> u64 {
    let mut h = DefaultHasher::new();
    (format!("{:?}", e.phase), &e.line, e.since, e.paused).hash(&mut h);
    h.finish()
}

/// The folder a cluster's copies go in, one per cluster.
fn copies(key: &str) -> Option<PathBuf> {
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    Some(
        horadric_hooks::state_dir()?
            .join(COPIES)
            .join(format!("{:016x}", h.finish())),
    )
}

/// Writes what was read where the view reads it, and says where.
fn keep_copy(key: &str, rel: &str, text: &str) -> Option<PathBuf> {
    let dir = copies(key)?;
    let path = dir.join(rel.replace('/', "\\"));
    std::fs::create_dir_all(path.parent()?).ok()?;
    std::fs::write(&path, text).ok()?;
    Some(dir)
}

impl App {
    /// Asks the brother keyed `key` again what changed, where its sessions
    /// moved on since it was last asked: each project's files tile, and
    /// each worktree session's changes.
    pub(super) fn far_glance(&mut self, key: &str) {
        let Some(entries) = self.far_entries(key) else {
            return;
        };
        let now = Instant::now();
        let mut asks = Vec::new();
        let mut projects: HashMap<String, u64> = HashMap::new();
        for e in &entries {
            let stir = stir(e);
            // Summed, so the order the brother lists them in does not count.
            let p = projects.entry(e.project.clone()).or_default();
            *p = p.wrapping_add(stir);
            if !e.worktree || e.paused {
                continue;
            }
            let due = self
                .far
                .files
                .counts
                .entry(afar::local_id(key, &e.id))
                .or_default();
            due.heard(stir);
            if due.start(now) {
                asks.push(json!({"t": "changes", "id": e.id}));
            }
        }
        for (project, stir) in projects {
            let cluster = afar::key(key, &project);
            if !self.clusters.iter().any(|c| c.key == cluster) {
                continue;
            }
            let Some(path) = self.far_project_path(key, &project) else {
                continue;
            };
            let due = self.far.files.trees.entry(cluster).or_default();
            due.heard(stir);
            if due.start(now) {
                asks.push(json!({"t": "tree", "project": path}));
            }
        }
        for ask in asks {
            self.far_send(key, ask);
        }
        let r = self.shared.registry.lock();
        if let Ok(r) = r {
            self.far.files.counts.retain(|id, _| r.get(id).is_some());
        }
        let clusters = &self.clusters;
        self.far
            .files
            .trees
            .retain(|k, _| clusters.iter().any(|c| c.key == *k));
    }

    /// Takes one of the answers above from the brother keyed `key`. None
    /// for any other message; else whether the tiles changed.
    pub(super) fn far_files_heard(&mut self, key: &str, msg: &Value) -> Option<bool> {
        match msg["t"].as_str()? {
            "tree" => {
                let path = msg["project"].as_str()?;
                let cluster = self.far_cluster_of(key, path)?;
                if let Some(due) = self.far.files.trees.get_mut(&cluster) {
                    due.done();
                }
                if let Some((_, files, changes, _)) = afar_files::heard_tree(msg) {
                    if let Some(c) = self.clusters.iter().find(|c| c.key == cluster) {
                        c.far_tree(Tree::build(&files, &changes));
                    }
                }
                // A file shown from it may be one that changed.
                let shown = self.far_shown_file(&cluster);
                if let Some(rel) = shown {
                    let read = Read::File(key.to_string(), path.to_string(), rel.clone());
                    self.far.files.reads.insert(read, (cluster, false));
                    self.far_send(key, json!({"t": "read", "project": path, "path": rel}));
                }
                Some(false)
            }
            "changes" => {
                let (id, files) = afar_files::heard_changes(msg)?;
                let local = afar::local_id(key, &id);
                if let Some(due) = self.far.files.counts.get_mut(&local) {
                    due.done();
                }
                let mut changed = false;
                if let Ok(mut r) = self.shared.registry.lock() {
                    if let Some(s) = r.get_mut(&local) {
                        let diff = Some(Diff {
                            uncommitted: files,
                            committed: Vec::new(),
                        });
                        changed = s.diff != diff;
                        s.diff = diff;
                    }
                }
                if changed {
                    for c in &self.clusters {
                        c.refresh();
                    }
                }
                Some(false)
            }
            "file" => {
                let (project, path, text, cut) = afar_files::heard_file(msg)?;
                let read = Read::File(key.to_string(), project, path.clone());
                let (cluster, show) = self.far.files.reads.remove(&read)?;
                self.far_show(&cluster, &path, &text, cut, show);
                Some(false)
            }
            "diff" => {
                let id = msg["id"].as_str()?;
                let path = msg["path"].as_str().filter(|p| afar_files::safe(p))?;
                let read = Read::Diff(key.to_string(), id.to_string(), path.to_string());
                let (cluster, show) = self.far.files.reads.remove(&read)?;
                let text = msg["text"].as_str().unwrap_or_default();
                let name = format!("{path}.diff");
                self.far_show(&cluster, &name, text, msg["cut"] == true, show);
                Some(false)
            }
            "error" if matches!(msg["for"].as_str(), Some("tree" | "read")) => {
                let path = msg["project"].as_str()?;
                let cluster = self.far_cluster_of(key, path);
                if msg["for"] == "tree" {
                    if let Some(due) = cluster.and_then(|c| self.far.files.trees.get_mut(&c)) {
                        due.done();
                    }
                    return Some(false);
                }
                let rel = msg["path"].as_str().unwrap_or_default();
                let read = Read::File(key.to_string(), path.to_string(), rel.to_string());
                if let Some((_, true)) = self.far.files.reads.remove(&read) {
                    let text = msg["text"].as_str().unwrap_or("It cannot be read.");
                    self.toasts
                        .show(crate::toast::Kind::Info, &format!("Cannot show {rel}"), text);
                }
                Some(false)
            }
            _ => None,
        }
    }

    /// Asks the brother whose cluster is `cluster` for a file of the
    /// project, to show on the stage.
    pub(super) fn far_read(&mut self, cluster: &str, rel: &str) {
        let Some((key, project)) = afar::split_key(cluster) else {
            return;
        };
        let (key, project) = (key.to_string(), project.to_string());
        let Some(path) = self.far_project_path(&key, &project) else {
            return;
        };
        let read = Read::File(key.clone(), path.clone(), rel.to_string());
        self.far.files.reads.insert(read, (cluster.to_string(), true));
        self.far_send(&key, json!({"t": "read", "project": path, "path": rel}));
    }

    /// Asks the brother for what a session of its changed in one file, to
    /// show on the stage.
    pub(super) fn far_read_diff(&mut self, id: &str, rel: &str) {
        let Some(key) = self.far_brother_key(id) else {
            return;
        };
        let Some(remote) = afar::remote_id(id, &key).map(str::to_string) else {
            return;
        };
        let Some(cluster) = self.project_of(id) else {
            return;
        };
        let read = Read::Diff(key.clone(), remote.clone(), rel.to_string());
        self.far.files.reads.insert(read, (cluster, true));
        self.far_send(&key, json!({"t": "diff", "id": remote, "path": rel}));
    }

    /// Shows what was read on the stage, or only reads the view showing it
    /// again.
    fn far_show(&mut self, cluster: &str, rel: &str, text: &str, cut: bool, show: bool) {
        let text = if cut {
            format!("{text}\n[cut here: the rest is too long to send]\n")
        } else {
            text.to_string()
        };
        let Some(dir) = keep_copy(cluster, rel, &text) else {
            return;
        };
        if show {
            self.open_view(cluster, &dir, rel);
        } else {
            self.files_changed(cluster);
        }
    }

    /// The file of a brother's project shown on the stage now, by its path
    /// in the project.
    fn far_shown_file(&self, cluster: &str) -> Option<String> {
        let dir = copies(cluster)?;
        let shown = self.views.get(cluster)?.path()?;
        let rel = shown.strip_prefix(&dir).ok()?;
        let rel = rel.to_string_lossy().replace('\\', "/");
        // A diff is a session's, read again from its tile menu.
        (!rel.ends_with(".diff")).then_some(rel)
    }

    /// The cluster here of the brother's project at `path` there.
    fn far_cluster_of(&self, key: &str, path: &str) -> Option<String> {
        let name = self.far_project_name(key, path)?;
        Some(afar::key(key, &name))
    }
}
