//! The file tree of a project with its git changes, as the files tile shows
//! it: folders first, names sorted without regard to case, a folder chain
//! with one child each drawn as one row (`crates/horadric-ui/src`), and every
//! folder coloured by the most important change inside it, as VS Code's
//! explorer does.
//!
//! Pure. The git calls that feed it and the watcher that reruns them live in
//! `watch`.

use std::collections::{HashMap, HashSet};

/// What git says about one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Change {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflict,
}

impl Change {
    /// The letter VS Code puts beside the name.
    pub fn letter(self) -> &'static str {
        match self {
            Change::Modified => "M",
            Change::Added => "A",
            Change::Deleted => "D",
            Change::Renamed => "R",
            Change::Untracked => "U",
            Change::Conflict => "C",
        }
    }

    /// Which change a folder shows when it holds several. A conflict blocks
    /// the next commit, so it wins; an untracked file is the least news.
    fn rank(self) -> u8 {
        match self {
            Change::Conflict => 5,
            Change::Modified => 4,
            Change::Deleted => 3,
            Change::Renamed => 2,
            Change::Added => 1,
            Change::Untracked => 0,
        }
    }

    fn from_xy(x: u8, y: u8) -> Option<Change> {
        Some(match (x, y) {
            (b'?', b'?') => Change::Untracked,
            (b'!', b'!') => return None,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => Change::Conflict,
            (b'R' | b'C', _) | (_, b'R' | b'C') => Change::Renamed,
            (_, b'D') | (b'D', _) => Change::Deleted,
            (b'A', _) => Change::Added,
            _ => Change::Modified,
        })
    }
}

/// Splits NUL separated output, as every `-z` git command prints it.
pub fn parse_list(out: &[u8]) -> Vec<String> {
    out.split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect()
}

/// Reads `git status --porcelain=v1 -z`. Its paths are relative to the
/// repository root, so `prefix` (from `git rev-parse --show-prefix`, with
/// its trailing slash) is taken off, and anything outside it is dropped.
pub fn parse_status(out: &[u8], prefix: &str) -> Vec<(String, Change)> {
    let mut changes = Vec::new();
    let mut fields = out.split(|&b| b == 0);
    while let Some(f) = fields.next() {
        if f.len() < 4 {
            continue;
        }
        let (x, y) = (f[0], f[1]);
        // A rename or copy is followed by the path it came from.
        if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') {
            fields.next();
        }
        let Some(change) = Change::from_xy(x, y) else {
            continue;
        };
        let path = String::from_utf8_lossy(&f[3..]);
        if let Some(rest) = path.strip_prefix(prefix) {
            // An untracked folder is reported as one entry ending in a slash
            // unless every file was asked for; either way it is a folder.
            let rest = rest.trim_end_matches('/');
            if !rest.is_empty() {
                changes.push((rest.to_string(), change));
            }
        }
    }
    changes
}

/// One file or folder.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub name: String,
    /// From the project folder, `/` separated. Empty for the root.
    pub path: String,
    pub dir: bool,
    /// A file's own change, or a folder's most important one.
    pub change: Option<Change>,
    children: Vec<usize>,
}

/// A project's files. Node 0 is the project folder itself.
#[derive(Debug, Clone, PartialEq)]
pub struct Tree {
    nodes: Vec<Node>,
    /// How many files have a change.
    pub changed: usize,
}

impl Tree {
    /// `files` are the paths git knows (tracked and untracked, not
    /// ignored), `changes` what `git status` said. A deleted file is only in
    /// the second, and still gets a row.
    pub fn build(files: &[String], changes: &[(String, Change)]) -> Tree {
        let mut tree = Tree {
            nodes: vec![Node {
                name: String::new(),
                path: String::new(),
                dir: true,
                change: None,
                children: Vec::new(),
            }],
            changed: 0,
        };
        let mut index: HashMap<String, usize> = HashMap::new();
        let by_path: HashMap<&str, Change> =
            changes.iter().map(|(p, c)| (p.as_str(), *c)).collect();
        let mut seen = HashSet::new();
        let all = files
            .iter()
            .map(String::as_str)
            .chain(changes.iter().map(|(p, _)| p.as_str()));
        for path in all {
            if !seen.insert(path) {
                continue;
            }
            let change = by_path.get(path).copied();
            if change.is_some() {
                tree.changed += 1;
            }
            tree.insert(&mut index, path, change);
        }
        tree.sort(0);
        tree.roll_up(0);
        tree
    }

    fn insert(&mut self, index: &mut HashMap<String, usize>, path: &str, change: Option<Change>) {
        let mut parent = 0;
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        for (i, part) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            let sub = parts[..=i].join("/");
            if let Some(&n) = index.get(&sub) {
                parent = n;
                continue;
            }
            let n = self.nodes.len();
            self.nodes.push(Node {
                name: part.to_string(),
                path: sub.clone(),
                dir: !last,
                change: if last { change } else { None },
                children: Vec::new(),
            });
            self.nodes[parent].children.push(n);
            index.insert(sub, n);
            parent = n;
        }
    }

    fn sort(&mut self, n: usize) {
        let mut kids = std::mem::take(&mut self.nodes[n].children);
        kids.sort_by(|&a, &b| {
            let (a, b) = (&self.nodes[a], &self.nodes[b]);
            b.dir
                .cmp(&a.dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.name.cmp(&b.name))
        });
        for &k in &kids {
            self.sort(k);
        }
        self.nodes[n].children = kids;
    }

    fn roll_up(&mut self, n: usize) -> Option<Change> {
        if !self.nodes[n].dir {
            return self.nodes[n].change;
        }
        let kids = self.nodes[n].children.clone();
        let mut best: Option<Change> = None;
        for k in kids {
            if let Some(c) = self.roll_up(k) {
                if best.is_none_or(|b| c.rank() > b.rank()) {
                    best = Some(c);
                }
            }
        }
        self.nodes[n].change = best;
        best
    }

    pub fn node(&self, n: usize) -> &Node {
        &self.nodes[n]
    }

    /// The rows to draw, top to bottom, for the folders open in `view`.
    pub fn rows(&self, view: &Expansion) -> Vec<Row> {
        let mut out = Vec::new();
        self.walk(0, 0, view, &mut out);
        out
    }

    fn walk(&self, n: usize, depth: usize, view: &Expansion, out: &mut Vec<Row>) {
        for &k in &self.nodes[n].children {
            let mut node = &self.nodes[k];
            let mut at = k;
            let mut label = node.name.clone();
            // A folder whose only child is a folder shares its row.
            while node.dir && node.children.len() == 1 && self.nodes[node.children[0]].dir {
                at = node.children[0];
                node = &self.nodes[at];
                label.push('/');
                label.push_str(&node.name);
            }
            let open = node.dir && view.is_open(node);
            out.push(Row {
                node: at,
                label,
                depth,
                open,
            });
            if open {
                self.walk(at, depth + 1, view, out);
            }
        }
    }
}

/// One line of the tile.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub node: usize,
    /// The name, or several folder names joined with `/`.
    pub label: String,
    pub depth: usize,
    pub open: bool,
}

/// Which folders are open. A folder holding a change opens by itself, so a
/// new change is in view without a click; a click overrides that either
/// way and is remembered by path, so it survives the tree being rebuilt.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Expansion {
    opened: HashSet<String>,
    closed: HashSet<String>,
}

impl Expansion {
    pub fn is_open(&self, node: &Node) -> bool {
        if self.opened.contains(&node.path) {
            return true;
        }
        if self.closed.contains(&node.path) {
            return false;
        }
        node.change.is_some()
    }

    pub fn toggle(&mut self, node: &Node) {
        if self.is_open(node) {
            self.opened.remove(&node.path);
            self.closed.insert(node.path.clone());
        } else {
            self.closed.remove(&node.path);
            self.opened.insert(node.path.clone());
        }
    }
}

/// Whether a path the watcher reported can change what the tile shows.
/// `ignored` holds what `git ls-files --ignored --directory` printed, where a
/// folder ends in a slash. Inside `.git` only the index and `HEAD` matter:
/// staging, committing and switching branch all rewrite one of them, and
/// everything else in there churns on every git command.
pub fn relevant(path: &str, ignored: &HashSet<String>) -> bool {
    let path = path.replace('\\', "/");
    if let Some(inner) = path.strip_prefix(".git/") {
        return inner == "index" || inner == "HEAD";
    }
    if path == ".git" {
        return false;
    }
    let mut end = 0;
    for part in path.split('/') {
        end += part.len();
        let sub = &path[..end];
        if ignored.contains(sub) || ignored.contains(&format!("{sub}/")) {
            return false;
        }
        end += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|p| p.to_string()).collect()
    }

    fn labels(tree: &Tree, view: &Expansion) -> Vec<String> {
        tree.rows(view)
            .iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), r.label))
            .collect()
    }

    #[test]
    fn status_reads_every_kind_and_skips_the_rename_source() {
        let out = b" M src/a.rs\0A  src/b.rs\0R  src/new.rs\0src/old.rs\0?? notes.md\0 D gone.rs\0UU both.rs\0";
        let got = parse_status(out, "");
        assert_eq!(
            got,
            vec![
                ("src/a.rs".into(), Change::Modified),
                ("src/b.rs".into(), Change::Added),
                ("src/new.rs".into(), Change::Renamed),
                ("notes.md".into(), Change::Untracked),
                ("gone.rs".into(), Change::Deleted),
                ("both.rs".into(), Change::Conflict),
            ]
        );
    }

    #[test]
    fn status_is_made_relative_to_the_project_folder() {
        let out = b" M app/src/a.rs\0 M other/b.rs\0";
        assert_eq!(
            parse_status(out, "app/"),
            vec![("src/a.rs".into(), Change::Modified)]
        );
    }

    #[test]
    fn list_splits_on_nul() {
        assert_eq!(parse_list(b"a\0b/c\0"), s(&["a", "b/c"]));
        assert!(parse_list(b"").is_empty());
    }

    #[test]
    fn folders_come_first_and_names_ignore_case() {
        let tree = Tree::build(&s(&["b.rs", "Zed/x", "a.rs", "docs/y", "C.rs"]), &[]);
        let mut view = Expansion::default();
        assert_eq!(
            labels(&tree, &view),
            s(&["docs", "Zed", "a.rs", "b.rs", "C.rs"])
        );
        let zed = tree.rows(&view)[1].node;
        view.toggle(tree.node(zed));
        assert_eq!(
            labels(&tree, &view),
            s(&["docs", "Zed", "  x", "a.rs", "b.rs", "C.rs"])
        );
    }

    #[test]
    fn a_folder_shows_its_most_important_change_and_opens_by_itself() {
        let files = s(&["src/a.rs", "src/b.rs", "src/new.rs", "README.md"]);
        let changes = vec![
            ("src/new.rs".to_string(), Change::Untracked),
            ("src/a.rs".to_string(), Change::Modified),
        ];
        let tree = Tree::build(&files, &changes);
        assert_eq!(tree.changed, 2);
        let view = Expansion::default();
        let rows = tree.rows(&view);
        assert_eq!(tree.node(rows[0].node).change, Some(Change::Modified));
        assert!(rows[0].open);
        assert_eq!(
            labels(&tree, &view),
            s(&["src", "  a.rs", "  b.rs", "  new.rs", "README.md"])
        );
        assert_eq!(tree.node(rows[4].node).change, None);
    }

    #[test]
    fn a_click_closes_a_changed_folder_and_the_choice_survives_a_rebuild() {
        let files = s(&["src/a.rs", "lib/b.rs"]);
        let changes = vec![("src/a.rs".to_string(), Change::Modified)];
        let tree = Tree::build(&files, &changes);
        let mut view = Expansion::default();
        let src = tree.rows(&view)[1].node;
        assert_eq!(tree.node(src).path, "src");
        view.toggle(tree.node(src));
        let again = Tree::build(&files, &changes);
        assert_eq!(labels(&again, &view), s(&["lib", "src"]));
    }

    #[test]
    fn single_child_folders_share_a_row() {
        let files = s(&["crates/ui/src/a.rs", "crates/ui/src/b.rs", "top.rs"]);
        let changes = vec![("crates/ui/src/a.rs".to_string(), Change::Added)];
        let tree = Tree::build(&files, &changes);
        let view = Expansion::default();
        let rows = tree.rows(&view);
        assert_eq!(rows[0].label, "crates/ui/src");
        assert_eq!(tree.node(rows[0].node).path, "crates/ui/src");
        assert_eq!(
            labels(&tree, &view),
            s(&["crates/ui/src", "  a.rs", "  b.rs", "top.rs"])
        );
    }

    #[test]
    fn a_deleted_file_is_shown_though_git_no_longer_lists_it() {
        let tree = Tree::build(
            &s(&["keep.rs"]),
            &[("gone.rs".to_string(), Change::Deleted)],
        );
        let rows = tree.rows(&Expansion::default());
        assert_eq!(rows.len(), 2);
        assert_eq!(tree.node(rows[0].node).change, Some(Change::Deleted));
    }

    #[test]
    fn a_file_listed_twice_counts_once() {
        let tree = Tree::build(
            &s(&["new.rs"]),
            &[("new.rs".to_string(), Change::Untracked)],
        );
        assert_eq!(tree.changed, 1);
        assert_eq!(tree.rows(&Expansion::default()).len(), 1);
    }

    #[test]
    fn relevant_skips_ignored_folders_and_git_internals() {
        let ignored: HashSet<String> = ["target/".to_string(), "debug.log".to_string()].into();
        assert!(relevant("src\\main.rs", &ignored));
        assert!(!relevant("target\\debug\\horadric.exe", &ignored));
        assert!(!relevant("target", &ignored));
        assert!(!relevant("debug.log", &ignored));
        assert!(relevant("targets/x", &ignored));
        assert!(relevant(".git\\index", &ignored));
        assert!(relevant(".git\\HEAD", &ignored));
        assert!(!relevant(".git\\objects\\ab\\cd", &ignored));
        assert!(!relevant(".git\\index.lock", &ignored));
    }
}
