//! Reading and writing a project's task list and its mode on disk, for the
//! app and for `horadric quest`. What the file means is
//! `horadric_core::tasks`; this is only where it lives and how a change
//! lands in it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use horadric_core::fleet::{self, Device};
use horadric_core::runeword::{self, Stone};
use horadric_core::tasks::{self, Mode, Task, CONFIG_FILE, OLD_FILE, QUESTS_FILE};
use horadric_core::{merge, ssh, warriv, worktree};

use crate::lock::Lock;

/// The list's path in a project.
pub fn file(project: &Path) -> PathBuf {
    project.join(rel(project))
}

/// The list's path from the project folder: the quest log, or a list from
/// before the rename while there is no quest log beside it.
pub fn rel(project: &Path) -> &'static str {
    tasks::list_file(
        project.join(QUESTS_FILE).is_file(),
        project.join(OLD_FILE).is_file(),
    )
}

/// The config's path in a project.
pub fn config_file(project: &Path) -> PathBuf {
    project.join(CONFIG_FILE)
}

/// The list's text, empty when there is none yet.
pub fn read(project: &Path) -> String {
    read_text(&file(project))
}

/// The project's mode as the human set it on this machine. Never set is
/// manual, whatever the repository's config suggests.
pub fn mode(project: &Path) -> Mode {
    tasks::mode(&local(project))
}

/// How many items the project's runner holds at once, as set on this
/// machine.
pub fn parallel(project: &Path) -> usize {
    tasks::parallel(&local(project))
}

/// Whether the project has Warriv on, as set on this machine.
pub fn orchestrator(project: &Path) -> bool {
    warriv::on(&local(project))
}

/// The mode the repository's config suggests, for the UI to offer. It
/// takes no effect until the human picks it.
pub fn suggested_mode(project: &Path) -> Mode {
    tasks::mode(&read_text(&config_file(project)))
}

/// Whether the repository's config suggests Warriv on, as
/// [`suggested_mode`].
pub fn suggested_orchestrator(project: &Path) -> bool {
    warriv::on(&read_text(&config_file(project)))
}

/// How many at once the repository's config suggests, as
/// [`suggested_mode`].
pub fn suggested_parallel(project: &Path) -> usize {
    tasks::parallel(&read_text(&config_file(project)))
}

/// The commands a finished quest must pass before it merges by itself, or
/// as the error those of them the human has not approved on this machine.
/// They come from the repository, so none runs unapproved.
pub fn checks(project: &Path) -> Result<Vec<String>, Vec<String>> {
    let all = merge::checks(&read_text(&config_file(project)));
    let not = tasks::unapproved(&all, &local(project));
    if not.is_empty() {
        Ok(all)
    } else {
        Err(not)
    }
}

/// Whether the human approved `command` for the project on this machine,
/// as a check or a quest's `{on cmd: ...}` wait.
pub fn approved(project: &Path, command: &str) -> bool {
    tasks::unapproved(&[command.to_string()], &local(project)).is_empty()
}

/// Approves `command` for the project on this machine: the human's
/// consent, or a wait a session here set itself.
pub fn approve(project: &Path, command: &str) -> io::Result<()> {
    change_local(project, |e| tasks::with_approved(e, command))
}

/// The local store: settings that make the runner act by itself, which a
/// repository may suggest but only the human may set. Beside the app's
/// state, so a dev instance keeps its own.
fn local_file() -> Option<PathBuf> {
    if cfg!(test) {
        return Some(std::env::temp_dir().join("horadric-test-local.json"));
    }
    crate::state_dir().map(|d| d.join("local.json"))
}

/// The project's key in the local store: its folder, the same however a
/// caller spelled it.
fn local_key(project: &Path) -> String {
    let path = fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.strip_prefix("//?/").unwrap_or(&text).trim_end_matches('/');
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text.to_string()
    }
}

/// The project's local entry, made the first time it is asked for.
fn local(project: &Path) -> String {
    let Some(path) = local_file() else {
        return String::new();
    };
    let key = local_key(project);
    if let Some(entry) = tasks::local_entry(&read_text(&path), &key) {
        return entry;
    }
    change_local(project, |e| e.to_string()).ok();
    tasks::local_entry(&read_text(&path), &key).unwrap_or_default()
}

/// Rewrites the project's local entry with `change`, under the store's
/// lock. A project not in it yet starts with what this machine changed in
/// its config before settings moved here, see `tasks::local_from`.
fn change_local(project: &Path, change: impl FnOnce(&str) -> String) -> io::Result<()> {
    let path = local_file().ok_or_else(|| io::Error::other("no state folder"))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let key = local_key(project);
    let _lock = Lock::take(&path.with_extension("lock"))?;
    let store = read_text(&path);
    let entry = tasks::local_entry(&store, &key).unwrap_or_else(|| first_entry(project));
    write(&path, &tasks::with_local_entry(&store, &key, &change(&entry)))
}

/// The entry a project starts with. Only a git work tree can tell the
/// human's changes from the commit's, so anything else (a folder unpacked
/// from a download, say) starts with nothing.
fn first_entry(project: &Path) -> String {
    let config = read_text(&config_file(project));
    if config.is_empty() || git(project, &["rev-parse", "--is-inside-work-tree"]).is_none() {
        return "{}".into();
    }
    let spec = format!("HEAD:{CONFIG_FILE}");
    let committed = git(project, &["show", &spec]).unwrap_or_default();
    let committed = committed.strip_prefix('\u{feff}').unwrap_or(&committed);
    tasks::local_from(&config, committed)
}

/// What git printed, None when it failed or could not run.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = crate::no_window(&mut std::process::Command::new("git"))
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The project's SSH hosts, none without a config.
pub fn hosts(project: &Path) -> Vec<String> {
    ssh::hosts(&read_text(&config_file(project)))
}

/// The project's inventory and the devices in it, None when its config
/// names none. A path in the config is from the project's folder. A file
/// that cannot be read is an empty fleet rather than none, so the agent is
/// still told where it should be.
pub fn fleet(project: &Path) -> Option<(PathBuf, Vec<Device>)> {
    let path = project.join(fleet::inventory(&read_text(&config_file(project)))?);
    let devices = fleet::parse(&read_text(&path));
    Some((path, devices))
}

/// Adds `host` to the project's hosts. False when it was there already or
/// is not something `ssh` takes as one destination.
pub fn add_host(project: &Path, host: &str) -> io::Result<bool> {
    let path = config_file(project);
    let old = read_text(&path);
    match ssh::with_host(&old, host) {
        Some(new) => write(&path, &new).map(|_| true),
        None => Ok(false),
    }
}

/// The plain host names in the user's `~/.ssh/config` and the files it
/// includes, in the order OpenSSH reads them, none without one.
pub fn ssh_config_hosts() -> Vec<String> {
    let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) else {
        return Vec::new();
    };
    config_hosts_in(Path::new(&home))
}

/// OpenSSH stops at this depth too, so a file that includes itself ends.
const INCLUDE_DEPTH: usize = 16;

fn config_hosts_in(home: &Path) -> Vec<String> {
    let mut out = Vec::new();
    read_ssh_config(&home.join(".ssh").join("config"), home, 0, &mut out);
    out
}

fn read_ssh_config(path: &Path, home: &Path, depth: usize, out: &mut Vec<String>) {
    for line in ssh::config_lines(&read_text(path)) {
        match line {
            ssh::ConfigLine::Host(name) if !out.contains(&name) => out.push(name),
            ssh::ConfigLine::Include(file) if depth < INCLUDE_DEPTH => {
                for f in included(&file, home) {
                    read_ssh_config(&f, home, depth + 1, out);
                }
            }
            _ => {}
        }
    }
}

/// The files an `Include` names. A relative path is from `~/.ssh`, as it
/// is for the user's own config. A wildcard is matched in the last part of
/// the path only, which is how includes are written in practice
/// (`config.d/*`), and the matches are read in name order as OpenSSH does.
fn included(file: &str, home: &Path) -> Vec<PathBuf> {
    let path = match file.strip_prefix("~/").or_else(|| file.strip_prefix("~\\")) {
        Some(rest) => home.join(rest),
        None if Path::new(file).is_absolute() => PathBuf::from(file),
        None => home.join(".ssh").join(file),
    };
    let Some(pattern) = path.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    if !pattern.contains(['*', '?']) {
        return vec![path];
    }
    let Some(dir) = path.parent() else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_file())
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| ssh::glob_matches(pattern, n))
        })
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

/// Every stone the project's Runetome holds, those that do not parse too.
pub fn stones(project: &Path) -> Vec<Stone> {
    runeword::stones(&read_text(&config_file(project)), &global_runewords())
}

/// The file of stones every project shares, `runewords.json` beside the
/// app's state: `%APPDATA%\Horadric`, or `Horadric-dev` for a dev instance.
pub fn runewords_file() -> Option<PathBuf> {
    crate::state_dir().map(|d| d.join("runewords.json"))
}

fn global_runewords() -> String {
    runewords_file().map(|f| read_text(&f)).unwrap_or_default()
}

/// What the project's config says about worktrees.
pub fn worktrees(project: &Path) -> worktree::Settings {
    worktree::settings(&read_text(&config_file(project)))
}

/// Puts the project in trunk mode, or with `enabled` gives each new session
/// a worktree of its own.
pub fn set_worktrees(project: &Path, enabled: bool) -> io::Result<()> {
    let path = config_file(project);
    let old = read_text(&path);
    write(&path, &worktree::with_enabled(&old, enabled))
}

/// Sets the project's mode on this machine. Never in the repository's
/// config: there it would reach every clone and start their agents.
pub fn set_mode(project: &Path, mode: Mode) -> io::Result<()> {
    change_local(project, |e| tasks::with_mode(e, mode))
}

/// How many items the runner holds at once, each in a worktree of its own,
/// set on this machine as [`set_mode`] is.
pub fn set_parallel(project: &Path, n: usize) -> io::Result<()> {
    change_local(project, |e| tasks::with_parallel(e, n))
}

/// Turns Warriv on or off for the project on this machine, as
/// [`set_mode`]: on, it starts agent sessions by itself.
pub fn set_orchestrator(project: &Path, on: bool) -> io::Result<()> {
    change_local(project, |e| tasks::with_orchestrator(e, on))
}

/// Reads the list, lets `change` rewrite it and writes the result back.
/// Read and written in one go, so an edit made in an editor a moment
/// before is not lost, and under the list's lock, so the app and every
/// session's `horadric quest` take turns. False when `change` found
/// nothing to change.
pub fn update(project: &Path, change: impl FnOnce(&str) -> Option<String>) -> io::Result<bool> {
    let path = file(project);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let _lock = Lock::take(&path.with_extension("lock"))?;
    let old = read_text(&path);
    match change(&old) {
        Some(new) if new != old => write(&path, &new).map(|_| true),
        Some(_) => Ok(true),
        None => Ok(false),
    }
}

/// The done file's path in a project: where the list's old done quests
/// went.
pub fn done_file(project: &Path) -> PathBuf {
    project.join(tasks::done_file(rel(project)))
}

/// The quests the done file holds, oldest first, none when there is none.
pub fn read_done(project: &Path) -> Vec<Task> {
    tasks::parse(&read_text(&done_file(project)))
}

/// Moves the list's done quests that `old` says ended long enough ago to
/// the done file, under the list's lock. The done file is written first,
/// so a failure loses no quest; at worst one shows twice. How many moved.
pub fn sweep_done(project: &Path, old: impl Fn(&Task) -> bool) -> io::Result<usize> {
    let done = done_file(project);
    let mut moved = 0;
    let mut failed = None;
    update(project, |text| {
        let (rest, gone) = tasks::sweep(text, old)?;
        let grown = tasks::append_done(&read_text(&done), &gone, rel(project));
        if let Err(e) = write(&done, &grown) {
            failed = Some(e);
            return None;
        }
        moved = tasks::parse(&gone).len();
        Some(rest)
    })?;
    failed.map_or(Ok(moved), Err)
}

/// Writes through a file beside it and a rename, so a reader never sees
/// half a list.
fn write(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("horadric-tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

/// A file's text, empty when there is none. Without a UTF-8 byte order
/// mark: PowerShell 5 writes one, and left in it hides the first item of a
/// list and makes a config unreadable JSON.
fn read_text(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_string(),
        None => text,
    }
}

/// The project folder above `from`, or `from` itself, whose list has an
/// item held by `holder`. An agent's shell can be anywhere in its project.
pub fn find_held(from: &Path, holder: &str) -> Option<PathBuf> {
    from.ancestors()
        .find(|dir| {
            tasks::parse(&read(dir))
                .iter()
                .any(|t| t.holder.as_deref() == Some(holder))
        })
        .map(Path::to_path_buf)
}

/// The project folder above `from`, or `from` itself, that has a list.
pub fn find_list(from: &Path) -> Option<PathBuf> {
    from.ancestors()
        .find(|dir| file(dir).is_file())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("horadric-tasks-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_update_writes_only_when_something_changed() {
        let dir = scratch("update");
        assert!(update(&dir, |t| Some(tasks::append(t, "First"))).unwrap());
        assert_eq!(read(&dir), "- [ ] First\n");
        assert!(!update(&dir, |_| None).unwrap());
        assert!(!dir.join(".horadric/quests.horadric-tmp").exists());
        assert!(dir.join(QUESTS_FILE).is_file());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn old_done_quests_move_to_the_done_file() {
        let dir = scratch("sweep");
        write(
            &dir.join(OLD_FILE),
            "- [x] Old @a
- [x] New @b
- [ ] Open
",
        )
        .unwrap();
        let old = |t: &Task| t.holder.as_deref() == Some("a");
        assert_eq!(sweep_done(&dir, old).unwrap(), 1);
        assert_eq!(
            read(&dir),
            "- [x] New @b
- [ ] Open
"
        );
        assert_eq!(done_file(&dir), dir.join(".horadric/tasks-done.md"));
        let done = read_done(&dir);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].title, "Old");
        assert_eq!(sweep_done(&dir, old).unwrap(), 0);
        assert!(!dir.join(".horadric/tasks.lock").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_list_from_before_the_rename_keeps_its_file() {
        let dir = scratch("old");
        write(
            &dir.join(OLD_FILE),
            "- [ ] First
",
        )
        .unwrap();
        assert!(update(&dir, |t| Some(tasks::append(t, "Second"))).unwrap());
        assert_eq!(
            read(&dir),
            "- [ ] First
- [ ] Second
"
        );
        assert!(!dir.join(QUESTS_FILE).exists());
        assert_eq!(find_list(&dir.join("src")), Some(dir.clone()));
        write(
            &dir.join(QUESTS_FILE),
            "- [ ] Quest
",
        )
        .unwrap();
        assert_eq!(
            read(&dir),
            "- [ ] Quest
"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parallel_is_set_here_and_only_suggested_by_the_config() {
        let dir = scratch("parallel");
        assert_eq!(parallel(&dir), 1);
        write(&config_file(&dir), "{\"tasks\":{\"parallel\":3}}").unwrap();
        assert_eq!(suggested_parallel(&dir), 3);
        assert_eq!(parallel(&dir), 1);
        set_parallel(&dir, 3).unwrap();
        assert_eq!(parallel(&dir), 3);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ssh_config_hosts_follow_includes_in_place() {
        let home = scratch("sshconf");
        assert!(config_hosts_in(&home).is_empty());
        let ssh = home.join(".ssh");
        fs::create_dir_all(ssh.join("config.d")).unwrap();
        fs::write(
            ssh.join("config"),
            "Host first
Include config.d/*.conf ~/.ssh/loop
Host last *
",
        )
        .unwrap();
        fs::write(
            ssh.join("config.d").join("b.conf"),
            "Host b
",
        )
        .unwrap();
        fs::write(
            ssh.join("config.d").join("a.conf"),
            "Host a first
",
        )
        .unwrap();
        fs::write(
            ssh.join("config.d").join("a.bak"),
            "Host not-me
",
        )
        .unwrap();
        fs::write(
            ssh.join("loop"),
            "Host looped
Include loop
",
        )
        .unwrap();
        assert_eq!(
            config_hosts_in(&home),
            vec!["first", "a", "b", "looped", "last"]
        );
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn a_host_is_added_to_the_config_once() {
        let dir = scratch("host");
        set_mode(&dir, Mode::Auto).unwrap();
        assert!(add_host(&dir, "myvps").unwrap());
        assert!(!add_host(&dir, "myvps").unwrap());
        assert_eq!(hosts(&dir), vec!["myvps"]);
        assert_eq!(mode(&dir), Mode::Auto);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_fleet_is_read_from_the_inventory_the_config_names() {
        let dir = scratch("fleet");
        assert_eq!(fleet(&dir), None);
        write(&config_file(&dir), "{\"inventory\":\"ops/devices.ini\"}").unwrap();
        let (path, devices) = fleet(&dir).unwrap();
        assert_eq!(path, dir.join("ops/devices.ini"));
        assert!(devices.is_empty());
        write(
            &path,
            "[gw]
gw-1 ansible_host=10.0.0.1
",
        )
        .unwrap();
        let (_, devices) = fleet(&dir).unwrap();
        assert_eq!(devices[0].destination, "10.0.0.1");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_mode_is_kept_on_this_machine_not_in_the_config() {
        let dir = scratch("mode");
        assert_eq!(mode(&dir), Mode::Manual);
        set_mode(&dir, Mode::Auto).unwrap();
        assert_eq!(mode(&dir), Mode::Auto);
        assert!(!read_text(&config_file(&dir)).contains("auto"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn warriv_is_turned_on_here_and_only_suggested_by_the_config() {
        let dir = scratch("orchestrator");
        write(&config_file(&dir), "{\"orchestrator\":true}").unwrap();
        assert!(suggested_orchestrator(&dir));
        assert!(!orchestrator(&dir));
        set_orchestrator(&dir, true).unwrap();
        assert!(orchestrator(&dir));
        set_orchestrator(&dir, false).unwrap();
        assert!(!orchestrator(&dir));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_repository_suggests_but_does_not_set_or_run() {
        let dir = scratch("suggest");
        write(
            &config_file(&dir),
            "{\"tasks\":{\"mode\":\"auto\",\"parallel\":16},\"checks\":[\"evil\"]}",
        )
        .unwrap();
        assert_eq!(suggested_mode(&dir), Mode::Auto);
        assert_eq!(suggested_parallel(&dir), 16);
        assert_eq!(mode(&dir), Mode::Manual);
        assert_eq!(parallel(&dir), 1);
        assert_eq!(checks(&dir), Err(vec!["evil".to_string()]));
        approve(&dir, "evil").unwrap();
        assert_eq!(checks(&dir), Ok(vec!["evil".to_string()]));
        assert!(!approved(&dir, "evil --more"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_byte_order_mark_hides_neither_the_first_item_nor_the_mode() {
        let dir = scratch("bom");
        write(&file(&dir), "\u{feff}- [ ] First\n- [ ] Second\n").unwrap();
        write(
            &config_file(&dir),
            "\u{feff}{\"tasks\":{\"mode\":\"auto\"}}",
        )
        .unwrap();
        assert_eq!(tasks::parse(&read(&dir))[0].title, "First");
        assert_eq!(suggested_mode(&dir), Mode::Auto);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_list_is_found_from_a_folder_inside_the_project() {
        let dir = scratch("find");
        let deep = dir.join("src").join("ui");
        fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_list(&deep).filter(|d| d.starts_with(&dir)), None);
        update(&dir, |_| Some("- [/] Fix it @fix-1\n".into())).unwrap();
        assert_eq!(find_list(&deep), Some(dir.clone()));
        assert_eq!(find_held(&deep, "fix-1"), Some(dir.clone()));
        assert_eq!(find_held(&deep, "other-2"), None);
        fs::remove_dir_all(&dir).unwrap();
    }
}
