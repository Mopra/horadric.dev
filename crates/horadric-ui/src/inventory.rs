//! What a project's sessions load, read from disk: the newest transcript
//! says what one session carried and what each piece cost, the project's
//! transcripts of the last days say what was used, and the settings say
//! what is turned off. The pure half is `horadric_core::context`.
//!
//! A project's sessions are those of its main folder and of its quests'
//! worktrees, which sit beside it as `<main>.<slug>` and whose folder may
//! be gone by now. Reading them all is megabytes, so this runs off the UI
//! thread.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use horadric_core::context::{self, Item, Source, Switch, Use};
use serde_json::Value;

/// How far back the use of an item is counted.
pub const DAYS: u64 = 30;

/// One thing the project's sessions load.
#[derive(Debug, Clone)]
pub struct Line {
    pub item: Item,
    pub switch: Switch,
    /// Whether the project's settings leave it on. A line turned off since
    /// the measured session still shows what it cost then.
    pub on: bool,
    /// Calls in the last [`DAYS`], none when it was never used.
    pub used: Option<Use>,
}

impl Line {
    /// Turned on, never used in [`DAYS`], and something that can be turned
    /// off: likely waste.
    pub fn idle(&self) -> bool {
        self.on
            && self.used.is_none()
            && self.switch != Switch::None
            && self.item.source != Source::Hook
    }
}

#[derive(Debug, Clone, Default)]
pub struct Inventory {
    /// Largest first.
    pub lines: Vec<Line>,
    /// The transcript the window was measured from, none when the project
    /// has no session yet.
    pub measured: Option<PathBuf>,
    /// Transcripts read for use.
    pub sessions: usize,
}

impl Inventory {
    /// Tokens of the fixed part of the window, every line (as measured)
    /// and only those left on (after the switches). Hooks are left out,
    /// since they speak per prompt, not once.
    pub fn per_turn(&self) -> (u64, u64) {
        let fixed = self.lines.iter().filter(|l| l.item.source != Source::Hook);
        let before: u64 = fixed.clone().map(|l| l.item.tokens()).sum();
        let after: u64 = fixed.filter(|l| l.on).map(|l| l.item.tokens()).sum();
        (before, after)
    }
}

fn home() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn json(p: &Path) -> Value {
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}

/// The project's settings file the switches write.
pub fn settings_path(main: &Path) -> PathBuf {
    main.join(".claude").join("settings.json")
}

/// The plugin ids Claude Code knows: those of the user's settings and the
/// installed ones, and the plugins the claude.ai account syncs, which are
/// filed as `<name>@<marketplace>` like the rest.
fn plugin_ids(claude: &Path) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    if let Some(o) = json(&claude.join("settings.json"))
        .get("enabledPlugins")
        .and_then(Value::as_object)
    {
        ids.extend(o.keys().cloned());
    }
    if let Some(o) = json(&claude.join("plugins").join("installed_plugins.json"))
        .get("plugins")
        .and_then(Value::as_object)
    {
        ids.extend(o.keys().cloned());
    }
    let synced = claude.join("plugins").join("synced");
    for dir in std::fs::read_dir(synced).into_iter().flatten().flatten() {
        let manifest = json(&dir.path().join("manifest.json"));
        for p in manifest
            .get("plugins")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let name = p.get("name").and_then(Value::as_str);
            let market = p.get("marketplaceName").and_then(Value::as_str);
            if let (Some(n), Some(m)) = (name, market) {
                ids.push(format!("{n}@{m}"));
            }
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

/// The working directory a transcript folder's sessions ran in, from the
/// first lines of one of its transcripts.
fn cwd_of(folder: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .find(|e| e.path().extension().is_some_and(|x| x == "jsonl"))?;
    let reader = BufReader::new(std::fs::File::open(file.path()).ok()?);
    reader.lines().take(30).map_while(Result::ok).find_map(|l| {
        serde_json::from_str::<Value>(&l)
            .ok()?
            .get("cwd")?
            .as_str()
            .map(str::to_string)
    })
}

fn norm(p: &str) -> String {
    p.replace('\\', "/").trim_end_matches('/').to_lowercase()
}

/// Whether a session in `cwd` is one of the project at `main`: its main
/// folder or a worktree of it. A folder beside it that is a repository of
/// its own (`horadric.dev.website`) is not.
fn belongs(main: &str, cwd: &str) -> bool {
    let (main, cwd_n) = (norm(main), norm(cwd));
    if cwd_n == main {
        return true;
    }
    if !cwd_n.starts_with(&format!("{main}.")) && !cwd_n.starts_with(&format!("{main}-")) {
        return false;
    }
    !Path::new(cwd).join(".git").is_dir()
}

/// The transcript folders of the project at `main`.
fn folders(root: &Path, main: &Path) -> Vec<PathBuf> {
    let main_s = main.to_string_lossy();
    let prefix: String = main_s
        .trim_end_matches(['\\', '/'])
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .to_lowercase();
    std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .to_lowercase()
                .starts_with(&prefix)
        })
        .filter(|e| cwd_of(&e.path()).is_some_and(|c| belongs(&main_s, &c)))
        .map(|e| e.path())
        .collect()
}

/// What the sessions of the project at `main` load, and what they used.
pub fn gather(main: &Path) -> Inventory {
    let claude = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| home().join(".claude"));
    let Some(root) = horadric_hooks::transcript::projects() else {
        return Inventory::default();
    };
    let since = SystemTime::now() - Duration::from_secs(DAYS * 86_400);
    let mut files: Vec<(SystemTime, PathBuf)> = folders(&root, main)
        .iter()
        .filter_map(|f| std::fs::read_dir(f).ok())
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort_by_key(|(m, _)| std::cmp::Reverse(*m));

    // The newest session that started fresh carries the whole listing; a
    // resumed one only what changed since.
    let mut measured = None;
    let mut items = Vec::new();
    for (_, path) in files.iter().take(20) {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        if !text.contains("\"skill_listing\"") {
            continue;
        }
        items = context::loaded(&text);
        measured = Some(path.clone());
        break;
    }
    let mut uses: BTreeMap<String, Use> = BTreeMap::new();
    let recent: Vec<&PathBuf> = files
        .iter()
        .filter(|(m, _)| *m >= since)
        .map(|(_, p)| p)
        .collect();
    for path in &recent {
        if let Ok(text) = std::fs::read_to_string(path) {
            context::used(&text, &mut uses);
        }
    }
    let settings = json(&settings_path(main));
    let plugins = plugin_ids(&claude);
    let lines = items
        .into_iter()
        .map(|item| {
            let switch = context::switch_of(&item.key, &plugins);
            Line {
                on: context::is_on(&settings, &switch),
                used: uses.get(&item.key).cloned(),
                switch,
                item,
            }
        })
        .collect();
    Inventory {
        lines,
        measured,
        sessions: recent.len(),
    }
}

/// Turns the item `key` on or off for the project at `main`, writing its
/// `.claude/settings.json`. New sessions see it; running ones keep what
/// they started with.
pub fn toggle(main: &Path, key: &str, on: bool) -> Result<Switch, String> {
    let claude = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| home().join(".claude"));
    let switch = context::switch_of(key, &plugin_ids(&claude));
    if switch == Switch::None {
        return Err(format!(
            "{key} is not something a project's settings turn off"
        ));
    }
    let path = settings_path(main);
    let mut settings = match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s)
            .map_err(|e| format!("{} is not JSON ({e}); fix it first", path.display()))?,
        Err(_) => serde_json::json!({}),
    };
    context::set(&mut settings, &switch, on);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(switch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worktree_belongs_to_its_project_and_a_sibling_repository_does_not() {
        assert!(belongs("C:/G/horadric.dev", "c:\\g\\horadric.dev\\"));
        assert!(belongs(
            "C:/G/horadric.dev",
            "C:/G/horadric.dev.some-quest-gone"
        ));
        assert!(!belongs("C:/G/horadric.dev", "C:/G/horadric"));
        assert!(!belongs("C:/G/mrr", "C:/G/mrrx"));
    }
}
