//! Reading and writing a project's task list and its mode on disk, for the
//! app and for `horadric task`. What the file means is
//! `horadric_core::tasks`; this is only where it lives and how a change
//! lands in it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use horadric_core::ssh;
use horadric_core::tasks::{self, Mode, CONFIG_FILE, TASKS_FILE};

/// The list's path in a project.
pub fn file(project: &Path) -> PathBuf {
    project.join(TASKS_FILE)
}

/// The config's path in a project.
pub fn config_file(project: &Path) -> PathBuf {
    project.join(CONFIG_FILE)
}

/// The list's text, empty when there is none yet.
pub fn read(project: &Path) -> String {
    fs::read_to_string(file(project)).unwrap_or_default()
}

/// The project's mode. No config, or one that says nothing, is manual.
pub fn mode(project: &Path) -> Mode {
    tasks::mode(&fs::read_to_string(config_file(project)).unwrap_or_default())
}

/// The project's SSH hosts, none without a config.
pub fn hosts(project: &Path) -> Vec<String> {
    ssh::hosts(&fs::read_to_string(config_file(project)).unwrap_or_default())
}

pub fn set_mode(project: &Path, mode: Mode) -> io::Result<()> {
    let path = config_file(project);
    let old = fs::read_to_string(&path).unwrap_or_default();
    write(&path, &tasks::with_mode(&old, mode))
}

/// Reads the list, lets `change` rewrite it and writes the result back.
/// Read and written in one go, so an edit made in an editor a moment
/// before is not lost. False when `change` found nothing to change.
pub fn update(project: &Path, change: impl FnOnce(&str) -> Option<String>) -> io::Result<bool> {
    let path = file(project);
    let old = fs::read_to_string(&path).unwrap_or_default();
    match change(&old) {
        Some(new) if new != old => write(&path, &new).map(|_| true),
        Some(_) => Ok(true),
        None => Ok(false),
    }
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
        assert!(!dir.join(".horadric/tasks.horadric-tmp").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_mode_is_kept_in_the_config() {
        let dir = scratch("mode");
        assert_eq!(mode(&dir), Mode::Manual);
        set_mode(&dir, Mode::Auto).unwrap();
        assert_eq!(mode(&dir), Mode::Auto);
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
