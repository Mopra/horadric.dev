//! The towns above the projects: folders Horadric owns, for work that
//! belongs to no project. Tristram is the first; the human adds others, a
//! work town and a private one, each with its own Claude login and browser
//! profile if they want (docs/plan/towns.md). Sessions there are ordinary
//! sessions keyed by the town's folder; the only new thing is that Horadric
//! knows the key is a town, keeps its cluster when it is empty and pins it
//! first.

use std::path::{Path, PathBuf};
use std::collections::BTreeMap;
use std::sync::{Mutex, RwLock};

use horadric_core::tasks::{self, Task, Waits};
use horadric_core::towns::{self as core, SavedTown};
use horadric_core::usage::{Limit, Limits, Usage};

/// A town as the app knows it: what the state keeps, and where it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Town {
    pub saved: SavedTown,
    pub dir: PathBuf,
    /// Its project key, asked for once since keys are asked for on every
    /// paint.
    pub key: String,
}

impl Town {
    /// Its own Claude home, when it has one: the folder being there is
    /// what says so (see `horadric_core::towns::HOME_DIR`).
    pub fn home(&self) -> Option<PathBuf> {
        Some(core::home_in(&self.dir)).filter(|h| h.is_dir())
    }
}

/// The towns, Tristram first. None until set or first asked for, when they
/// are read from the saved state: a command line process asks too.
static TOWNS: RwLock<Option<Vec<Town>>> = RwLock::new(None);

/// The town's folder under the user's home: `Horadric\tristram`, or
/// `Horadric-dev\tristram` for a dev instance, which keeps its own town.
pub fn folder_in(home: &Path, state_name: &str) -> PathBuf {
    core::folder(&home.join(state_name), core::TRISTRAM)
}

fn build(list: Vec<SavedTown>) -> Vec<Town> {
    let Some(base) = horadric_hooks::towns::base() else {
        return Vec::new();
    };
    list.into_iter()
        .map(|saved| {
            let dir = core::folder(&base, &saved.slug);
            let key = crate::project::folder_key(&dir.to_string_lossy());
            Town { saved, dir, key }
        })
        .collect()
}

/// Takes the towns the state holds, or the ones Settings changed.
pub fn set(list: Vec<SavedTown>) {
    let towns = build(core::migrate(list, false));
    if let Ok(mut t) = TOWNS.write() {
        *t = Some(towns);
    }
}

fn with<R>(f: impl FnOnce(&[Town]) -> R) -> R {
    if let Ok(t) = TOWNS.read() {
        if let Some(towns) = t.as_deref() {
            return f(towns);
        }
    }
    let towns = build(crate::store::load().towns());
    if let Ok(mut t) = TOWNS.write() {
        t.get_or_insert(towns);
    }
    match TOWNS.read() {
        Ok(t) => f(t.as_deref().unwrap_or_default()),
        Err(_) => f(&[]),
    }
}

/// Every town, Tristram first.
pub fn all() -> Vec<Town> {
    with(<[Town]>::to_vec)
}

/// The towns as the state keeps them.
pub fn saved() -> Vec<SavedTown> {
    with(|t| t.iter().map(|t| t.saved.clone()).collect())
}

/// Changes the towns, the folders of any new one made.
pub fn change(f: impl FnOnce(&mut Vec<SavedTown>)) {
    let mut list = saved();
    f(&mut list);
    set(list);
    make();
}

/// Every town's project key.
pub fn keys() -> Vec<String> {
    with(|t| t.iter().map(|t| t.key.clone()).collect())
}

/// The town whose project key is `key`.
pub fn of(key: &str) -> Option<Town> {
    with(|t| t.iter().find(|t| t.key == key).cloned())
}

/// What a town's cluster is called, in place of its folder's name.
pub fn name_of(key: &str) -> Option<String> {
    with(|t| t.iter().find(|t| t.key == key).map(|t| t.saved.name.clone()))
}

/// Whether the project with this key is a town.
pub fn is(project: &str) -> bool {
    with(|t| t.iter().any(|t| t.key == project))
}

/// Whether the town with this key is hidden.
pub fn hidden(key: &str) -> bool {
    with(|t| t.iter().any(|t| t.key == key && t.saved.hidden))
}

/// Tristram's folder, none when there is no home to put it in. Where a
/// project is founded from and the first town, which a command with no
/// town named means.
pub fn dir() -> Option<PathBuf> {
    with(|t| t.first().map(|t| t.dir.clone()))
}

/// The folder of the town with this key.
pub fn dir_of(key: &str) -> Option<PathBuf> {
    with(|t| t.iter().find(|t| t.key == key).map(|t| t.dir.clone()))
}

/// Makes every town's folder that is not there yet.
pub fn make() -> Option<PathBuf> {
    for t in all() {
        if let Err(e) = std::fs::create_dir_all(&t.dir) {
            eprintln!("horadric: cannot make the town at {}: {e}", t.dir.display());
        }
    }
    dir().filter(|d| d.is_dir())
}

/// What a session in `cwd` gets when it runs in a town with its own Claude
/// home: `CLAUDE_CONFIG_DIR` pointing at it. Nothing anywhere else, so a
/// project keeps the machine's login and Accounts.
pub fn session_env(cwd: &Path) -> Vec<(String, String)> {
    let home = horadric_hooks::towns::home_for(&cwd.to_string_lossy());
    core::env(home.as_deref())
}

/// The WebView2 profile the browser pane of the project `key` opens in,
/// None for the one shared by every project. A project's identity that
/// names itself picks one of its own, so its logins never meet another
/// identity's; a town without an identity picked has its own when it asks
/// for one. A town goes only by a pick made for it, never by a folder an
/// identity claims, since a claim on the home folder would take them all.
pub fn profile_for(key: &str) -> Option<String> {
    let town = of(key);
    #[cfg(windows)]
    {
        let identity = match &town {
            Some(_) => crate::vault::identity::pick(key).filter(|s| !s.is_empty()),
            None => crate::vault::identity::of_project(Path::new(key))
                .slug()
                .map(String::from),
        };
        if let Some(slug) = identity {
            return Some(identity_profile(&slug));
        }
    }
    town?.saved.profile()
}

/// The profile name of an identity's browser.
pub fn identity_profile(slug: &str) -> String {
    format!("identity-{slug}")
}

/// Writes every town's `CLAUDE.md` when the projects or the towns changed
/// since the last time. Only the folders count of the projects: how long
/// ago a session ran would rewrite it on every pass.
pub fn write_all(projects: &[Project]) {
    type Written = (Vec<String>, Vec<(SavedTown, bool)>);
    static LAST: Mutex<Option<Written>> = Mutex::new(None);
    let towns = all();
    let now: Written = (
        projects.iter().map(|p| p.dir.clone()).collect(),
        towns
            .iter()
            .map(|t| (t.saved.clone(), t.home().is_some()))
            .collect(),
    );
    let Ok(mut last) = LAST.lock() else {
        return;
    };
    if last.as_ref() == Some(&now) {
        return;
    }
    for t in towns.iter().filter(|t| t.dir.is_dir()) {
        write(t, projects);
    }
    *last = Some(now);
}

/// The usage limits of each town's own Claude login, by its home's folder.
/// The machine's login keeps its own in the app, as before towns.
static HOME_USAGE: Mutex<BTreeMap<String, Usage>> = Mutex::new(BTreeMap::new());

fn home_key(home: &Path) -> String {
    home.to_string_lossy().into_owned()
}

/// At the app's start: the limits as last heard of each town login, and
/// the hooks and the mod written into each town home, since `install` may
/// have run before the home was made. A dev instance writes them too: its
/// town homes are its own, under `Horadric-dev`, and never the machine's
/// `~/.claude`, which it still leaves alone.
pub fn start(saved: &BTreeMap<String, Usage>) {
    if let Ok(mut u) = HOME_USAGE.lock() {
        *u = saved.clone();
    }
    for e in horadric_hooks::install::install_towns(horadric_hooks::port()) {
        eprintln!("horadric: town hooks not written: {e}");
    }
}

/// The town logins' limits, for the state file.
pub fn usage_saved() -> BTreeMap<String, Usage> {
    HOME_USAGE.lock().map(|u| u.clone()).unwrap_or_default()
}

/// Takes in limits a session in `cwd` heard. True when they were a town
/// login's, kept here, so the machine's limits do not take them: the work
/// town's limit must never read as the private one's.
pub fn hear_limits(cwd: &str, limits: &Limits, now: u64) -> bool {
    let Some(home) = horadric_hooks::towns::home_for(cwd) else {
        return false;
    };
    if let Ok(mut all) = HOME_USAGE.lock() {
        let held = all.get(&home_key(&home));
        let merged = Usage {
            limits: held.map_or(limits.clone(), |h| h.limits.merge(limits, now)),
            at: now,
        };
        all.insert(home_key(&home), merged);
    }
    true
}

/// The limits of the login a session in project `key` runs on, when that
/// is a town's own; None for the machine's.
pub fn usage_for(key: &str) -> Option<Option<Usage>> {
    let home = of(key)?.home()?;
    Some(HOME_USAGE.lock().ok()?.get(&home_key(&home)).cloned())
}

/// The usage window's rows: the machine login's limits as before, then
/// one set for each town login heard from, named by its town.
pub fn limit_rows(machine: Option<&Usage>) -> Vec<(String, Limit)> {
    let mut rows: Vec<(String, Limit)> = machine
        .map(|u| u.limits.named())
        .unwrap_or_default()
        .into_iter()
        .map(|(n, l)| (n.to_string(), l))
        .collect();
    let Ok(heard) = HOME_USAGE.lock() else {
        return rows;
    };
    for t in all() {
        let Some(u) = t.home().and_then(|h| heard.get(&home_key(&h))) else {
            continue;
        };
        rows.extend(
            u.limits
                .named()
                .into_iter()
                .map(|(n, l)| (town_limit_name(&t.saved.name, n), l)),
        );
    }
    rows
}

/// A town login's limit as its row says it: "Acme week".
pub fn town_limit_name(town: &str, limit: &str) -> String {
    format!("{town} {}", limit.to_lowercase())
}

/// One project the town agent is told about.
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub name: String,
    pub dir: String,
    /// The first lines of its README or CLAUDE.md, empty when it has neither.
    pub intro: String,
    /// Seconds since a session last ran there, none when Horadric has not
    /// seen one.
    pub last_ran: Option<u64>,
}

/// The first lines worth reading of a README or CLAUDE.md: no blank lines,
/// no heading hashes, at most `max` lines.
pub fn intro_of(text: &str, max: usize) -> String {
    text.lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .filter(|l| !l.is_empty())
        .take(max)
        .collect::<Vec<_>>()
        .join(" ")
}

fn ago(secs: u64) -> String {
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86_400),
    }
}

/// The `CLAUDE.md` of a town's folder, a pure function of the town, whether
/// it has a login of its own, and the projects.
pub fn brief(town: &SavedTown, own_login: bool, projects: &[Project]) -> String {
    let name = &town.name;
    let mut out = format!(
        "# {name}\n\n\
         Generated by Horadric. A hand edit is lost: put your own notes in `NOTES.md` \
         beside this file, and read it first.\n\n\
         You are in {name}, a town above the projects. It belongs to no project. \
         You answer broad questions and run errands across projects. You do not edit \
         code in a project: projects run in trunk mode with other sessions in them, so \
         you would be one more writer nobody planned for. Read them, answer, and file a \
         quest with `horadric quest add` for the project's own sessions to do.\n\n"
    );
    out.push_str(&core::about_lines(town, own_login));
    out.push_str("## Projects\n\n");
    if projects.is_empty() {
        out.push_str("None yet.\n");
    }
    for p in projects {
        out.push_str(&format!("### {}\n\n- Folder: `{}`\n", p.name, p.dir));
        match p.last_ran {
            Some(s) => out.push_str(&format!("- Last session: {}\n", ago(s))),
            None => out.push_str("- Last session: none seen\n"),
        }
        if !p.intro.is_empty() {
            out.push_str(&format!("- About: {}\n", p.intro));
        }
        out.push('\n');
    }
    out
}

/// Writes a town's `CLAUDE.md` and makes `NOTES.md` if it is not there, so
/// the file the brief points at exists. Leaves `CLAUDE.md` alone when it is
/// current.
pub fn write(town: &Town, projects: &[Project]) {
    let dir = &town.dir;
    let notes = dir.join("NOTES.md");
    if !notes.exists() {
        let text = format!(
            "# Notes for {}\n\nYour own notes. Horadric never edits this file.\n",
            town.saved.name
        );
        let _ = std::fs::write(&notes, text);
    }
    let text = brief(&town.saved, town.home().is_some(), projects);
    let file = dir.join("CLAUDE.md");
    if std::fs::read_to_string(&file).is_ok_and(|old| old == text) {
        return;
    }
    if let Err(e) = std::fs::write(&file, text) {
        eprintln!("horadric: cannot write {}: {e}", file.display());
    }
}

/// `--add-dir` for each project folder, so the town agent reads them
/// without a prompt.
pub fn add_dir_args(projects: &[Project]) -> Vec<String> {
    projects
        .iter()
        .flat_map(|p| ["--add-dir".to_string(), p.dir.clone()])
        .collect()
}

/// The folder `--project` names: a project's folder name, compared without
/// case, or its whole folder. Several folders with one name are refused with
/// the matches listed, since filing a quest in the wrong project is quiet.
pub fn resolve(query: &str, dirs: &[String]) -> Result<String, String> {
    let slashed = |d: &str| d.replace('\\', "/").trim_end_matches('/').to_string();
    let q = slashed(query.trim());
    if let Some(d) = dirs.iter().find(|d| slashed(d).eq_ignore_ascii_case(&q)) {
        return Ok(d.clone());
    }
    let named: Vec<&String> = dirs
        .iter()
        .filter(|d| {
            slashed(d)
                .rsplit('/')
                .next()
                .is_some_and(|n| n.eq_ignore_ascii_case(&q))
        })
        .collect();
    match named.as_slice() {
        [] => Err(format!("no known project is called \"{query}\"")),
        [one] => Ok((*one).clone()),
        many => Err(format!(
            "\"{query}\" matches more than one project, give the folder: {}",
            many.iter()
                .map(|d| d.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The project folders Horadric knows from its saved state: the recent
/// folders that still exist, once each, without the town.
pub fn known_dirs() -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    crate::store::load()
        .recent
        .into_iter()
        .filter(|p| Path::new(p).is_dir())
        .filter(|p| {
            let key = crate::project::folder_key(p);
            !is(&key) && seen.insert(key)
        })
        .collect()
}

/// Every saved session as its folder, name and whether its process was
/// running when the state was last written.
pub fn saved_sessions() -> Vec<(String, String, bool)> {
    crate::store::load()
        .sessions
        .into_iter()
        .map(|s| (s.cwd, s.name, s.running))
        .collect()
}

/// What `horadric projects` knows of one project.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Overview {
    pub name: String,
    pub dir: String,
    /// Each session's name and whether it was running.
    pub sessions: Vec<(String, bool)>,
    pub open: usize,
    pub working: usize,
    pub last_commit: String,
}

/// The text of `horadric projects`, a pure function of what it gathered.
pub fn overview(projects: &[Overview]) -> String {
    if projects.is_empty() {
        return "No projects known yet.\n".to_string();
    }
    let mut out = String::new();
    for p in projects {
        out.push_str(&format!("{}\n  folder: {}\n", p.name, p.dir));
        if p.sessions.is_empty() {
            out.push_str("  sessions: none\n");
        } else {
            // Many sessions share a name (Warriv's), so each kind is counted.
            let mut kinds: Vec<(String, usize)> = Vec::new();
            for (n, up) in &p.sessions {
                let label = format!("{n} ({})", if *up { "running" } else { "paused" });
                match kinds.iter_mut().find(|(l, _)| *l == label) {
                    Some((_, c)) => *c += 1,
                    None => kinds.push((label, 1)),
                }
            }
            let list: Vec<String> = kinds
                .into_iter()
                .map(|(l, c)| if c > 1 { format!("{l} x{c}") } else { l })
                .collect();
            out.push_str(&format!("  sessions: {}\n", list.join(", ")));
        }
        out.push_str(&format!(
            "  quests: {} open, {} working\n",
            p.open, p.working
        ));
        if !p.last_commit.is_empty() {
            out.push_str(&format!("  last commit: {}\n", p.last_commit));
        }
    }
    out
}

/// The blocked quests of every project, so the human sees in one place
/// what waits on them: those that need them, oldest first across projects,
/// each with what they would do (else why it is blocked), then a short list
/// of those waiting on something else. `projects` is each project's name
/// and its quest log. Nothing when no quest is blocked.
pub fn needs_you(projects: &[(String, Vec<Task>)], now: u64) -> String {
    let mut yours: Vec<(&str, &Task)> = Vec::new();
    let mut waiting: Vec<(&str, &Task)> = Vec::new();
    for (name, log) in projects {
        for i in tasks::by_need(log) {
            let t = &log[i];
            match t.waits() {
                Some(Waits::Human) => yours.push((name, t)),
                _ => waiting.push((name, t)),
            }
        }
    }
    // A line with no time was written before lines had one: the oldest.
    yours.sort_by_key(|(_, t)| t.since.unwrap_or(0));
    waiting.sort_by_key(|(_, t)| (t.waits(), t.since.unwrap_or(0)));
    let age = |t: &Task| t.since.map(|s| format!(" ({})", tasks::age(s, now)));
    let mut out = String::new();
    if !yours.is_empty() {
        out.push_str(&format!(
            "{} ({})
",
            Waits::Human.group(),
            yours.len()
        ));
    }
    for (name, t) in &yours {
        let title = tasks::one_line(&t.title);
        out.push_str(&format!(
            "  {name}: {title}{}
",
            age(t).unwrap_or_default()
        ));
        let why = t
            .ask
            .as_deref()
            .or(t.reason.as_deref())
            .map(tasks::one_line);
        if let Some(w) = why.filter(|w| !w.is_empty()) {
            out.push_str(&format!(
                "    {w}
"
            ));
        }
    }
    if !waiting.is_empty() {
        out.push_str(&format!(
            "Waiting ({})
",
            waiting.len()
        ));
    }
    for (name, t) in &waiting {
        let title = tasks::one_line(&t.title);
        let on = match &t.wait {
            Some(w) => w.label(now),
            None => String::new(),
        };
        let age = age(t).unwrap_or_default();
        out.push_str(&format!(
            "  {name}: {title}, {on}{age}
"
        ));
    }
    out
}

/// Why a project can not be called this, or none when it can. The name is a
/// folder, so what a Windows folder refuses is refused here.
pub fn check_name(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return Some("a project needs a name".into());
    }
    if name
        .chars()
        .any(|c| r#"<>:"/\|?*"#.contains(c) || c.is_control())
    {
        return Some(r#"a name cannot hold any of < > : " / \ | ? *"#.into());
    }
    if name.ends_with('.') || name.ends_with(' ') || name == "." || name == ".." {
        return Some("a name cannot end in a dot or a space".into());
    }
    None
}

/// The first `README.md` of a founded project: what the town settled, which
/// the new session reads first.
pub fn readme(name: &str, summary: &str) -> String {
    let summary = summary.trim();
    if summary.is_empty() {
        format!(
            "# {name}
"
        )
    } else {
        format!(
            "# {name}

{summary}
"
        )
    }
}

/// What the first session of a founded project is asked, so it starts from
/// what the town settled instead of from nothing.
pub fn first_prompt(name: &str, summary: &str) -> String {
    let summary = summary.trim();
    if summary.is_empty() {
        format!(
            "This is {name}, founded from Tristram. Read README.md and ask what to build first."
        )
    } else {
        format!(
            "This is {name}, founded from Tristram. What the town conversation settled:              {summary}

Read README.md, then start on it."
        )
    }
}

/// Where a founded project goes by default: beside the project Horadric knew
/// most recently, which is where the human keeps their projects.
pub fn default_parent() -> Option<PathBuf> {
    known_dirs()
        .first()
        .and_then(|d| Path::new(d).parent().map(Path::to_path_buf))
}

/// Makes the folder, runs `git init` and writes the first README. A folder
/// that is already there is refused, since founding over work loses it.
pub fn found(name: &str, parent: &Path, summary: &str) -> Result<PathBuf, String> {
    let name = name.trim();
    if let Some(why) = check_name(name) {
        return Err(why);
    }
    if !parent.is_dir() {
        return Err(format!("{} is not a folder", parent.display()));
    }
    let dir = parent.join(name);
    if dir.exists() {
        return Err(format!("{} is already there", dir.display()));
    }
    std::fs::create_dir(&dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    let init = horadric_hooks::no_window(
        std::process::Command::new("git")
            .arg("init")
            .current_dir(&dir),
    )
    .output()
    .map_err(|e| format!("cannot run git: {e}"))?;
    if !init.status.success() {
        return Err("git init failed".into());
    }
    std::fs::write(dir.join("README.md"), readme(name, summary))
        .map_err(|e| format!("cannot write the README: {e}"))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_you_lists_what_waits_on_the_human_oldest_first_across_projects() {
        let day = 86_400;
        let at = |d: u64| tasks::utc(100 * day - d * day);
        let a = tasks::parse(&format!(
            concat!(
                "- [!] Name it @a-1: a long why {{you: Pick a name}} {{since: {}}}
",
                "- [!] Later @a-2: when the window opens {{until: {}}} {{since: {}}}
",
                "- [/] Busy @a-3
",
            ),
            at(2),
            tasks::utc(103 * day),
            at(1),
        ));
        let b = tasks::parse(&format!(
            "- [!] Key @b-1: needs the API key {{since: {}}}
- [!] Old @b-2: from before
",
            at(5)
        ));
        let out = needs_you(&[("a".into(), a), ("b".into(), b)], 100 * day);
        assert_eq!(
            out,
            concat!(
                "Needs you (3)
",
                "  b: Old
",
                "    from before
",
                "  b: Key (5d)
",
                "    needs the API key
",
                "  a: Name it (2d)
",
                "    Pick a name
",
                "Waiting (1)
",
                "  a: Later, in 3 d 0 h (1d)
",
            )
        );
        let calm = tasks::parse(
            "- [ ] A
- [/] B @b-1
",
        );
        assert_eq!(needs_you(&[("a".into(), calm)], 0), "");
    }

    #[test]
    fn the_town_lives_beside_the_state_it_belongs_to() {
        let home = Path::new("/home/a");
        assert_eq!(
            folder_in(home, "Horadric"),
            Path::new("/home/a/Horadric/tristram")
        );
        assert_eq!(
            folder_in(home, "Horadric-dev"),
            Path::new("/home/a/Horadric-dev/tristram")
        );
    }

    fn proj(name: &str, last: Option<u64>, intro: &str) -> Project {
        Project {
            name: name.into(),
            dir: format!("C:/code/{name}"),
            intro: intro.into(),
            last_ran: last,
        }
    }

    #[test]
    fn the_brief_lists_every_project() {
        let b = brief(
            &tristram(),
            false,
            &[proj("app", Some(7200), "A thing"), proj("lib", None, "")],
        );
        assert!(b.contains("### app"));
        assert!(b.contains("`C:/code/app`"));
        assert!(b.contains("2 h ago"));
        assert!(b.contains("About: A thing"));
        assert!(b.contains("none seen"));
        assert!(b.contains("NOTES.md"));
    }

    #[test]
    fn an_empty_town_says_so() {
        assert!(brief(&tristram(), false, &[]).contains("None yet."));
    }

    fn tristram() -> SavedTown {
        core::migrate(Vec::new(), false).remove(0)
    }

    #[test]
    fn each_town_names_itself_and_lists_its_connectors() {
        let acme = SavedTown {
            name: "Acme".into(),
            slug: "acme".into(),
            about: "Mail, chat and tickets for work at Acme.".into(),
            connectors: vec!["Microsoft 365".into(), "Slack".into()],
            ..Default::default()
        };
        let b = brief(&acme, true, &[]);
        assert!(b.starts_with("# Acme\n"));
        assert!(b.contains("You are in Acme, a town"));
        assert!(b.contains("for work at Acme."));
        assert!(b.contains("- Microsoft 365\n"));
        assert!(b.contains("account of its own"));
        assert!(!b.contains("Tristram"));
    }

    #[test]
    fn the_intro_skips_blanks_and_hashes() {
        assert_eq!(
            intro_of("# Title\n\nFirst\nSecond\nThird", 3),
            "Title First Second"
        );
    }

    #[test]
    fn every_project_folder_is_added() {
        assert_eq!(
            add_dir_args(&[proj("a", None, ""), proj("b", None, "")]),
            ["--add-dir", "C:/code/a", "--add-dir", "C:/code/b"]
        );
    }

    fn dirs() -> Vec<String> {
        vec![
            "C:/code/app".into(),
            "D:/old/app".into(),
            "C:/code/lib".into(),
        ]
    }

    #[test]
    fn a_project_is_found_by_name_or_folder() {
        assert_eq!(resolve("LIB", &dirs()).unwrap(), "C:/code/lib");
        assert_eq!(resolve("D:\\old\\app\\", &dirs()).unwrap(), "D:/old/app");
    }

    #[test]
    fn an_ambiguous_or_unknown_project_is_refused() {
        let e = resolve("app", &dirs()).unwrap_err();
        assert!(e.contains("C:/code/app") && e.contains("D:/old/app"));
        assert!(resolve("nope", &dirs()).is_err());
    }

    #[test]
    fn the_overview_says_what_goes_on() {
        let t = overview(&[Overview {
            name: "app".into(),
            dir: "C:/code/app".into(),
            sessions: vec![("a".into(), true), ("b".into(), false), ("b".into(), false)],
            open: 3,
            working: 1,
            last_commit: "abc fix".into(),
        }]);
        assert!(t.contains("a (running), b (paused) x2"));
        assert!(t.contains("3 open, 1 working"));
        assert!(t.contains("last commit: abc fix"));
        assert!(overview(&[]).contains("No projects"));
    }

    #[test]
    fn names_are_folders() {
        assert!(check_name("my-app").is_none());
        assert!(check_name("  ").is_some());
        assert!(check_name("a/b").is_some());
        assert!(check_name("a:b").is_some());
        assert!(check_name("x.").is_some());
    }

    #[test]
    fn readme_and_prompt_carry_the_summary() {
        assert_eq!(
            readme("x", " "),
            "# x
"
        );
        assert_eq!(
            readme("x", "A tool."),
            "# x

A tool.
"
        );
        assert!(first_prompt("x", "A tool.").contains("A tool."));
        assert!(first_prompt("x", "").contains("ask what to build"));
    }

    #[test]
    fn founding_refuses_an_existing_folder() {
        let parent = std::env::temp_dir().join(format!("horadric-found-{}", std::process::id()));
        std::fs::create_dir_all(&parent).unwrap();
        let dir = found("p1", &parent, "Hi.").unwrap();
        assert!(dir.join("README.md").is_file());
        assert!(found("p1", &parent, "").is_err());
        let _ = std::fs::remove_dir_all(&parent);
    }
}
